//! Async Fetch transport; authorization is scoped by a caller-controlled stateless signer.
use super::*;
use crate::backend::Backend;
use js_sys::{Reflect, Uint8Array};
use lightcraft_engine::cloud_status::{CloudFailure, FailureKind};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use wasm_bindgen::{JsCast, JsValue, closure::Closure};
use wasm_bindgen_futures::JsFuture;
use web_sys::{Request, RequestInit, Response};
mod b2;

fn js_error(_: JsValue) -> String {
    "browser network/storage operation failed; reconnect or reauthorize and retry".into()
}
fn network_error(cancel: &AtomicBool) -> CloudFailure {
    if cancel.load(Ordering::Relaxed) {
        CloudFailure::new(FailureKind::Cancelled, "cloud transfer cancelled; pending work retained")
    } else {
        CloudFailure::connectivity("browser Fetch failed or timed out; check connectivity, endpoint TLS and CORS, then retry (pending work retained)")
    }
}
async fn body(response: &Response, limit: usize, cancel: &AtomicBool) -> Result<Vec<u8>, CloudFailure> {
    if response.headers().get("Content-Length").map_err(js_error)?.and_then(|v| v.parse::<usize>().ok()).is_some_and(|n| n > limit) {
        return Err("remote response exceeds the supported length".into());
    }
    let Some(stream) = response.body() else { return Ok(Vec::new()) };
    let reader: web_sys::ReadableStreamDefaultReader = stream.get_reader().dyn_into().map_err(|e| js_error(e.into()))?;
    let mut bytes = Vec::new();
    loop {
        let chunk = JsFuture::from(reader.read()).await.map_err(|_| network_error(cancel))?;
        if Reflect::get(&chunk, &"done".into()).map_err(js_error)?.as_bool() == Some(true) {
            break;
        }
        let value = Reflect::get(&chunk, &"value".into()).map_err(js_error)?;
        let data = Uint8Array::new(&value);
        if bytes.len().saturating_add(data.length() as usize) > limit {
            let _ = reader.cancel();
            return Err("remote response exceeds the supported length".into());
        }
        bytes.extend_from_slice(&data.to_vec());
    }
    Ok(bytes)
}
// Keep the abort timer alive through streaming the body, not just until response headers.
struct TransferDeadline {
    window: web_sys::Window,
    timer: i32,
    _callback: Closure<dyn FnMut()>,
    controller: web_sys::AbortController,
}
impl TransferDeadline {
    fn new(options: &RequestInit, cancel: Arc<AtomicBool>) -> Result<Self, CloudFailure> {
        let window = web_sys::window().ok_or("browser window unavailable")?;
        let controller = web_sys::AbortController::new().map_err(js_error)?;
        options.set_signal(Some(&controller.signal()));
        let deadline = web_time::Instant::now() + std::time::Duration::from_secs(120);
        let abort = controller.clone();
        let callback = Closure::<dyn FnMut()>::new(move || {
            if cancel.load(Ordering::Relaxed) || web_time::Instant::now() >= deadline {
                abort.abort();
            }
        });
        let timer = window.set_interval_with_callback_and_timeout_and_arguments_0(callback.as_ref().unchecked_ref(), 200).map_err(js_error)?;
        Ok(Self { window, timer, _callback: callback, controller })
    }
}
impl Drop for TransferDeadline {
    fn drop(&mut self) {
        self.window.clear_interval_with_handle(self.timer);
        self.controller.abort();
    }
}
async fn fetch(request: &Request, cancel: &AtomicBool) -> Result<Response, CloudFailure> {
    let window = web_sys::window().ok_or("browser window unavailable")?;
    JsFuture::from(window.fetch_with_request(request))
        .await
        .map_err(|_| network_error(cancel))?
        .dyn_into()
        .map_err(|e| CloudFailure::from(js_error(e)))
}
fn url(url: &str) -> Result<web_sys::Url, CloudFailure> {
    let parsed = web_sys::Url::new(url).map_err(js_error)?;
    if !parsed.username().is_empty()
        || !parsed.password().is_empty()
        || !parsed.hash().is_empty()
        || !(parsed.protocol() == "https:" || (parsed.protocol() == "http:" && matches!(parsed.hostname().as_str(), "localhost" | "127.0.0.1")))
    {
        return Err("authorized URLs require HTTPS and cannot contain user credentials or fragments".into());
    }
    Ok(parsed)
}
#[derive(Clone)]
pub struct Provider {
    pub connection: Connection,
    // An ephemeral login token, deliberately excluded from Serialize and browser storage.
    token: Rc<RefCell<Option<String>>>,
    cancel: Arc<AtomicBool>,
    pub(super) metadata: Option<Rc<super::metadata::Cache>>,
    pub(super) verification_budget: Option<Arc<lightcraft_engine::cloud_verification::Budget>>,
    pub(super) fresh_history: bool,
    pub(super) skip_discovery: bool,
}
pub struct Reply {
    pub status: u16,
    pub bytes: Vec<u8>,
    pub etag: Option<String>,
}
impl Provider {
    pub fn new(connection: Connection, token: Option<String>) -> Result<Self, CloudFailure> {
        connection.validate()?;
        url(&connection.authorize_endpoint)?;
        let origin = url(&connection.storage_origin)?;
        if origin.origin() != connection.storage_origin {
            return Err("storageOrigin must be an origin without a path".into());
        }
        if let Some(root) = &connection.bucket_root {
            let parsed = url(root)?;
            if root.len() > 4096
                || parsed.origin() != connection.storage_origin
                || !parsed.search().is_empty()
                || !parsed.pathname().ends_with('/')
                || parsed.href() != *root
            {
                return Err("bucketRoot must be a canonical bucket URL ending in / on storageOrigin".into());
            }
        }
        if token.as_ref().is_some_and(|v| v.len() > 16384 || v.chars().any(char::is_control)) {
            return Err("invalid reauthorization token".into());
        }
        Ok(Self {
            connection,
            token: Rc::new(RefCell::new(token)),
            cancel: Arc::new(AtomicBool::new(false)),
            metadata: None,
            verification_budget: None,
            fresh_history: false,
            skip_discovery: true,
        })
    }
    pub fn with_cancel(&self, cancel: Arc<AtomicBool>) -> Self {
        Self { cancel, ..self.clone() }
    }
    pub(super) fn with_metadata(&self, backend: Backend) -> Self {
        Self { metadata: Some(Rc::new(super::metadata::Cache::new(backend, &self.connection))), ..self.clone() }
    }
    pub(super) fn verification_provider(&self) -> Self {
        Self { fresh_history: true, verification_budget: Some(Arc::new(lightcraft_engine::cloud_verification::Budget::default())), ..self.clone() }
    }
    pub(super) fn fresh_provider(&self) -> Self {
        Self { fresh_history: true, ..self.clone() }
    }
    pub(super) fn with_discovery(&self, enabled: bool) -> Self {
        Self { skip_discovery: !enabled, ..self.clone() }
    }
    async fn authorize(
        &self,
        method: &str,
        key: &str,
        version: Option<&str>,
        condition: Option<&str>,
        data: Option<&[u8]>,
        list: Option<&lightcraft_engine::cloud_append::listing::ListRequest>,
        immutable: bool,
    ) -> Result<Authorized, CloudFailure> {
        if let Some(list) = list {
            list.validate(&self.connection.prefix).map_err(CloudFailure::from)?;
        } else {
            self.connection.validate_key(key)?;
        }
        let mut params = serde_json::json!({"connection":self.connection.id,"method":method,"key":key,"version":version,
            "condition":condition,"size":data.map(|d|d.len()),"contentSha256":data.map(|d|lightcraft_engine::assets::sha256(d).0)});
        if let Some(list) = list {
            params["operation"] = serde_json::json!("list");
            params["list"] = serde_json::json!(list);
        } else if immutable {
            params["operation"] = serde_json::json!("putImmutable");
        }
        let options = RequestInit::new();
        options.set_method("POST");
        options.set_credentials(web_sys::RequestCredentials::Omit);
        options.set_cache(web_sys::RequestCache::NoStore);
        options.set_redirect(web_sys::RequestRedirect::Error);
        options.set_body(&JsValue::from_str(&params.to_string()));
        let _deadline = TransferDeadline::new(&options, self.cancel.clone())?;
        let request = Request::new_with_str_and_init(&self.connection.authorize_endpoint, &options).map_err(js_error)?;
        request.headers().set("Content-Type", "application/json").map_err(js_error)?;
        if let Some(token) = self.token.borrow().as_ref() {
            request.headers().set("Authorization", &format!("Bearer {token}")).map_err(js_error)?;
        }
        let response = fetch(&request, &self.cancel).await?;
        if !response.ok() {
            return Err(CloudFailure::http("cloud authorization", response.status()));
        }
        let authorized: Authorized =
            serde_json::from_slice(&body(&response, 16 << 10, &self.cancel).await?).map_err(|_| "invalid authorization response")?;
        authorized.validate_headers()?;
        let signed = url(&authorized.url)?;
        if signed.origin() != self.connection.storage_origin {
            return Err("signer returned another storage origin".into());
        }
        let path = js_sys::decode_uri_component(&signed.pathname()).map_err(js_error)?;
        if let Some(list) = list {
            self.validate_list_url(&signed, list)?;
        } else if let Some(root) = &self.connection.bucket_root {
            let root = url(root)?;
            let root_path = String::from(js_sys::decode_uri_component(&root.pathname()).map_err(js_error)?);
            if path != format!("{root_path}{key}") {
                return Err("signer returned another bucket or object key".into());
            }
        } else if !String::from(path).ends_with(&format!("/{key}")) {
            return Err("signer returned another object key".into());
        }
        let now = js_sys::Date::now().max(0.) as u64;
        if authorized.expires_at <= now || authorized.expires_at > now.saturating_add(24 * 60 * 60 * 1000) {
            return Err("signer returned an expired or excessive URL lifetime; reauthorize".into());
        }
        if (immutable || list.is_some()) && authorized.headers.keys().any(|k| matches!(k.to_ascii_lowercase().as_str(), "if-match" | "if-none-match"))
        {
            return Err("append requests must not carry write preconditions".into());
        }
        if let Some(condition) = condition {
            let name = if condition == "*" { "if-none-match" } else { "if-match" };
            if authorized.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str()) != Some(condition) {
                return Err("signer did not authorize the required conditional write; pending work retained".into());
            }
        }
        if list.is_none()
            && (signed.search_params().get("versionId").as_deref() != version || signed.search_params().get_all("versionId").length() > 1)
        {
            return Err("signer changed the requested object version".into());
        }
        if method == "PUT" {
            let data = data.ok_or("write has no bytes")?;
            let digest = lightcraft_engine::assets::sha256(data).0;
            let mut latin = String::new();
            for pair in digest.as_bytes().chunks_exact(2) {
                let part = std::str::from_utf8(pair).map_err(|_| "invalid digest")?;
                latin.push(char::from(u8::from_str_radix(part, 16).map_err(|_| "invalid digest")?));
            }
            let checksum = web_sys::window().ok_or("browser window unavailable")?.btoa(&latin).map_err(js_error)?;
            authorized.validate_body_binding(&digest, &checksum)?;
        }
        Ok(authorized)
    }
    pub async fn request(
        &self,
        method: &str,
        key: &str,
        version: Option<&str>,
        condition: Option<&str>,
        data: Option<&[u8]>,
        limit: usize,
    ) -> Result<Reply, CloudFailure> {
        if !matches!(method, "GET" | "PUT" | "HEAD") || (method == "PUT" && condition.is_none()) {
            return Err("invalid or unconditional browser cloud request".into());
        }
        self.request_typed(method, key, version, condition, data, limit, None, false).await
    }
    async fn request_typed(
        &self,
        method: &str,
        key: &str,
        version: Option<&str>,
        condition: Option<&str>,
        data: Option<&[u8]>,
        limit: usize,
        list: Option<&lightcraft_engine::cloud_append::listing::ListRequest>,
        immutable: bool,
    ) -> Result<Reply, CloudFailure> {
        for attempt in 0..2 {
            if self.cancel.load(Ordering::Relaxed) {
                return Err(CloudFailure::new(FailureKind::Cancelled, "cloud transfer cancelled; pending work retained"));
            }
            if let Some(budget) = &self.verification_budget {
                budget.admit(16 << 10).map_err(CloudFailure::from)?;
                budget.admit(limit.max(16 << 10)).map_err(CloudFailure::from)?;
            }
            let authorized = self.authorize(method, key, version, condition, data, list, immutable).await?;
            let options = RequestInit::new();
            options.set_method(method);
            options.set_credentials(web_sys::RequestCredentials::Omit);
            options.set_cache(web_sys::RequestCache::NoStore);
            options.set_redirect(web_sys::RequestRedirect::Error);
            if let Some(data) = data {
                options.set_body(&Uint8Array::from(data));
            }
            let _deadline = TransferDeadline::new(&options, self.cancel.clone())?;
            let request = Request::new_with_str_and_init(&authorized.url, &options).map_err(js_error)?;
            for (name, value) in authorized.headers {
                request.headers().set(&name, &value).map_err(js_error)?;
            }
            let response = fetch(&request, &self.cancel).await?;
            if response.status() == 403 {
                use lightcraft_engine::cloud_status::provider::{MAX_ERROR_BODY, b2_download_cap};
                match body(&response, MAX_ERROR_BODY, &self.cancel).await {
                    Ok(bytes) if b2_download_cap(403, &bytes) => return Err(CloudFailure::provider_limit(403)),
                    Err(error) if self.cancel.load(Ordering::Relaxed) => return Err(error),
                    _ => {}
                }
            }
            // A transfer can outlive its URL. Re-authorize once, preserving the exact body,
            // key and precondition. A second refusal is an actionable login failure.
            if matches!(response.status(), 401 | 403) && attempt == 0 {
                continue;
            }
            let status = response.status();
            if matches!(status, 401 | 403) {
                return Err(CloudFailure::http("cloud object authorization", status));
            }
            if CloudFailure::http("cloud request", status).retryable() && !matches!(status, 409 | 412) {
                return Err(CloudFailure::http("cloud request", status));
            }
            let etag = response.headers().get("ETag").map_err(js_error)?;
            let bytes = if matches!(status, 200 | 201 | 204) { body(&response, limit, &self.cancel).await? } else { Vec::new() };
            if let Some(budget) = &self.verification_budget {
                budget.read(limit.max(16 << 10), bytes.len());
            }
            return Ok(Reply { status, bytes, etag });
        }
        Err("cloud URL refresh exhausted; pending work retained".into())
    }
    fn validate_list_url(&self, signed: &web_sys::Url, list: &lightcraft_engine::cloud_append::listing::ListRequest) -> Result<(), CloudFailure> {
        let root = url(self.connection.bucket_root.as_ref().ok_or("append mode requires bucketRoot")?)?;
        if root.origin() != self.connection.storage_origin || !root.search().is_empty() || root.pathname() != signed.pathname() {
            return Err("signer returned another bucket root".into());
        }
        let params = signed.search_params();
        for (name, expected) in [
            ("list-type", Some("2")),
            ("max-keys", Some("1000")),
            ("prefix", Some(list.prefix.as_str())),
            ("delimiter", list.delimiter.then_some("/")),
            ("continuation-token", list.continuation.as_deref()),
            ("start-after", list.start_after.as_deref()),
        ] {
            if params.get(name).as_deref() != expected || params.get_all(name).length() > 1 {
                return Err("signer changed a scoped listing parameter".into());
            }
        }
        let allowed = [
            "list-type",
            "max-keys",
            "prefix",
            "delimiter",
            "continuation-token",
            "start-after",
            "X-Amz-Algorithm",
            "X-Amz-Credential",
            "X-Amz-Date",
            "X-Amz-Expires",
            "X-Amz-SignedHeaders",
            "X-Amz-Signature",
            "X-Amz-Security-Token",
        ];
        for pair in js_sys::try_iter(&params).map_err(js_error)?.ok_or("invalid listing query")? {
            let pair = js_sys::Array::from(&pair.map_err(js_error)?);
            if pair.get(0).as_string().is_none_or(|s| !allowed.contains(&s.as_str())) {
                return Err("signer added an unsupported listing parameter".into());
            }
        }
        Ok(())
    }
    pub async fn list(
        &self,
        list: &lightcraft_engine::cloud_append::listing::ListRequest,
    ) -> Result<lightcraft_engine::cloud_append::listing::Page, CloudFailure> {
        let reply = self.request_typed("GET", "", None, None, None, 4 << 20, Some(list), false).await?;
        if reply.status != 200 {
            return Err(CloudFailure::http("object listing", reply.status));
        }
        lightcraft_engine::cloud_append::listing::parse(&reply.bytes, list).map_err(CloudFailure::from)
    }
    pub async fn put_immutable(&self, key: &str, bytes: &[u8]) -> Result<(), CloudFailure> {
        lightcraft_engine::cloud_append::immutable_metadata(key, bytes).map_err(CloudFailure::from)?;
        self.put_verified(key, bytes).await
    }
    pub async fn put_catalog(&self, key: &str, bytes: &[u8]) -> Result<(), CloudFailure> {
        if !self.connection.supports_b2_authority() {
            return Err(CloudFailure::new(FailureKind::Unsupported, "this destination does not support B2 catalog publication"));
        }
        lightcraft_engine::cloud_records::b2_catalog::validate(&self.connection.prefix, key, bytes)?;
        self.put_verified(key, bytes).await
    }
    async fn put_verified(&self, key: &str, bytes: &[u8]) -> Result<(), CloudFailure> {
        let existing = self.request("GET", key, None, None, None, 64 << 20).await?;
        if existing.status == 200 {
            return if existing.bytes == bytes { Ok(()) } else { Err(CloudFailure::integrity("immutable object identity collision")) };
        }
        if existing.status != 404 {
            return Err(CloudFailure::http("immutable lookup", existing.status));
        }
        for attempt in 0..2 {
            let result = self.request_typed("PUT", key, None, None, Some(bytes), 2 << 20, None, true).await;
            if let Err(error) = &result
                && !error.retryable()
            {
                return Err(error.clone());
            }
            let verified = self.request("GET", key, None, None, None, 64 << 20).await?;
            if verified.status == 200 {
                return if verified.bytes == bytes {
                    Ok(())
                } else {
                    Err(CloudFailure::integrity("immutable readback failed SHA-256/length verification"))
                };
            }
            if attempt == 0
                && verified.status == 404
                && (result.as_ref().is_err_and(CloudFailure::retryable) || result.as_ref().is_ok_and(|r| matches!(r.status, 200 | 201 | 204)))
            {
                continue;
            }
            return Err(match result {
                Ok(reply) if matches!(reply.status, 200 | 201 | 204) && verified.status == 404 => {
                    CloudFailure::connectivity("immutable publication is not yet visible; pending work retained")
                }
                Ok(reply) => CloudFailure::http("immutable publication", reply.status),
                Err(error) => error,
            });
        }
        Err(CloudFailure::connectivity("immutable publication retry exhausted"))
    }
    pub async fn blob(&self, backend: &Backend, blob: &Blob) -> Result<Vec<u8>, CloudFailure> {
        blob.validate()?;
        if blob.remote.connection != self.connection.id {
            return Err("blob belongs to another storage connection".into());
        }
        if blob.size > MAX_BROWSER_BLOB {
            return Err("blob exceeds the browser's 512 MiB transfer limit".into());
        }
        if let Ok(Some(bytes)) = super::cache::storage::read_verified(backend, blob).await
            && verify_blob(blob, &bytes).is_ok()
        {
            if let Err(error) = super::cache::storage::touch(backend, blob).await {
                log::warn!("cache index: {error}");
            }
            return Ok(bytes);
        }
        let reply = self.request("GET", &blob.remote.key, blob.remote.version.as_deref(), None, None, blob.size as usize).await?;
        if reply.status != 200 {
            return Err(CloudFailure::http("cloud download", reply.status));
        }
        verify_blob(blob, &reply.bytes).map_err(CloudFailure::integrity)?;
        if let Err(error) = super::cache::storage::store(backend, blob, &reply.bytes).await {
            super::cache::storage::temporary(blob, error);
        }
        Ok(reply.bytes)
    }
}
