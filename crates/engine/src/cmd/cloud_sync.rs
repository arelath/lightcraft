//! Synchronization setup/status through the same command registry as edits and storage.
use super::{CommandSpec, always, bad, cmd};
use crate::{Result, Session};
use serde_json::{Value, json};

pub fn status(s: &mut Session, _: &Value) -> Result<Value> {
    let Some(state) = s.catalog.sync_state() else {
        return Ok(json!({"enabled":false}));
    };
    let conflicts: Vec<_> = state
        .documents
        .iter()
        .flat_map(|(entity, fields)| {
            fields
                .iter()
                .filter(|(_, register)| register.0.values().next().is_some_and(|first| register.0.values().any(|value| value != first)))
                .map(move |(field, register)| json!({"entity":entity,"field":field,"revisions":register.0.keys().collect::<Vec<_>>()}))
        })
        .collect();
    #[cfg(not(target_arch = "wasm32"))]
    let (running, reports) = (s.cloud_replication_jobs.running(), s.cloud_replication_jobs.reports.clone());
    #[cfg(target_arch = "wasm32")]
    let (running, reports) = (false, Vec::<Value>::new());
    Ok(json!({"enabled":true,"library":state.library,"device":state.device,"connection":state.connection,
        "preparingMigration":preparing_migration(s),
        "migrating":migrating(s),
        "protocol":state.protocol,"frontiers":state.append.as_ref().map(|p|&p.frontiers),"gaps":state.append.as_ref().map(|p|&p.gaps),
        "pending":state.pending.len(),"generation":state.generation,"cursor":state.cursor,"conflicts":conflicts,
        "running":running,"reports":reports,
        "transportAvailable":cfg!(not(target_arch="wasm32"))}))
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
    let library = super::str_param(p, "library")
        .map(str::to_owned)
        .or_else(|| s.catalog.cloud_identity("library").map(str::to_owned))
        .map_or_else(identity, Ok)?;
    let owner = if protocol == lightcraft_catalog::replication::SyncProtocol::AppendLog && lib.on_disk {
        crate::cloud_append::owner::identity(&lib.dir)?
    } else {
        String::new()
    };
    let op = crate::cloud_sync::enable_mode(&s.catalog, library, identity()?, connection.into(), protocol, owner)?;
    s.apply_system(op)?;
    status(s, p)
}
pub fn specs() -> Vec<CommandSpec> {
    #[allow(unused_mut)]
    let mut specs = vec![
        cmd!(query "cloud.syncStatus","Cloud synchronization status",[],None,"{} → protocol, pending count, cursor/frontiers, gaps and conflict revisions",always,status),
    ];
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
        "cloud.migrateAppend",
        "Migrate cloud library to append synchronization",
        [],
        None,
        "{writersStopped:true,wait?}; background on desktop; seed verified remote state after every old writer stops",
        always,
        crate::cloud_append::migration::migrate
    ));
    #[cfg(not(target_arch = "wasm32"))]
    specs.push(cmd!(
        "cloud.syncEnable",
        "Enable cloud mutation journal",
        [],
        None,
        "{connection, library?, protocol?}; records durable offline metadata intent; protocol defaults to conditional-head-v1",
        always,
        enable
    ));
    #[cfg(not(target_arch = "wasm32"))]
    specs.push(cmd!(
        "cloud.sync",
        "Synchronize cloud library",
        [],
        None,
        "{wait?, checkpoint?}; reconcile and publish durable mutations using the library's selected protocol",
        always,
        crate::cloud_replication::start
    ));
    specs.push(cmd!(query "cloud.configureBrowser","Configure browser cloud storage",[],None,"{id, prefix, authorizeEndpoint, storageOrigin, token?}; device-local settings and ephemeral login",always, browser_host));
    #[cfg(target_arch = "wasm32")]
    specs.extend([
        cmd!(query "cloud.prepareMigration","Pause this device for cloud migration",[],None,"{}; stop automatic publication; Sync now resumes",always,browser_host),
        cmd!(query "cloud.migrateAppend","Migrate cloud library to append synchronization",[],None,"{writersStopped:true}; seed verified remote history; pause every old writer",always,browser_host),
        cmd!(query "cloud.connections","Cloud storage connections",[],None,"{}",always,browser_host),
        cmd!(query "cloud.syncEnable","Enable cloud synchronization",[],None,"{connection,library?,protocol?}",always,browser_host),
        cmd!(query "cloud.sync","Synchronize cloud library",[],None,"{checkpoint?}; durable async synchronization",always,browser_host),
        cmd!(query "cloud.fetch","Download cloud originals",[],None,"{ids?}; verified asynchronous acquisition",always,browser_host),
        cmd!(query "cloud.reconnect","Retry cloud transfers",[],None,"{}",always,browser_host),
    ]);
    specs
}
fn browser_host(_: &mut Session, _: &Value) -> Result<Value> {
    Err(bad("cloud.browser", "this operation requires the browser host and persistent browser storage"))
}

fn resolve(s: &mut Session, p: &Value) -> Result<Value> {
    const C: &str = "cloud.resolveConflict";
    let string = |key| super::str_param(p, key).ok_or_else(|| bad(C, format!("missing {key}")));
    let op = crate::cloud_sync::resolve(&s.catalog, string("entity")?, string("field")?, string("revision")?)?;
    s.apply_received(op)?;
    status(s, p)
}
fn restore(s: &mut Session, p: &Value) -> Result<Value> {
    let entity = super::str_param(p, "entity").ok_or_else(|| bad("cloud.restoreDeleted", "missing entity"))?;
    let op = crate::cloud_sync::restore_deleted(&s.catalog, entity)?;
    s.apply_received(op)?;
    status(s, p)
}
