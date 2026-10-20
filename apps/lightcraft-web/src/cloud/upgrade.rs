//! Browser library upgrades through the conditional head's maintenance state, matching
//! the native runner (`lightcraft_engine::cloud_upgrade`). Every transition is a CAS on
//! the head and is confirmed by rereading exactly the control it wrote.
use super::Provider;
use super::protocol::{Outcome, immutable, read_head};
use lightcraft_engine::assets::sha256;
use lightcraft_engine::catalog::replication::{SyncProtocol, SyncState};
use lightcraft_engine::cloud_control::{Blocked, Boundary, Control, Plan, State, Successor};
use lightcraft_engine::cloud_records::{Batch, Head, RecordRef, object_key};
use lightcraft_engine::cloud_status::{CloudFailure, FailureKind};

const ATTEMPTS: usize = 16;
mod b2;
mod lease;
mod rebase;
use lightcraft_engine::cloud_control::lease::Lease;
use std::cell::RefCell;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Request {
    Auto {
        writers_stopped: bool,
    },
    Rebase,
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
async fn current(provider: &Provider, library: &str) -> Result<(Head, String, Control), CloudFailure> {
    let (head, etag) = read_head(provider, library).await?.ok_or("cloud library has no head to upgrade; synchronize first")?;
    let control = head.control()?;
    control.may_read()?;
    Ok((head, etag, control))
}
enum Failure {
    /// Storage rejected conditional writes (HTTP 501): the head cannot hold a barrier.
    Unsupported,
    Other(CloudFailure),
}
impl From<CloudFailure> for Failure {
    fn from(why: CloudFailure) -> Self {
        Self::Other(why)
    }
}
impl Failure {
    fn message(self) -> CloudFailure {
        match self {
            Self::Unsupported => CloudFailure::new(
                FailureKind::Unsupported,
                "storage rejected conditional writes (HTTP 501); safe catalog publication requires If-None-Match and If-Match",
            ),
            Self::Other(why) => why,
        }
    }
}
/// A marker batch plus a conditional head carrying `next`'s control.
async fn transition_control(
    provider: &Provider,
    library: &str,
    next: impl Fn(&Head, &Control) -> Result<Option<Control>, CloudFailure>,
) -> Result<Head, Failure> {
    for _ in 0..ATTEMPTS {
        let (head, etag, observed) = current(provider, library).await?;
        let Some(wanted) = next(&head, &observed)? else { return Ok(head) };
        if wanted == observed {
            return Ok(head);
        }
        let batch = Batch { version: 3, parent: Some(head.tip.clone()), imports: Vec::new(), mutations: Vec::new(), checkpoint_marker: true };
        let bytes = serde_json::to_vec(&batch).map_err(CloudFailure::from)?;
        let hash = sha256(&bytes);
        let tip = RecordRef { key: object_key(&provider.connection.prefix, wanted.epoch, "changes", &hash.0), size: bytes.len() as u64, hash };
        // Classify a 501 before the shared immutable writer turns it into text.
        let probe = if etag.starts_with("b2:") {
            immutable(provider, &tip.key, &bytes).await?;
            None
        } else {
            Some(provider.request("PUT", &tip.key, None, Some("*"), Some(&bytes), 16 << 10).await)
        };
        match probe {
            Some(Ok(reply)) if reply.status == 501 => return Err(Failure::Unsupported),
            Some(Ok(reply)) if matches!(reply.status, 200 | 201 | 204) => {}
            None => {}
            _ => immutable(provider, &tip.key, &bytes).await?,
        }
        let replacement = Head::next(Some(&head), tip, library, head.checkpoint.clone(), wanted.clone()).map_err(CloudFailure::from)?;
        match super::protocol::write_head(provider, Some(&etag), &replacement).await {
            Err(error) if error.http_status == Some(501) => return Err(Failure::Unsupported),
            Err(error) if super::protocol::retry_cas(&error) => {}
            Err(error) => return Err(error.into()),
            Ok(()) => {}
        }
        let (confirmed, _, after) = current(provider, library).await?;
        if after == wanted {
            return Ok(confirmed);
        }
    }
    Err(CloudFailure::new(FailureKind::Contention, "cloud library head remained busy; upgrade not started or not confirmed").into())
}
async fn transition(provider: &Provider, library: &str, next: impl Fn(&Head, &Control) -> Result<Control, CloudFailure>) -> Result<Head, Failure> {
    transition_control(provider, library, |h, c| next(h, c).map(Some)).await
}

fn taken_over() -> CloudFailure {
    CloudFailure::new(FailureKind::Contention, "upgrade ownership was taken over by another device; this result was not activated")
}

/// Seed append-log, then retire the head; or rejoin a head that is already retired.
async fn append(provider: &Provider, local: &SyncState, owner: &str, writers_stopped: bool) -> Result<Outcome, CloudFailure> {
    if super::protocol::b2_enabled(provider).await? {
        return Err(CloudFailure::new(FailureKind::Unsupported, "this B2 library uses fenced publication; use the rebase plan"));
    }
    let library = &local.library;
    let manifest = sha256(&lightcraft_engine::cloud_append::manifest(library).map_err(CloudFailure::from)?).0;
    let legacy = |failure: CloudFailure| -> Result<(), CloudFailure> {
        if writers_stopped {
            Ok(())
        } else {
            let message = format!("{}; stop every other device, then retry with writersStopped:true", failure.message);
            Err(CloudFailure { message, ..failure })
        }
    };
    let Some((_, _, observed)) = (match read_head(provider, library).await? {
        Some(_) => Some(current(provider, library).await?),
        None => None,
    }) else {
        legacy("cloud library has no published head".into())?;
        return super::append::migrate(provider, local).await;
    };
    if observed.state != State::Retired {
        let token = super::host::random_identity().map_err(CloudFailure::local)?;
        let id = format!("appendLog-{}", token.get(..16).unwrap_or("migration"));
        let acquired = transition(provider, library, |head, c| {
            Ok(match &c.migration {
                None => {
                    c.begin(&id, Plan::AppendLog, Boundary { tip: head.tip.hash.0.clone(), generation: head.generation }, owner, &token, now_ms())
                }
                Some(m) if m.plan != Plan::AppendLog => Err(Blocked::Maintenance { migration: m.id.clone(), plan: m.plan, owner: m.owner.clone() }),
                Some(m) => c.takeover(owner, &token, now_ms(), m.owner == owner),
            }?)
        })
        .await;
        let acquired = match acquired {
            Ok(head) => head,
            Err(Failure::Unsupported) => {
                legacy(Failure::Unsupported.message())?;
                return super::append::migrate(provider, local).await;
            }
            Err(failure) => return Err(failure.message()),
        };
        let ownership = RefCell::new(Lease::new(acquired.control()?)?);
        return lease::run(provider, library, &ownership, async {
            let publishing = transition(provider, library, |head, c| {
                if head.control.as_ref().and_then(|c| c.migration.as_ref()).is_none_or(|m| m.token != token) {
                    return Err(taken_over());
                }
                Ok(c.begin_output(&token, now_ms())?)
            })
            .await
            .map_err(Failure::message)?;
            let seeded = super::append::migrate(provider, local).await?;
            let successor = Successor { protocol: "append-log-v1".into(), epoch: 1, manifest: manifest.clone() };
            ownership.borrow_mut().expect_completion(publishing.control()?.retire(&token, successor.clone())?)?;
            transition(provider, library, |head, c| {
                if head.control.as_ref().and_then(|c| c.migration.as_ref()).is_none_or(|m| m.token != token) {
                    return Err(taken_over());
                }
                Ok(c.retire(&token, successor.clone())?)
            })
            .await
            .map_err(Failure::message)?;
            Ok(seeded)
        })
        .await;
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
    super::append::receive_only(provider, &probe).await
}

async fn cancel(provider: &Provider, local: &SyncState, owner: &str) -> Result<Outcome, CloudFailure> {
    let token = super::host::random_identity().map_err(CloudFailure::local)?;
    transition(provider, &local.library, |_, c| {
        let Some(m) = &c.migration else { return Err("no upgrade in progress; nothing to cancel".into()) };
        Ok(c.takeover(owner, &token, now_ms(), m.owner == owner)?.cancel(&token)?)
    })
    .await
    .map_err(Failure::message)?;
    super::protocol::receive_only(provider, local).await
}

pub async fn run(provider: &Provider, local: &SyncState, owner: &str, request: Request) -> Result<Outcome, CloudFailure> {
    if !local.protocol.is_head() {
        return match request {
            Request::Auto { .. } | Request::Rebase => b2::run(provider, local, owner).await,
            Request::Cancel if super::protocol::b2_enabled(provider).await? => cancel(provider, local, owner).await,
            _ => Err(CloudFailure::new(FailureKind::Unsupported, "this library already uses append synchronization")),
        };
    }
    match request {
        Request::Auto { writers_stopped } => {
            if super::protocol::b2_enabled(provider).await? {
                rebase::run(provider, local, owner).await
            } else {
                append(provider, local, owner, writers_stopped).await
            }
        }
        Request::Rebase => rebase::run(provider, local, owner).await,
        Request::Append { writers_stopped } => append(provider, local, owner, writers_stopped).await,
        Request::Cancel => cancel(provider, local, owner).await,
    }
}
