//! Async Fetch transport; authorization is scoped by a caller-controlled stateless signer.
use super::*;
use crate::backend::Backend;
use js_sys::{Reflect, Uint8Array};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use wasm_bindgen::{JsCast, JsValue, closure::Closure};
use wasm_bindgen_futures::JsFuture;
use web_sys::{Request, RequestInit, Response};

fn js_error(_: JsValue) -> String {
    "browser network/storage operation failed; reconnect or reauthorize and retry".into()
}
async fn body(response: &Response, limit: usize) -> Result<Vec<u8>, String> {
    if response.headers().get("Content-Length").map_err(js_error)?.and_then(|v| v.parse::<usize>().ok()).is_some_and(|n| n > limit) {
        return Err("remote response exceeds the supported length".into());
    }
    let Some(stream) = response.body() else { return Ok(Vec::new()) };
    let reader: web_sys::ReadableStreamDefaultReader = stream.get_reader().dyn_into().map_err(|e| js_error(e.into()))?;
    let mut bytes = Vec::new();
    loop {
        let chunk = JsFuture::from(reader.read()).await.map_err(js_error)?;
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
}
impl TransferDeadline {
    fn new(options: &RequestInit, cancel: Arc<AtomicBool>) -> Result<Self, String> {
        let window = web_sys::window().ok_or("browser window unavailable")?;
        let controller = web_sys::AbortController::new().map_err(js_error)?;
        options.set_signal(Some(&controller.signal()));
        let deadline = js_sys::Date::now() + 120_000.;
        let callback = Closure::<dyn FnMut()>::new(move || {
            if cancel.load(Ordering::Relaxed) || js_sys::Date::now() >= deadline {
                controller.abort();
            }
        });
        let timer = window.set_interval_with_callback_and_timeout_and_arguments_0(callback.as_ref().unchecked_ref(), 200).map_err(js_error)?;
        Ok(Self { window, timer, _callback: callback })
    }
}
impl Drop for TransferDeadline {
    fn drop(&mut self) {
        self.window.clear_interval_with_handle(self.timer);
    }
}
async fn fetch(request: &Request) -> Result<Response, String> {
    let window = web_sys::window().ok_or("browser window unavailable")?;
    JsFuture::from(window.fetch_with_request(request)).await.map_err(js_error)?.dyn_into().map_err(js_error)
}
fn url(url: &str) -> Result<web_sys::Url, String> {
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
}
pub struct Reply {
    pub status: u16,
    pub bytes: Vec<u8>,
    pub etag: Option<String>,
}
impl Provider {
    pub fn new(connection: Connection, token: Option<String>) -> Result<Self, String> {
        connection.validate()?;
        url(&connection.authorize_endpoint)?;
        let origin = url(&connection.storage_origin)?;
        if origin.origin() != connection.storage_origin {
            return Err("storageOrigin must be an origin without a path".into());
        }
        if token.as_ref().is_some_and(|v| v.len() > 16384 || v.chars().any(char::is_control)) {
            return Err("invalid reauthorization token".into());
        }
        Ok(Self { connection, token: Rc::new(RefCell::new(token)), cancel: Arc::new(AtomicBool::new(false)) })
    }
    pub fn with_cancel(&self, cancel: Arc<AtomicBool>) -> Self {
        Self { connection: self.connection.clone(), token: self.token.clone(), cancel }
    }
    async fn authorize(
        &self,
        method: &str,
        key: &str,
        version: Option<&str>,
        condition: Option<&str>,
        data: Option<&[u8]>,
    ) -> Result<Authorized, String> {
        self.connection.validate_key(key)?;
        let params = serde_json::json!({"connection":self.connection.id,"method":method,"key":key,"version":version,
            "condition":condition,"size":data.map(|d|d.len()),"contentSha256":data.map(|d|lightcraft_engine::assets::sha256(d).0)});
        let options = RequestInit::new();
        options.set_method("POST");
        options.set_credentials(web_sys::RequestCredentials::Omit);
        options.set_redirect(web_sys::RequestRedirect::Error);
        options.set_body(&JsValue::from_str(&params.to_string()));
        let _deadline = TransferDeadline::new(&options, self.cancel.clone())?;
        let request = Request::new_with_str_and_init(&self.connection.authorize_endpoint, &options).map_err(js_error)?;
        request.headers().set("Content-Type", "application/json").map_err(js_error)?;
        if let Some(token) = self.token.borrow().as_ref() {
            request.headers().set("Authorization", &format!("Bearer {token}")).map_err(js_error)?;
        }
        let response = fetch(&request).await?;
        if !response.ok() {
            return Err("cloud authorization failed; sign in again and retry (pending work retained)".into());
        }
        let authorized: Authorized = serde_json::from_slice(&body(&response, 16 << 10).await?).map_err(|_| "invalid authorization response")?;
        authorized.validate_headers()?;
        let signed = url(&authorized.url)?;
        if signed.origin() != self.connection.storage_origin {
            return Err("signer returned another storage origin".into());
        }
        let path = js_sys::decode_uri_component(&signed.pathname()).map_err(js_error)?;
        if !String::from(path).ends_with(&format!("/{key}")) {
            return Err("signer returned another object key".into());
        }
        let now = js_sys::Date::now().max(0.) as u64;
        if authorized.expires_at <= now || authorized.expires_at > now.saturating_add(24 * 60 * 60 * 1000) {
            return Err("signer returned an expired or excessive URL lifetime; reauthorize".into());
        }
        if let Some(condition) = condition {
            let name = if condition == "*" { "if-none-match" } else { "if-match" };
            if authorized.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str()) != Some(condition) {
                return Err("signer did not authorize the required conditional write; pending work retained".into());
            }
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
    ) -> Result<Reply, String> {
        if !matches!(method, "GET" | "PUT" | "HEAD") || (method == "PUT" && condition.is_none()) {
            return Err("invalid or unconditional browser cloud request".into());
        }
        for attempt in 0..2 {
            if self.cancel.load(Ordering::Relaxed) {
                return Err("cloud transfer cancelled; pending work retained".into());
            }
            let authorized = self.authorize(method, key, version, condition, data).await?;
            let options = RequestInit::new();
            options.set_method(method);
            options.set_credentials(web_sys::RequestCredentials::Omit);
            options.set_redirect(web_sys::RequestRedirect::Error);
            if let Some(data) = data {
                options.set_body(&Uint8Array::from(data));
            }
            let _deadline = TransferDeadline::new(&options, self.cancel.clone())?;
            let request = Request::new_with_str_and_init(&authorized.url, &options).map_err(js_error)?;
            for (name, value) in authorized.headers {
                request.headers().set(&name, &value).map_err(js_error)?;
            }
            let response = fetch(&request).await?;
            // A transfer can outlive its URL. Re-authorize once, preserving the exact body,
            // key and precondition. A second refusal is an actionable login failure.
            if matches!(response.status(), 401 | 403) && attempt == 0 {
                continue;
            }
            let status = response.status();
            if matches!(status, 401 | 403) {
                return Err("cloud access expired or was revoked; reauthorize and retry".into());
            }
            let etag = response.headers().get("ETag").map_err(js_error)?;
            let bytes = if matches!(status, 200 | 201 | 204) { body(&response, limit).await? } else { Vec::new() };
            return Ok(Reply { status, bytes, etag });
        }
        Err("cloud URL refresh exhausted; pending work retained".into())
    }
    pub async fn blob(&self, backend: &Backend, blob: &Blob) -> Result<Vec<u8>, String> {
        blob.validate()?;
        if blob.remote.connection != self.connection.id {
            return Err("blob belongs to another storage connection".into());
        }
        if blob.size > MAX_BROWSER_BLOB {
            return Err("blob exceeds the browser's 512 MiB transfer limit".into());
        }
        let key = cache_key(blob);
        if let Some(bytes) = backend.read(&key).await?
            && verify_blob(blob, &bytes).is_ok()
        {
            return Ok(bytes);
        }
        let reply = self.request("GET", &blob.remote.key, blob.remote.version.as_deref(), None, None, blob.size as usize).await?;
        if reply.status != 200 {
            return Err(format!("cloud download returned HTTP {}; cached/pending data retained", reply.status));
        }
        verify_blob(blob, &reply.bytes)?;
        backend.write(&key, &reply.bytes).await.map_err(|_| "browser cache save failed (quota or storage unavailable); free space and retry")?;
        Ok(reply.bytes)
    }
}
