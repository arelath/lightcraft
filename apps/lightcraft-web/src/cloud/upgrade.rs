//! Browser library upgrades through the conditional head's maintenance state, matching
//! the native runner (`lightcraft_engine::cloud_upgrade`). Every transition is a CAS on
//! the head and is confirmed by rereading exactly the control it wrote.
use super::Provider;
use super::protocol::{Outcome, immutable, read_head};
use lightcraft_engine::assets::sha256;
use lightcraft_engine::catalog::replication::{SyncProtocol, SyncState};
use lightcraft_engine::cloud_control::{Blocked, Boundary, Control, Plan, State, Successor};
use lightcraft_engine::cloud_records::{Batch, Head, RecordRef, object_key};

const ATTEMPTS: usize = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Request {
    /// `writers_stopped` is used only when the head cannot hold the barrier.
    Append {
        writers_stopped: bool,
    },
    Cancel,
}

fn now_ms() -> u64 {
    let now = js_sys::Date::now();
    if now.is_finite() && now > 0. { now as u64 } else { 0 }
}
async fn current(provider: &Provider, library: &str) -> Result<(Head, String, Control), String> {
    let (head, etag) = read_head(provider, library).await?.ok_or("cloud library has no head to upgrade; synchronize first")?;
    let control = head.control()?;
    control.may_read().map_err(|b| b.to_string())?;
    Ok((head, etag, control))
}
enum Failure {
    /// Storage rejected conditional writes (HTTP 501): the head cannot hold a barrier.
    Unsupported,
    Other(String),
}
impl From<String> for Failure {
    fn from(why: String) -> Self {
        Self::Other(why)
    }
}
impl From<&str> for Failure {
    fn from(why: &str) -> Self {
        Self::Other(why.into())
    }
}
impl Failure {
    fn message(self) -> String {
        match self {
            Self::Unsupported => {
                "storage rejected conditional writes (HTTP 501); safe catalog publication requires If-None-Match and If-Match".into()
            }
            Self::Other(why) => why,
        }
    }
}
/// A marker batch plus a conditional head carrying `next`'s control.
async fn transition(provider: &Provider, library: &str, next: impl Fn(&Head, &Control) -> Result<Control, Blocked>) -> Result<Head, Failure> {
    for _ in 0..ATTEMPTS {
        let (head, etag, observed) = current(provider, library).await?;
        let wanted = next(&head, &observed).map_err(|b| b.to_string())?;
        let batch = Batch { version: 3, parent: Some(head.tip.clone()), imports: Vec::new(), mutations: Vec::new(), checkpoint_marker: true };
        let bytes = serde_json::to_vec(&batch).map_err(|e| e.to_string())?;
        let hash = sha256(&bytes);
        let tip = RecordRef { key: object_key(&provider.connection.prefix, wanted.epoch, "changes", &hash.0), size: bytes.len() as u64, hash };
        // Classify a 501 before the shared immutable writer turns it into text.
        let probe = provider.request("PUT", &tip.key, None, Some("*"), Some(&bytes), 16 << 10).await;
        match probe {
            Ok(reply) if reply.status == 501 => return Err(Failure::Unsupported),
            Ok(reply) if matches!(reply.status, 200 | 201 | 204) => {}
            _ => immutable(provider, &tip.key, &bytes).await?,
        }
        let replacement = Head::next(Some(&head), tip, library, head.checkpoint.clone(), wanted.clone())?;
        let body = serde_json::to_vec(&replacement).map_err(|e| e.to_string())?;
        let key = format!("{}/catalog/head.json", provider.connection.prefix);
        match provider.request("PUT", &key, None, Some(&etag), Some(&body), 16 << 10).await {
            Ok(reply) if reply.status == 501 => return Err(Failure::Unsupported),
            Ok(reply) if !matches!(reply.status, 200 | 201 | 204 | 412) => {
                return Err(format!("conditional head write returned HTTP {}; upgrade not started, local work retained", reply.status).into());
            }
            _ => {}
        }
        let (confirmed, _, after) = current(provider, library).await?;
        if after == wanted {
            return Ok(confirmed);
        }
    }
    Err("cloud library head remained busy; upgrade not started or not confirmed".into())
}

/// Seed append-log, then retire the head; or rejoin a head that is already retired.
async fn append(provider: &Provider, local: &SyncState, owner: &str, writers_stopped: bool) -> Result<Outcome, String> {
    let library = &local.library;
    let manifest = sha256(&lightcraft_engine::cloud_append::manifest(library).map_err(|e| e.to_string())?).0;
    let legacy = |why: &str| -> Result<(), String> {
        if writers_stopped { Ok(()) } else { Err(format!("{why}; stop every other device, then retry with writersStopped:true")) }
    };
    let Some((_, _, observed)) = (match read_head(provider, library).await? {
        Some(_) => Some(current(provider, library).await?),
        None => None,
    }) else {
        legacy("cloud library has no published head")?;
        return super::append::migrate(provider, local).await;
    };
    if observed.state != State::Retired {
        let token = super::host::random_identity()?;
        let id = format!("appendLog-{}", token.get(..16).unwrap_or("migration"));
        let acquired = transition(provider, library, |head, c| match &c.migration {
            None => c.begin(&id, Plan::AppendLog, Boundary { tip: head.tip.hash.0.clone(), generation: head.generation }, owner, &token, now_ms()),
            Some(m) if m.plan != Plan::AppendLog => Err(Blocked::Maintenance { migration: m.id.clone(), plan: m.plan, owner: m.owner.clone() }),
            Some(m) => c.takeover(owner, &token, now_ms(), m.owner == owner),
        })
        .await;
        match acquired {
            Ok(_) => {}
            Err(Failure::Unsupported) => {
                legacy(&Failure::Unsupported.message())?;
                return super::append::migrate(provider, local).await;
            }
            Err(failure) => return Err(failure.message()),
        }
        let seeded = super::append::migrate(provider, local).await?;
        let successor = Successor { protocol: "append-log-v1".into(), epoch: 1, manifest: manifest.clone() };
        transition(provider, library, |head, c| {
            if head.control.as_ref().and_then(|c| c.migration.as_ref()).is_none_or(|m| m.token != token) {
                return Err(Blocked::Invalid("upgrade ownership was taken over by another device".into()));
            }
            c.retire(&token, successor.clone())
        })
        .await
        .map_err(Failure::message)?;
        return Ok(seeded);
    }
    match &observed.successor {
        Some(s) if s.protocol == "append-log-v1" && s.manifest == manifest => {}
        _ => return Err("cloud library head is not retired to this append manifest".into()),
    }
    // Read-only discovery; preserved pending work publishes on the next ordinary poll.
    let mut probe = local.clone();
    probe.protocol = SyncProtocol::AppendLog;
    probe.append = None;
    probe.cursor = None;
    probe.generation = 0;
    probe.pending.clear();
    probe.require_existing = true;
    super::append::synchronize(provider, &probe, false, true).await
}

async fn cancel(provider: &Provider, local: &SyncState, owner: &str) -> Result<Outcome, String> {
    let token = super::host::random_identity()?;
    transition(provider, &local.library, |_, c| {
        let Some(m) = &c.migration else { return Err(Blocked::Invalid("no upgrade in progress".into())) };
        c.takeover(owner, &token, now_ms(), m.owner == owner)?.cancel(&token)
    })
    .await
    .map_err(Failure::message)?;
    super::protocol::receive_only(provider, local).await
}

pub async fn run(provider: &Provider, local: &SyncState, owner: &str, request: Request) -> Result<Outcome, String> {
    if !local.protocol.is_head() {
        return Err("this library already uses append synchronization".into());
    }
    match request {
        Request::Append { writers_stopped } => append(provider, local, owner, writers_stopped).await,
        Request::Cancel => cancel(provider, local, owner).await,
    }
}
