//! Bounded, process-local cloud usage diagnostics. No credentials, URLs or raw keys.
use crate::assets::s3::S3Connection;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    cell::RefCell,
    collections::{BTreeMap, VecDeque},
    marker::PhantomData,
    rc::Rc,
    sync::{Mutex, OnceLock},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

mod transport;
pub(crate) use transport::{Response, Target, request};

const MAX_OPERATIONS: usize = 64;
const MAX_REQUESTS: usize = 256;
const MAX_DESTINATIONS: usize = 32;
const MAX_DEPTH: usize = 8;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum Purpose {
    #[default]
    Direct,
    Sync,
    Audit,
    Upload,
    UploadVerification,
    Publication,
    MediaRead,
    Preview,
    Thumbnail,
    OfflineCopy,
    Setup,
    Maintenance,
    Upgrade,
    Signer,
}

#[derive(Clone, Default)]
struct Context {
    background_poll: bool,
    path: Vec<u64>,
    purpose: Purpose,
    library: Option<String>,
    import: Option<String>,
    task: Option<u64>,
}
thread_local! { static CONTEXT: RefCell<Context> = RefCell::new(Context::default()); }

/// Restores a worker's attribution, including during unwinding. It never crosses threads.
pub(crate) struct Scope {
    previous: Context,
    operation: Option<u64>,
    _local: PhantomData<Rc<()>>,
}
impl Scope {
    pub fn background_poll(background_poll: bool) -> Self {
        let mut next = CONTEXT.with(|c| c.borrow().clone());
        next.background_poll = background_poll;
        Self::replace(next, None)
    }
    pub fn operation(purpose: Purpose, library: Option<&str>, import: Option<&str>, task: Option<u64>) -> Self {
        let mut next = CONTEXT.with(|c| c.borrow().clone());
        next.purpose = purpose;
        next.library = library.map(identity).or(next.library);
        next.import = import.map(identity).or(next.import);
        next.task = task.or(next.task);
        let mut registry = registry().lock().unwrap_or_else(|e| e.into_inner());
        let operation = if next.path.len() < MAX_DEPTH { registry.operation(&next) } else { None };
        if let Some(id) = operation {
            next.path.push(id);
        }
        drop(registry);
        Self::replace(next, operation)
    }
    pub fn purpose(purpose: Purpose) -> Self {
        let mut next = CONTEXT.with(|c| c.borrow().clone());
        next.purpose = purpose;
        Self::replace(next, None)
    }
    pub fn default_purpose(purpose: Purpose) -> Self {
        let next = CONTEXT.with(|c| c.borrow().clone());
        if next.purpose == Purpose::Direct { Self::purpose(purpose) } else { Self::replace(next, None) }
    }
    fn replace(next: Context, operation: Option<u64>) -> Self {
        let previous = CONTEXT.with(|c| c.replace(next));
        Self { previous, operation, _local: PhantomData }
    }
}
impl Drop for Scope {
    fn drop(&mut self) {
        CONTEXT.with(|c| {
            c.replace(self.previous.clone());
        });
        if let Some(id) = self.operation {
            let mut registry = registry().lock().unwrap_or_else(|e| e.into_inner());
            let elapsed = registry.elapsed();
            if let Some(operation) = registry.operations.iter_mut().find(|o| o.id == id) {
                operation.finished_ms = Some(elapsed);
            }
        }
    }
}

/// Explicit propagation into scoped helper threads; thread locals are not inherited.
#[derive(Clone)]
pub(crate) struct Attribution(Context);
impl Attribution {
    pub fn capture() -> Self {
        Self(CONTEXT.with(|c| c.borrow().clone()))
    }
    pub fn enter(&self) -> Scope {
        Scope::replace(self.0.clone(), None)
    }
}

#[derive(Clone, Default, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Totals {
    pub finished_attempts: u64,
    pub background_poll_attempts: u64,
    pub background_poll_body_bytes_read: u64,
    pub attempts: u64,
    pub responses: u64,
    pub body_bytes_sent: u64,
    pub body_bytes_read: u64,
    pub complete_bodies: u64,
    pub transport_failures: u64,
    pub http_errors: u64,
    pub provider_caps: u64,
    pub elapsed_ms: u64,
    pub local_cache_hits: u64,
    pub shared_proof_hits: u64,
}
impl Totals {
    fn add(&mut self, other: &Self) {
        macro_rules! add { ($($name:ident),*) => { $(self.$name = self.$name.saturating_add(other.$name);)* }; }
        add!(
            finished_attempts,
            background_poll_attempts,
            background_poll_body_bytes_read,
            attempts,
            responses,
            body_bytes_sent,
            body_bytes_read,
            complete_bodies,
            transport_failures,
            http_errors,
            provider_caps,
            elapsed_ms,
            local_cache_hits,
            shared_proof_hits
        );
    }
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct Operation {
    background_poll: bool,
    id: u64,
    parent: Option<u64>,
    purpose: Purpose,
    library: Option<String>,
    import: Option<String>,
    task: Option<u64>,
    started_ms: u64,
    finished_ms: Option<u64>,
    totals: Totals,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct Record {
    background_poll: bool,
    id: u64,
    operation_path: Vec<u64>,
    purpose: Purpose,
    library: Option<String>,
    import: Option<String>,
    task: Option<u64>,
    destination: String,
    resource: String,
    transport: &'static str,
    kind: &'static str,
    method: &'static str,
    /// Ordinal of the HTTP attempt within the innermost operation, not a guessed retry count.
    attempt_in_operation: Option<u64>,
    status: Option<u16>,
    provider_refusal: Option<&'static str>,
    body_bytes_sent: u64,
    body_bytes_read: u64,
    body_complete: bool,
    transport_failure: bool,
    elapsed_ms: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Hour {
    hour_since_start: u64,
    totals: Totals,
}

struct Registry {
    untracked_operations: u64,
    start: Instant,
    started_unix_ms: Option<u64>,
    serial: u64,
    totals: Totals,
    destinations: BTreeMap<String, Totals>,
    operations: VecDeque<Operation>,
    recent: VecDeque<Record>,
    hours: VecDeque<Hour>,
}
impl Default for Registry {
    fn default() -> Self {
        Self {
            untracked_operations: 0,
            start: Instant::now(),
            started_unix_ms: SystemTime::now().duration_since(UNIX_EPOCH).ok().map(milliseconds),
            serial: 0,
            totals: Totals::default(),
            destinations: BTreeMap::new(),
            operations: VecDeque::new(),
            recent: VecDeque::new(),
            hours: VecDeque::new(),
        }
    }
}
impl Registry {
    fn elapsed(&self) -> u64 {
        milliseconds(self.start.elapsed())
    }
    fn serial(&mut self) -> Option<u64> {
        self.serial = self.serial.checked_add(1)?;
        Some(self.serial)
    }
    fn operation(&mut self, context: &Context) -> Option<u64> {
        if self.operations.len() >= MAX_OPERATIONS {
            if let Some(index) = self.operations.iter().position(|operation| operation.finished_ms.is_some()) {
                self.operations.remove(index);
            } else {
                self.untracked_operations = self.untracked_operations.saturating_add(1);
                return None;
            }
        }
        let id = self.serial()?;
        self.operations.push_back(Operation {
            background_poll: context.background_poll,
            id,
            parent: context.path.last().copied(),
            purpose: context.purpose,
            library: context.library.clone(),
            import: context.import.clone(),
            task: context.task,
            started_ms: self.elapsed(),
            finished_ms: None,
            totals: Totals::default(),
        });
        Some(id)
    }
    fn add(&mut self, context: &Context, destination: Option<&str>, hour: u64, totals: &Totals) {
        self.totals.add(totals);
        if let Some(destination) = destination {
            let key = if self.destinations.len() < MAX_DESTINATIONS || self.destinations.contains_key(destination) {
                destination
            } else {
                "otherDestinations"
            };
            self.destinations.entry(key.into()).or_default().add(totals);
        }
        for operation in &mut self.operations {
            if context.path.contains(&operation.id) {
                operation.totals.add(totals);
            }
        }
        // Attribute transfers to their start hour. Old in-flight requests cannot resurrect
        // expired buckets; lifetime totals still include their eventual completion.
        let cutoff = (self.start.elapsed().as_secs() / 3600).saturating_sub(23);
        while self.hours.front().is_some_and(|h| h.hour_since_start < cutoff) {
            self.hours.pop_front();
        }
        if hour >= cutoff && self.hours.back().is_none_or(|h| hour > h.hour_since_start) {
            self.hours.push_back(Hour { hour_since_start: hour, totals: Totals::default() });
            while self.hours.len() > 24 {
                self.hours.pop_front();
            }
        }
        if let Some(bucket) = self.hours.iter_mut().find(|h| h.hour_since_start == hour) {
            bucket.totals.add(totals);
        }
    }
}
fn registry() -> &'static Mutex<Registry> {
    static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();
    REGISTRY.get_or_init(Default::default)
}
fn milliseconds(value: Duration) -> u64 {
    u64::try_from(value.as_millis()).unwrap_or(u64::MAX)
}
fn identity(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}
pub(crate) fn destination(connection: &S3Connection) -> String {
    let mut hash = Sha256::new();
    hash.update([u8::from(connection.endpoint.is_some())]);
    for value in [
        connection.id.as_str(),
        connection.bucket.as_str(),
        connection.region.as_str(),
        connection.prefix.trim_end_matches('/'),
        connection.endpoint.as_deref().unwrap_or_default().trim_end_matches('/'),
    ] {
        hash.update((value.len() as u64).to_le_bytes());
        hash.update(value.as_bytes());
    }
    format!("{:x}", hash.finalize())
}

pub(crate) fn reuse(shared: bool) {
    let context = CONTEXT.with(|c| c.borrow().clone());
    let mut registry = registry().lock().unwrap_or_else(|e| e.into_inner());
    let hour = registry.start.elapsed().as_secs() / 3600;
    registry.add(&context, None, hour, &Totals { local_cache_hits: u64::from(!shared), shared_proof_hits: u64::from(shared), ..Default::default() });
}

pub(crate) fn summary() -> serde_json::Value {
    let registry = registry().lock().unwrap_or_else(|e| e.into_inner());
    serde_json::json!({"scope":"thisProcess","startedUnixMs":registry.started_unix_ms,"activeAttempts":registry.totals.attempts.saturating_sub(registry.totals.finished_attempts),"totals":registry.totals})
}
pub fn status(session: &mut crate::Session, params: &serde_json::Value) -> crate::Result<serde_json::Value> {
    let error = |why: &str| crate::EngineError::BadParams { cmd: "cloud.traffic".into(), msg: why.into() };
    if !params.is_object() {
        return Err(error("expected an object"));
    }
    let limit = match params.get("limit") {
        Some(value) => usize::try_from(value.as_u64().ok_or_else(|| error("limit must be 0..256"))?)
            .ok()
            .filter(|n| *n <= MAX_REQUESTS)
            .ok_or_else(|| error("limit must be 0..256"))?,
        None => 32,
    };
    let operation = params.get("operation").map(|v| v.as_u64().ok_or_else(|| error("operation must be an operation ID"))).transpose()?;
    let connections = session.library.as_mut().map(|library| library.cloud_connections()).transpose()?.unwrap_or_default();
    let bindings = connections
        .iter()
        .take(MAX_DESTINATIONS)
        .map(|connection| serde_json::json!({"connection":connection.id,"destination":destination(connection)}))
        .collect::<Vec<_>>();
    let library = session.catalog.sync_state().map(|state| serde_json::json!({"library":state.library,"trafficIdentity":identity(&state.library)}));
    let registry = registry().lock().unwrap_or_else(|e| e.into_inner());
    let selected =
        operation.map(|id| registry.operations.iter().find(|o| o.id == id).ok_or_else(|| error("operation is no longer retained"))).transpose()?;
    let requests: Vec<_> = registry.recent.iter().rev().filter(|r| operation.is_none_or(|id| r.operation_path.contains(&id))).take(limit).collect();
    let cutoff = (registry.start.elapsed().as_secs() / 3600).saturating_sub(23);
    let hours = registry.hours.iter().filter(|hour| hour.hour_since_start >= cutoff).map(|hour| serde_json::json!({
        "hourSinceStart":hour.hour_since_start,"observedMs":registry.elapsed().saturating_sub(hour.hour_since_start.saturating_mul(3_600_000)).min(3_600_000),"totals":hour.totals
    })).collect::<Vec<_>>();
    Ok(serde_json::json!({"scope":"thisProcess","startedUnixMs":registry.started_unix_ms,"elapsedMs":registry.elapsed(),
        "selectedOperation":operation,"totalsScope":if operation.is_some() {"operation"}else{"thisProcess"},
        "currentLibrary":library,"connectionBindings":bindings,
        "connectionBindingsTruncated":connections.len() > MAX_DESTINATIONS,
        "totals":selected.map_or(&registry.totals,|o| &o.totals),"destinations":registry.destinations,"operations":registry.operations,
        "recentRequests":requests,"hours":hours,"untrackedOperations":registry.untracked_operations,"activeAttempts":registry.totals.attempts.saturating_sub(registry.totals.finished_attempts),"retention":{"operations":MAX_OPERATIONS,"requests":MAX_REQUESTS,"hours":24},
        "byteBasis":"HTTP body bytes successfully sent in completed transport chunks or read by the application; excludes headers, TLS and unread/prefetched bytes. Partial transfers are lower bounds, not provider billing."}))
}

#[cfg(test)]
mod tests;

#[cfg(test)]
pub(crate) fn observed_operation(scope: &Scope) -> Option<serde_json::Value> {
    let id = scope.operation?;
    let registry = registry().lock().unwrap_or_else(|e| e.into_inner());
    let operation = registry.operations.iter().find(|operation| operation.id == id)?;
    let requests = registry.recent.iter().filter(|request| request.operation_path.contains(&id)).collect::<Vec<_>>();
    Some(serde_json::json!({"totals":operation.totals,"requests":requests}))
}
