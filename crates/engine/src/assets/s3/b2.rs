//! Backblaze native bucket revision CAS. No bearer token leaves this module.
//! This is the provider primitive, not an append maintenance barrier: callers must
//! still validate/fence the logical publication and recover late legacy writes.
//! One conditional-head authority owns each dedicated storage prefix, matching S3.
use super::{Credentials, S3Connection, http_error};
use crate::assets::{AssetError, Result, sha256};
use crate::cloud_publication::RecordRef;
use lightcraft_fetch::http::{self, Limits, Method, RequestBody, Url};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::{
    collections::BTreeMap,
    io::Cursor,
    sync::{Arc, Mutex, OnceLock, atomic::AtomicBool},
    time::{Duration, Instant},
};
const MAX_JSON: usize = 1 << 20;
const MAX_INFO: usize = 10_000;
const MAX_AUTHORITY: u64 = 16 << 10;
const AUTH_URL: &str = "https://api.backblazeb2.com/b2api/v4/b2_authorize_account";
fn unsupported(why: &str) -> AssetError {
    AssetError::Unsupported(format!("Backblaze library control {why}; cloud data and pending changes are retained"))
}
fn configuration(why: &str) -> AssetError {
    AssetError::Unconfigured(format!("Backblaze library control {why}"))
}
fn invalid(why: &str) -> AssetError {
    AssetError::Invalid(format!("invalid Backblaze library control {why}"))
}
fn unconfirmed() -> AssetError {
    AssetError::Network("Backblaze control response was not confirmed; reread before retrying".into())
}
// Neither auth credentials nor tokens implement Debug or Serialize.
struct Auth {
    account: String,
    api: String,
    token: String,
    writable: bool,
}
struct Cached {
    expires: Instant,
    auth: Arc<Auth>,
}
static AUTH_CACHE: OnceLock<Mutex<BTreeMap<String, Cached>>> = OnceLock::new();
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Pointer {
    version: u32,
    binding: String,
    head: RecordRef,
}
/// An observed revision is opaque except for its scoped head and concurrency counter.
/// The full map stays native so compare_exchange preserves unrelated bucket information.
#[derive(Clone)]
pub struct Observed {
    revision: u64,
    head: Option<RecordRef>,
    scope: String,
    entry: String,
    bucket_id: String,
    info: Map<String, Value>,
    auth: Arc<Auth>,
}
#[derive(Clone)]
struct Scope {
    entry: String,
    binding: String,
    root: String,
}
impl Scope {
    fn new(c: &S3Connection) -> Result<Self> {
        c.validate()?;
        let (endpoint, region) = c
            .endpoint
            .as_deref()
            .and_then(crate::cloud_setup::b2_endpoint)
            .ok_or_else(|| configuration("requires a canonical Backblaze endpoint"))?;
        if region != c.region {
            return Err(configuration("configured region differs from its endpoint"));
        }
        if c.prefix.is_empty() {
            return Err(configuration("requires a dedicated nonempty storage folder"));
        }
        let root = crate::assets::keys::in_library(c.prefix.trim_end_matches('/'), "catalog")?;
        let encoded = serde_json::to_vec(&(&c.bucket, region, endpoint, c.prefix.trim_end_matches('/'))).map_err(|_| invalid("scope"))?;
        let binding = sha256(&encoded).0;
        let short = binding.get(..40).ok_or_else(|| invalid("scope digest"))?;
        Ok(Self { entry: format!("lc-ctrl-{short}"), binding, root })
    }
    fn pointer(&self, head: &RecordRef) -> Result<Value> {
        if head.size == 0
            || head.size > MAX_AUTHORITY
            || head.hash.0.len() != 64
            || !head.hash.0.bytes().all(|b| b.is_ascii_digit() || matches!(b, b'a'..=b'f'))
            || head.key != format!("{}/authority/{}.json", self.root, head.hash.0)
        {
            return Err(invalid("authority reference"));
        }
        let pointer = Pointer { version: 1, binding: self.binding.clone(), head: head.clone() };
        Ok(Value::String(serde_json::to_string(&pointer).map_err(|_| invalid("pointer encoding"))?))
    }
    fn decode(&self, value: Option<&Value>) -> Result<Option<RecordRef>> {
        let Some(value) = value else { return Ok(None) };
        let text = value.as_str().ok_or_else(|| unsupported("entry has an unsupported value; update LightCraft"))?;
        if text.len() > 4096 {
            return Err(invalid("pointer size"));
        }
        let value: Value = serde_json::from_str(text).map_err(|_| invalid("pointer encoding"))?;
        if value.get("version").and_then(Value::as_u64) != Some(1) {
            return Err(unsupported("entry needs a newer LightCraft"));
        }
        let pointer: Pointer = serde_json::from_value(value).map_err(|_| unsupported("entry is unreadable; repair it before upgrading"))?;
        if pointer.binding != self.binding {
            return Err(invalid("scope collision; choose another storage folder"));
        }
        if self.pointer(&pointer.head)?.as_str() != Some(text) {
            return Err(invalid("noncanonical pointer"));
        }
        Ok(Some(pointer.head))
    }
}
fn basic(credentials: &Credentials) -> Result<String> {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let bytes = format!("{}:{}", credentials.access, credentials.secret).into_bytes();
    let mut out = String::from("Basic ");
    for part in bytes.chunks(3) {
        let a = u32::from(*part.first().ok_or_else(|| invalid("credentials"))?);
        let b = u32::from(part.get(1).copied().unwrap_or(0));
        let c = u32::from(part.get(2).copied().unwrap_or(0));
        let n = (a << 16) | (b << 8) | c;
        for (index, shift) in [18, 12, 6, 0].into_iter().enumerate() {
            if index > part.len() {
                out.push('=');
            } else {
                let digit = usize::try_from((n >> shift) & 63).map_err(|_| invalid("credentials"))?;
                out.push(char::from(*ALPHABET.get(digit).ok_or_else(|| invalid("credentials"))?));
            }
        }
    }
    Ok(out)
}
fn endpoint(raw: &str) -> Result<String> {
    if raw.contains(['#', '?', '@', '\\']) {
        return Err(invalid("API endpoint"));
    }
    let url = Url::parse(raw).map_err(http_error)?;
    let host = url.host.strip_suffix(".backblazeb2.com").and_then(|host| host.strip_prefix("api"));
    if !url.tls || url.port != 443 || url.path != "/" || host.is_none_or(|suffix| suffix.len() > 16 || !suffix.bytes().all(|b| b.is_ascii_digit())) {
        return Err(invalid("API endpoint"));
    }
    Ok(format!("https://{}", url.host))
}
fn request(url: &str, authorization: &str, body: Option<&Value>, cancel: &AtomicBool) -> Result<Value> {
    let url = Url::parse(url).map_err(http_error)?;
    let encoded = body.map(serde_json::to_vec).transpose().map_err(|_| invalid("request"))?.unwrap_or_default();
    if encoded.len() > MAX_JSON {
        return Err(invalid("request size"));
    }
    let mut bytes = Cursor::new(&encoded);
    let headers = vec![("authorization", authorization.into()), ("content-type", "application/json".into())];
    let limits = Limits { connect: Duration::from_secs(15), stall: Duration::from_secs(30), cancel };
    let mut response = http::request(
        if body.is_some() { Method::Post } else { Method::Get },
        &url,
        &headers,
        body.map(|_| RequestBody { reader: &mut bytes, length: encoded.len() as u64 }),
        &limits,
        &mut |_| {},
    )
    .map_err(http_error)?;
    match response.status {
        200 => {}
        409 | 412 => return Err(AssetError::Conflict),
        301 | 302 | 303 | 307 | 308 => return Err(configuration("API redirected; check the storage region")),
        code => return Err(AssetError::Http(code)),
    }
    if response.content_length().is_some_and(|length| length > MAX_JSON as u64) {
        return Err(unconfirmed());
    }
    let mut result = Vec::new();
    let mut chunk = [0u8; 16 << 10];
    loop {
        let size = response.read(&mut chunk, &limits).map_err(http_error)?;
        if size == 0 {
            break;
        }
        if result.len().saturating_add(size) > MAX_JSON {
            return Err(unconfirmed());
        }
        result.try_reserve(size).map_err(|_| AssetError::Disk("not enough memory for Backblaze control response".into()))?;
        result.extend_from_slice(chunk.get(..size).ok_or(AssetError::Integrity)?);
    }
    serde_json::from_slice(&result).map_err(|_| unconfirmed())
}
fn text(value: &Value, key: &str, max: usize) -> Result<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|v| !v.is_empty() && v.len() <= max && !v.chars().any(char::is_control))
        .map(str::to_owned)
        .ok_or_else(|| invalid("response fields"))
}
fn authorize(c: &S3Connection, cancel: &AtomicBool, refresh: bool) -> Result<Arc<Auth>> {
    c.validate()?;
    if crate::cloud_setup::Provider::of(c) != crate::cloud_setup::Provider::Backblaze {
        return Err(configuration("requires a Backblaze B2 connection"));
    }
    let credentials = c.credentials()?;
    if credentials.token.is_some() {
        return Err(configuration("requires a B2 application key rather than AWS temporary credentials"));
    }
    let fingerprint =
        sha256(&serde_json::to_vec(&(&credentials.access, &credentials.secret, &c.endpoint)).map_err(|_| invalid("credential binding"))?).0;
    let cache = AUTH_CACHE.get_or_init(Default::default);
    if !refresh
        && let Some(auth) = cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&fingerprint)
            .filter(|cached| Instant::now() < cached.expires)
            .map(|cached| cached.auth.clone())
    {
        return Ok(auth);
    }
    let value = request(AUTH_URL, &basic(&credentials)?, None, cancel)?;
    let storage = value.get("apiInfo").and_then(|v| v.get("storageApi")).ok_or_else(|| invalid("account API"))?;
    let api = endpoint(&text(storage, "apiUrl", 512)?)?;
    let s3 = text(storage, "s3ApiUrl", 512)?;
    if crate::cloud_setup::b2_endpoint(&s3) != c.endpoint.as_deref().and_then(crate::cloud_setup::b2_endpoint) {
        return Err(configuration("account and configured S3 region differ"));
    }
    let capabilities = storage
        .get("allowed")
        .and_then(|v| v.get("capabilities"))
        .and_then(Value::as_array)
        .filter(|v| v.len() <= 128)
        .ok_or_else(|| invalid("capabilities"))?;
    if !capabilities.iter().any(|v| v.as_str() == Some("listBuckets")) {
        return Err(configuration("requires listBuckets permission"));
    }
    let auth = Arc::new(Auth {
        account: text(&value, "accountId", 256)?,
        token: text(&value, "authorizationToken", 16 << 10)?,
        api,
        writable: capabilities.iter().any(|v| v.as_str() == Some("writeBuckets")),
    });
    let mut cache = cache.lock().unwrap_or_else(|e| e.into_inner());
    if cache.len() >= 16
        && let Some(key) = cache.keys().next().cloned()
    {
        cache.remove(&key);
    }
    cache.insert(fingerprint, Cached { expires: Instant::now() + Duration::from_secs(300), auth: auth.clone() });
    Ok(auth)
}
fn bucket(c: &S3Connection, auth: &Auth, cancel: &AtomicBool) -> Result<Value> {
    let reply = request(
        &format!("{}/b2api/v4/b2_list_buckets", auth.api),
        &auth.token,
        Some(&json!({"accountId":auth.account,"bucketName":c.bucket,"bucketTypes":["allPrivate","allPublic"]})),
        cancel,
    )?;
    let buckets = reply.get("buckets").and_then(Value::as_array).filter(|v| v.len() == 1).ok_or_else(|| unsupported("bucket is unavailable"))?;
    let bucket = buckets.first().ok_or_else(|| unsupported("bucket is unavailable"))?;
    if bucket.get("bucketName").and_then(Value::as_str) != Some(c.bucket.as_str())
        || bucket.get("accountId").and_then(Value::as_str) != Some(auth.account.as_str())
    {
        return Err(invalid("bucket identity"));
    }
    Ok(bucket.clone())
}
fn info(value: &Value) -> Result<Map<String, Value>> {
    value.get("bucketInfo").and_then(Value::as_object).cloned().filter(|v| v.len() <= 1024).ok_or_else(|| invalid("bucket information"))
}
fn bounded_info(info: &Map<String, Value>) -> Result<()> {
    let bytes = info
        .values()
        .try_fold(0usize, |total, v| serde_json::to_vec(v).map(|v| total.saturating_add(v.len())))
        .map_err(|_| invalid("bucket information"))?;
    if bytes > MAX_INFO {
        return Err(AssetError::Capacity(
            "Backblaze bucket control metadata has insufficient space; use a separate bucket or fewer cloud libraries".into(),
        ));
    }
    Ok(())
}
impl Observed {
    pub fn revision(&self) -> u64 {
        self.revision
    }
    pub fn head(&self) -> Option<&RecordRef> {
        self.head.as_ref()
    }
    /// Refuse unsupported permissions before a caller stages any migration output.
    pub fn require_writable(&self) -> Result<()> {
        if self.auth.writable { Ok(()) } else { Err(configuration("requires writeBuckets permission")) }
    }
}
impl S3Connection {
    /// Stage an exact, verified content-addressed head. This never changes the
    /// publication authority: only `b2_compare_exchange_authority` may do that.
    pub fn b2_stage_head(&self, head: &crate::cloud_records::Head, cancel: &AtomicBool) -> Result<RecordRef> {
        let scope = Scope::new(self)?;
        validate_head(self, head)?;
        let bytes = serde_json::to_vec(head).map_err(|_| invalid("head encoding"))?;
        let hash = sha256(&bytes);
        let reference = RecordRef { key: format!("{}/authority/{}.json", scope.root, hash.0), size: bytes.len() as u64, hash };
        scope.pointer(&reference)?;
        self.put_verified_metadata(&reference.key, &bytes, cancel)?;
        Ok(reference)
    }
    /// Read the scoped pointer and an opaque revision snapshot; no control data is mutated.
    pub fn b2_read_authority(&self, cancel: &AtomicBool) -> Result<Observed> {
        let scope = Scope::new(self)?;
        let mut auth = authorize(self, cancel, false)?;
        let value = match bucket(self, &auth, cancel) {
            Err(AssetError::Http(401)) => {
                auth = authorize(self, cancel, true)?;
                bucket(self, &auth, cancel)?
            }
            value => value?,
        };
        let info = info(&value)?;
        let head = scope.decode(info.get(&scope.entry))?;
        Ok(Observed {
            revision: value.get("revision").and_then(Value::as_u64).ok_or_else(|| invalid("bucket revision"))?,
            head,
            scope: scope.binding,
            entry: scope.entry,
            bucket_id: text(&value, "bucketId", 256)?,
            info,
            auth,
        })
    }
    /// Atomically install an already verified content-addressed authority. A failed
    /// revision check cannot make the object visible; uncertain responses require reread.
    pub fn b2_compare_exchange_authority(&self, observed: &Observed, next: &RecordRef, cancel: &AtomicBool) -> Result<()> {
        let scope = Scope::new(self)?;
        if scope.binding != observed.scope || scope.entry != observed.entry {
            return Err(invalid("observed scope"));
        }
        observed.require_writable()?;
        let mut info = observed.info.clone();
        info.insert(scope.entry.clone(), scope.pointer(next)?);
        bounded_info(&info)?;
        // The provider boundary never publishes an unverified pointer. A content-addressed
        // orphan is harmless; only bucket revision CAS makes it authoritative.
        let reply = self.get_small(&next.key, MAX_AUTHORITY as usize, cancel)?.ok_or(AssetError::Missing)?;
        if reply.bytes.len() as u64 != next.size || sha256(&reply.bytes) != next.hash {
            return Err(AssetError::Integrity);
        }
        let head = crate::cloud_records::Head::decode(&reply.bytes).map_err(AssetError::Blocked)?;
        validate_head(self, &head)?;
        let update = |auth: &Auth| update(observed, &info, auth, cancel);
        match update(&observed.auth) {
            Err(AssetError::Http(401)) => {
                let auth = authorize(self, cancel, true)?;
                if auth.account != observed.auth.account || !auth.writable {
                    return Err(configuration("account or writeBuckets permission changed"));
                }
                update(&auth)
            }
            value => value,
        }
    }
}

fn validate_head(connection: &S3Connection, head: &crate::cloud_records::Head) -> Result<()> {
    let library = head.library.as_deref().filter(|id| lightcraft_catalog::assets::identifier(id)).ok_or_else(|| invalid("head library"))?;
    head.validate_scope_typed(connection.prefix.trim_end_matches('/'), library).map_err(AssetError::Blocked)
}

fn update(observed: &Observed, next: &Map<String, Value>, auth: &Auth, cancel: &AtomicBool) -> Result<()> {
    let body = json!({"accountId":auth.account,"bucketId":observed.bucket_id,"ifRevisionIs":observed.revision,"bucketInfo":next});
    let reply = request(&format!("{}/b2api/v4/b2_update_bucket", auth.api), &auth.token, Some(&body), cancel)?;
    if reply.get("bucketId").and_then(Value::as_str) != Some(observed.bucket_id.as_str())
        || reply.get("accountId").and_then(Value::as_str) != Some(auth.account.as_str())
        || reply.get("revision").and_then(Value::as_u64).is_none_or(|revision| revision <= observed.revision)
        || info(&reply).ok().as_ref() != Some(next)
    {
        return Err(unconfirmed());
    }
    Ok(())
}

#[cfg(test)]
mod tests;
