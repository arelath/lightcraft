//! Synchronization setup/status through the same command registry as edits and storage.
use super::{CommandSpec, always, bad, cmd};
use crate::{Result, Session};
use serde_json::{Value, json};
mod recovery;

pub fn status(s: &mut Session, params: &Value) -> Result<Value> {
    if params.get("section").and_then(Value::as_str) == Some("choice") {
        return recovery::choice(s, params);
    }
    if params.get("section").is_some() {
        return recovery::page(s, params);
    }
    let token = s.library_token();
    let Some(state) = s.catalog.sync_state() else {
        return Ok(json!({"enabled":false}));
    };
    if s.cloud_conflicts_cache.as_ref().is_none_or(|(owner, revision, _)| !std::sync::Arc::ptr_eq(owner, &token) || *revision != s.catalog.revision) {
        let conflicts: Vec<_> = state
            .documents
            .iter()
            .flat_map(|(entity, fields)| {
                fields
                    .iter()
                    .filter(|(_, register)| register.0.values().next().is_some_and(|first| register.0.values().any(|value| value != first)))
                    .map(move |(field, register)| conflict_view(state, entity, field, register))
            })
            .collect();
        let deleted = deleted_photos(state);
        let utilization = serde_json::to_value(state.utilization()?).map_err(|e| crate::EngineError::Other(e.to_string()))?;
        s.cloud_conflicts_cache = Some((
            token,
            s.catalog.revision,
            json!({"conflicts":conflicts,"structuralConflicts":crate::cloud_sync::structural_conflicts(&s.catalog),
                "warnings":utilization.get("warnings"),"utilization":utilization,
                "deletedPhotos":deleted.0,"deletedPhotoCount":deleted.1}),
        ));
    }
    let conflicts = s.cloud_conflicts_cache.as_ref().map(|(_, _, value)| value);
    #[cfg(not(target_arch = "wasm32"))]
    let (running, reports) = (s.cloud_replication_jobs.running(), s.cloud_replication_jobs.reports.clone());
    #[cfg(not(target_arch = "wasm32"))]
    let health = s.cloud_replication_jobs.health(!s.cloud_transition.local_only(), s.cloud_paused);
    #[cfg(not(target_arch = "wasm32"))]
    let publication_blocked = s.cloud_replication_jobs.publication_blocked().map(str::to_owned);
    #[cfg(target_arch = "wasm32")]
    let publication_blocked: Option<String> = None;
    #[cfg(not(target_arch = "wasm32"))]
    let publication_failure = s.cloud_replication_jobs.publication_failure().cloned();
    #[cfg(target_arch = "wasm32")]
    let publication_failure: Option<crate::cloud_status::Failure> = None;
    #[cfg(target_arch = "wasm32")]
    let (running, reports) = (false, Vec::<Value>::new());
    #[cfg(target_arch = "wasm32")]
    let health = crate::cloud_status::PollHealth::default().view(true, false, s.cloud_paused, false, false);
    #[cfg(not(target_arch = "wasm32"))]
    let enabled = !s.cloud_transition.local_only();
    #[cfg(target_arch = "wasm32")]
    let enabled = true;
    Ok(json!({"enabled":enabled,"library":state.library,"device":state.device,"connection":state.connection,
        "paused":s.cloud_paused,"userPaused":s.cloud_paused,"canResume":crate::cloud_status::can_resume(s),
        "health":health,
        "publicationBlocked":publication_blocked,"publicationFailure":publication_failure,
        "preparingMigration":preparing_migration(s),
        "canUpgrade":crate::cloud_status::can_upgrade(s),
        "migrating":migrating(s),
        "protocol":state.protocol,"frontiers":state.append.as_ref().map(|p|&p.frontiers),"gaps":state.append.as_ref().map(|p|&p.gaps),
        "appendAudit":state.append.as_ref().map(|p|&p.scan.audit),
        "consumedCheckpoints":state.append.as_ref().map(|p|p.scan.checkpoints.len()),
        "pending":state.pending.len(),"generation":state.generation,"cursor":state.cursor,"conflicts":conflicts.and_then(|v|v.get("conflicts")),
        "structuralConflicts":conflicts.and_then(|v|v.get("structuralConflicts")),
        "deletedPhotos":conflicts.and_then(|v|v.get("deletedPhotos")),"deletedPhotoCount":conflicts.and_then(|v|v.get("deletedPhotoCount")),
        "utilization":conflicts.and_then(|v|v.get("utilization")),"capacityWarnings":conflicts.and_then(|v|v.get("warnings")),
        "running":running,"reports":reports,
        "transportAvailable":cfg!(not(target_arch="wasm32"))}))
}
/// Retained conflict choices listed per field, so a long-lived conflict cannot grow the status.
pub const MAX_CONFLICT_CHOICES: usize = 8;
/// Deleted photos listed for recovery; `deletedPhotoCount` reports the full number.
pub const MAX_DELETED_LISTED: usize = 200;
const PREVIEW_CHARS: usize = 80;

/// What a person needs to pick between the retained values of one field: the entity's name,
/// and each value's short preview, newest first with the displayed (winning) value at the top.
fn conflict_view(
    state: &lightcraft_catalog::replication::SyncState,
    entity: &str,
    field: &str,
    register: &lightcraft_catalog::replication::Register,
) -> Value {
    let winner = state.winning(entity, field).map(|(stamp, _)| stamp.id());
    let sequence = |revision: &str| revision.rsplit_once('-').and_then(|(_, n)| n.parse::<u64>().ok()).unwrap_or(0);
    let mut revisions: Vec<(&String, &Value)> = register.0.iter().collect();
    revisions.sort_by_key(|(revision, _)| (winner.as_deref() != Some(revision.as_str()), std::cmp::Reverse(sequence(revision))));
    let choices: Vec<_> = revisions
        .iter()
        .take(MAX_CONFLICT_CHOICES)
        .map(|(revision, value)| {
            let (kind, preview) = preview(value);
            json!({"revision":revision,"current":winner.as_deref()==Some(revision.as_str()),
                "thisDevice":revision.rsplit_once('-').is_some_and(|(device, _)| device == state.device),
                "kind":kind,"preview":preview})
        })
        .collect();
    json!({"entity":entity,"field":field,"name":entity_name(state, entity),"revisions":register.0.keys().collect::<Vec<_>>(),
        "choices":choices,"moreChoices":register.0.len().saturating_sub(MAX_CONFLICT_CHOICES)})
}
/// `(kind, text)`: kind is "none", "settings" (an object: develop settings and the like,
/// too large to preview) or "value" with a short single-line text.
fn preview(value: &Value) -> (&'static str, String) {
    let text = match value {
        Value::Null => return ("none", String::new()),
        Value::Object(_) => return ("settings", String::new()),
        Value::String(text) => text.clone(),
        Value::Array(items) => items.iter().map(|item| item.as_str().map_or_else(|| item.to_string(), str::to_owned)).collect::<Vec<_>>().join(", "),
        other => other.to_string(),
    };
    let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut short: String = line.chars().take(PREVIEW_CHARS).collect();
    if line.chars().count() > PREVIEW_CHARS {
        short.push('…');
    }
    ("value", short)
}
/// A photo's file name or an album's name as currently shown; None when neither is known.
fn entity_name(state: &lightcraft_catalog::replication::SyncState, entity: &str) -> Option<String> {
    let field = match entity.split_once(':')?.0 {
        "photo" => "file_name",
        "album" => "name",
        _ => return None,
    };
    state.winning(entity, field).and_then(|(_, value)| value.as_str()).map(str::to_owned)
}
/// Retained deleted cloud photos that `cloud.restoreDeleted` can recover, capped for display.
fn deleted_photos(state: &lightcraft_catalog::replication::SyncState) -> (Vec<Value>, usize) {
    let mut listed = Vec::new();
    let mut count = 0usize;
    for entity in state.documents.keys().filter(|entity| entity.starts_with("photo:")) {
        if state.winning(entity, "deleted").is_some_and(|(_, value)| value.as_bool() == Some(true)) {
            count = count.saturating_add(1);
            if listed.len() < MAX_DELETED_LISTED {
                listed.push(json!({"entity":entity,"name":entity_name(state, entity)}));
            }
        }
    }
    (listed, count)
}
fn preparing_migration(s: &Session) -> bool {
    #[cfg(not(target_arch = "wasm32"))]
    {
        s.cloud_replication_jobs.preparing_migration()
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = s;
        false
    }
}
fn migrating(s: &Session) -> bool {
    #[cfg(not(target_arch = "wasm32"))]
    {
        s.cloud_replication_jobs.migrating()
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = s;
        false
    }
}
#[cfg(not(target_arch = "wasm32"))]
fn enable(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "cloud.syncEnable";
    let protocol: lightcraft_catalog::replication::SyncProtocol =
        serde_json::from_value(p.get("protocol").cloned().unwrap_or(json!("conditional-head-v1")))
            .map_err(|_| bad(C, "invalid synchronization protocol"))?;
    if let Some(state) = s.catalog.sync_state() {
        if super::str_param(p, "connection").is_some_and(|id| id != state.connection)
            || super::str_param(p, "library").is_some_and(|id| id != state.library)
            || p.get("protocol").is_some_and(|_| protocol != state.protocol)
        {
            return Err(bad(C, "this library already has a durable synchronization identity; open a separate library to join another one"));
        }
        crate::cloud_transition::enable(s)?;
        return status(s, p);
    }
    let connection = super::str_param(p, "connection").ok_or_else(|| bad(C, "missing connection"))?;
    let lib = s.library.as_mut().ok_or_else(|| bad(C, "open a persistent library first"))?;
    if !lib.cloud_connections()?.iter().any(|c| c.id == connection) {
        return Err(bad(C, "configure the storage connection first"));
    }
    let identity = || -> Result<String> {
        let mut bytes = [0u8; 32];
        getrandom::fill(&mut bytes).map_err(|_| bad(C, "secure random identity generation failed"))?;
        Ok(crate::assets::sha256(&bytes).0)
    };
    let joining = super::str_param(p, "library").is_some() || s.catalog.cloud_identity("library").is_some();
    let library = super::str_param(p, "library")
        .map(str::to_owned)
        .or_else(|| s.catalog.cloud_identity("library").map(str::to_owned))
        .map_or_else(identity, Ok)?;
    let owner = if lib.on_disk { crate::cloud_append::owner::identity(&lib.dir)? } else { String::new() };
    let enroll = if joining { crate::cloud_sync::join_mode } else { crate::cloud_sync::enable_mode };
    let op = enroll(&s.catalog, library, identity()?, connection.into(), protocol, owner)?;
    s.apply_system(op)?;
    s.cloud_uploads.request_resume(true);
    status(s, p)
}
pub fn specs() -> Vec<CommandSpec> {
    #[allow(unused_mut)]
    let mut specs = vec![
        cmd!(query "cloud.overview", "Cloud Library Status", [], None, "{}; managed file counts, known data size, queue and live publication progress", always, crate::cloud_status::overview),
        cmd!(query "cloud.pause", "Pause Syncing", [], None, "{}; persist device-local pause; stop uploads and catalog publication, retaining durable work", always, crate::cloud_status::pause),
        cmd!(query "cloud.resume", "Resume Syncing", [], None, "{}; resume durable uploads and catalog publication after paused jobs drain", always, crate::cloud_status::resume),
        cmd!(query "cloud.syncStatus","Cloud synchronization status",[],None,"{} → status; {section: conflicts|choices|deleted|structural, search?, cursor?, limit?, entity?, field?} → bounded recovery page; choices requires entity/field; {section: choice, entity, field, revision, library?, libraryGeneration?} → full retained value and writer/revision (read-only)",always,status),
    ];
    #[cfg(not(target_arch = "wasm32"))]
    specs.extend([
        cmd!(query "cloud.traffic", "Cloud traffic diagnostics", [], None, "{limit?:0..256,operation?:id}; process-local request/body-byte totals, recent requests, operation costs and hourly buckets; no network I/O", always, crate::cloud_traffic::status),
        cmd!(query "cloud.syncDisablePreview", "Review turning off cloud sync", [], None, "{}; estimate permanent local copies before final remote discovery", always, crate::cloud_transition::preview),
        cmd!(query "cloud.syncDisable", "Turn off cloud sync", [], None, "{confirm?:true,cancel?:true,wait?:false}; download and verify live library files before committing local-only mode; never delete cloud files", always, crate::cloud_transition::disable),
        cmd!(query "cloud.transitionStatus", "Cloud mode conversion status", [], None, "{}; durable conversion stage, bytes, files, destination and recovery actions", always, crate::cloud_transition::status),
    ]);
    specs.push(cmd!(
        "cloud.restoreDeleted",
        "Recover deleted cloud photo",
        [],
        None,
        "{entity}; explicitly restore a retained tombstone and its offline edits",
        always,
        restore
    ));
    specs.push(cmd!(
        "cloud.resolveConflict",
        "Resolve cloud field conflict",
        [],
        None,
        "{entity, field, revision}; chooses a retained revision and creates a causal resolution",
        always,
        resolve
    ));
    specs.push(cmd!(
        "cloud.acceptStructure",
        "Keep current folder or stack structure",
        [],
        None,
        "{entity}; explicitly resolve a structural conflict using the valid displayed parent/membership; preserve immutable history",
        always,
        |s, p| {
            #[cfg(not(target_arch = "wasm32"))]
            s.claim_cloud_owner()?;
            let entity = super::str_param(p, "entity").ok_or_else(|| bad("cloud.acceptStructure", "missing entity"))?;
            let op = crate::cloud_sync::accept_structure(&s.catalog, entity)?;
            s.apply_received(op)?;
            status(s, p)
        }
    ));
    #[cfg(not(target_arch = "wasm32"))]
    specs.push(cmd!(
        "cloud.prepareMigration",
        "Pause this device for cloud migration",
        [],
        None,
        "{}; stop local automatic publication; Sync now explicitly resumes",
        always,
        crate::cloud_replication::prepare_migration
    ));
    #[cfg(not(target_arch = "wasm32"))]
    specs.push(cmd!(
        "cloud.upgradeLibrary",
        "Upgrade cloud library format",
        [],
        None,
        "{plan?: appendLog|rebase, cancel?, wait?}; automatically choose the supported successor; elect maintenance through the head, preserve pending identities and rejoin an existing upgrade",
        always,
        crate::cloud_replication::start_migration
    ));
    #[cfg(not(target_arch = "wasm32"))]
    specs.push(cmd!(
        "cloud.migrateAppend",
        "Migrate cloud library to append synchronization",
        [],
        None,
        "{wait?}; alias of cloud.upgradeLibrary {plan:\"appendLog\"}; writersStopped is accepted but no longer required",
        always,
        crate::cloud_append::migration::migrate
    ));
    #[cfg(not(target_arch = "wasm32"))]
    specs.push(cmd!(
        "cloud.syncEnable",
        "Enable cloud mutation journal",
        [],
        None,
        "{connection, library?, protocol?}; omit library to create, supply its ID to join an existing library; protocol defaults to conditional-head-v1",
        always,
        enable
    ));
    #[cfg(not(target_arch = "wasm32"))]
    specs.push(cmd!(
        "cloud.sync",
        "Synchronize cloud library",
        [],
        None,
        "{wait?, checkpoint?, audit?}; incremental synchronization; audit:true additionally requests bounded fresh verification; cloud.verifyHistory verifies without publication",
        always,
        crate::cloud_replication::start
    ));
    #[cfg(not(target_arch = "wasm32"))]
    specs.push(cmd!(
        "cloud.verifyHistory",
        "Verify remote history",
        [],
        None,
        "{wait?}; fresh bounded history verification; preserves local edits and pending work",
        always,
        crate::cloud_replication::verify
    ));
    specs.push(cmd!(query "cloud.configureBrowser","Configure browser cloud storage",[],None,"{id, prefix, authorizeEndpoint, storageOrigin, token?}; device-local settings and ephemeral login",always, browser_host));
    #[cfg(target_arch = "wasm32")]
    specs.extend([
        cmd!(query "cloud.prepareMigration","Pause this device for cloud migration",[],None,"{}; stop automatic publication; Sync now resumes",always,browser_host),
        cmd!(query "cloud.upgradeLibrary","Upgrade cloud library format",[],None,"{plan?: appendLog, cancel?}; pause publication through the head, seed and retire, or rejoin a retired head",always,browser_host),
        cmd!(query "cloud.migrateAppend","Migrate cloud library to append synchronization",[],None,"{}; alias of cloud.upgradeLibrary {plan:\"appendLog\"}",always,browser_host),
        cmd!(query "cloud.connections","Cloud storage connections",[],None,"{}",always,browser_host),
        cmd!(query "cloud.syncEnable","Enable cloud synchronization",[],None,"{connection,library?,protocol?}; omit library to create, supply its ID to join",always,browser_host),
        cmd!(query "cloud.sync","Synchronize cloud library",[],None,"{checkpoint?}; durable async synchronization",always,browser_host),
        cmd!(query "cloud.verifyHistory","Verify remote history",[],None,"{}; fresh bounded remote history verification",always,browser_host),
        cmd!(query "cloud.fetch","Download cloud originals",[],None,"{ids?}; verified asynchronous acquisition",always,browser_host),
        cmd!(query "cloud.reconnect","Retry cloud transfers",[],None,"{}",always,browser_host),
    ]);
    specs
}
fn browser_host(_: &mut Session, _: &Value) -> Result<Value> {
    Err(bad("cloud.browser", "this operation requires the browser host and persistent browser storage"))
}

fn resolve(s: &mut Session, p: &Value) -> Result<Value> {
    #[cfg(not(target_arch = "wasm32"))]
    s.claim_cloud_owner()?;
    const C: &str = "cloud.resolveConflict";
    let string = |key| super::str_param(p, key).ok_or_else(|| bad(C, format!("missing {key}")));
    let op = crate::cloud_sync::resolve(&s.catalog, string("entity")?, string("field")?, string("revision")?)?;
    s.apply_received(op)?;
    status(s, p)
}
fn restore(s: &mut Session, p: &Value) -> Result<Value> {
    #[cfg(not(target_arch = "wasm32"))]
    s.claim_cloud_owner()?;
    let entity = super::str_param(p, "entity").ok_or_else(|| bad("cloud.restoreDeleted", "missing entity"))?;
    let op = crate::cloud_sync::restore_deleted(&s.catalog, entity)?;
    s.apply_received(op)?;
    status(s, p)
}
