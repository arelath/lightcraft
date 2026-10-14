//! Installation/catalog writer protection shared by both synchronization protocols.
//! Sequence reservations live outside catalog backups and precede local journal writes.
use crate::{EngineError, Result, Session};
use lightcraft_catalog::{Op, replication::SyncState};
use serde::{Deserialize, Serialize};
use std::io::Read;
use std::path::{Path, PathBuf};

fn error(message: impl Into<String>) -> EngineError {
    EngineError::Other(message.into())
}
fn io(e: std::io::Error) -> EngineError {
    error(format!("cloud writer identity: {e}; use writable persistent installation storage; catalog retained"))
}
pub fn random() -> Result<String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|_| error("secure identity generation failed"))?;
    Ok(crate::assets::sha256(&bytes).0)
}
fn configuration() -> Result<PathBuf> {
    #[cfg(test)]
    {
        Ok(std::env::temp_dir().join(format!("lightcraft-writer-unit-{}", std::process::id())))
    }
    #[cfg(not(test))]
    {
        crate::config::config_dir().ok_or_else(|| error("cloud synchronization requires persistent writer identity; configure LIGHTCRAFT_CONFIG_DIR"))
    }
}
pub fn identity(dir: &Path) -> Result<String> {
    let config = configuration()?;
    std::fs::create_dir_all(&config).map_err(io)?;
    let path = config.join("cloud-installation-id");
    if !path.exists() {
        let id = random()?;
        match lightcraft_catalog::safe_file::write_new(&path, id.as_bytes()) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(io(e)),
        }
    }
    let mut id = String::new();
    std::fs::File::open(path).map_err(io)?.take(65).read_to_string(&mut id).map_err(io)?;
    if id.len() != 64 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(error("invalid installation identity; restore it or configure new persistent installation storage; catalog retained"));
    }
    let canonical = std::fs::canonicalize(dir).map_err(io)?;
    let path =
        canonical.to_str().ok_or_else(|| error("cloud catalog path is not Unicode; choose a Unicode directory to preserve writer identity"))?;
    Ok(crate::assets::sha256(format!("{id}/{path}").as_bytes()).0)
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Reservation {
    version: u32,
    library: String,
    author: String,
    sequence: u64,
}
fn reservation_path(owner: &str) -> Result<PathBuf> {
    if owner.len() != 64 || !owner.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(error("invalid writer owner"));
    }
    Ok(configuration()?.join("cloud-writer-sequences").join(format!("{owner}.json")))
}
fn read(owner: &str) -> Result<Option<Reservation>> {
    let file = match std::fs::File::open(reservation_path(owner)?) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(io(e)),
    };
    let mut bytes = Vec::new();
    file.take(1025).read_to_end(&mut bytes).map_err(io)?;
    if bytes.len() > 1024 {
        return Err(error("oversized writer reservation; catalog retained"));
    }
    let record: Reservation = serde_json::from_slice(&bytes)
        .map_err(|_| error("invalid writer reservation; restore installation storage before editing; catalog retained"))?;
    if record.version != 1
        || !lightcraft_catalog::assets::identifier(&record.library)
        || !lightcraft_catalog::assets::identifier(&record.author)
        || record.author.len() > 96
    {
        return Err(error("unsupported writer reservation; update LightCraft or restore installation storage; catalog retained"));
    }
    Ok(Some(record))
}
fn remember(owner: &str, state: &SyncState) -> Result<()> {
    let path = reservation_path(owner)?;
    let parent = path.parent().ok_or_else(|| error("writer reservation directory unavailable"))?;
    std::fs::create_dir_all(parent).map_err(io)?;
    let record = Reservation {
        version: 1,
        library: state.library.clone(),
        author: state.device.clone(),
        sequence: state.clock.get(&state.device).copied().unwrap_or(0),
    };
    let bytes = serde_json::to_vec(&record).map_err(|e| error(e.to_string()))?;
    lightcraft_catalog::safe_file::write_atomic(&path, &bytes).map_err(io)
}
pub(crate) fn bind_initial(s: &Session, op: &mut Op) -> Result<()> {
    let Some(lib) = s.library.as_ref().filter(|lib| lib.on_disk) else { return Ok(()) };
    let mut pending = vec![op];
    while let Some(op) = pending.pop() {
        match op {
            Op::Batch { ops } => pending.extend(ops.iter_mut().rev()),
            Op::SetSyncState { state: Some(state) } if state.local_owner.is_empty() => {
                state.local_owner = identity(&lib.dir)?;
            }
            _ => {}
        }
    }
    Ok(())
}
pub(crate) fn reserve(s: &Session, op: &Op) -> Result<()> {
    let Some(lib) = s.library.as_ref().filter(|lib| lib.on_disk) else { return Ok(()) };
    let Some(state) = crate::cloud_sync::authored_state(&s.catalog, op) else { return Ok(()) };
    state.validate()?;
    let owner = identity(&lib.dir)?;
    if state.local_owner != owner {
        return Err(error("claim this catalog's writer before editing; reopen the library; pending work retained"));
    }
    let prior = read(&owner)?; // Preserve unrecognized installation records, including during setup.
    if let Some(current) = s.catalog.sync_state() {
        let record = prior.ok_or_else(|| error("writer reservation unavailable; reopen the library before editing"))?;
        if record.library != current.library
            || record.author != current.device
            || current.clock.get(&current.device).copied().unwrap_or(0) < record.sequence
            || state.device != current.device
        {
            return Err(error("catalog predates its writer reservation; reopen to start a safe writing epoch; pending work retained"));
        }
    }
    // A failed catalog append cannot reuse this sequence after a crash or backup restore.
    remember(&owner, state)
}
impl Session {
    pub(crate) fn claim_cloud_owner(&mut self) -> Result<()> {
        let Some(mut state) = self.catalog.sync_state().cloned() else { return Ok(()) };
        let Some(lib) = self.library.as_ref().filter(|lib| lib.on_disk) else { return Ok(()) };
        let owner = identity(&lib.dir)?;
        let record = read(&owner)?;
        let restored = state.local_owner == owner
            && record.as_ref().is_none_or(|r| {
                r.library != state.library || r.author != state.device || state.clock.get(&state.device).copied().unwrap_or(0) < r.sequence
            });
        let changed = if restored {
            state.begin_writing_epoch(owner.clone(), random()?)?;
            true
        } else {
            state.claim_owner(owner.clone(), random()?)?
        };
        if changed || record.as_ref().is_none_or(|r| r.sequence < state.clock.get(&state.device).copied().unwrap_or(0)) {
            remember(&owner, &state)?;
        }
        if changed {
            self.apply_received(Op::SetSyncState { state: Some(Box::new(state)) })?;
            self.persist()?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lightcraft_catalog::replication::Change;
    use serde_json::json;
    fn session() -> Session {
        let root = std::env::temp_dir().join(format!("lightcraft-owner-{}", random().unwrap()));
        let mut s = Session::new().with_fs();
        s.open_library(&root, false).unwrap();
        let op = crate::cloud_sync::enable(&s.catalog, "library".into(), "original".into(), "personal".into()).unwrap();
        s.apply_system(op).unwrap();
        s.persist().unwrap();
        s
    }
    fn edit(s: &Session) -> Op {
        let mut state = s.catalog.sync_state().unwrap().clone();
        state.enqueue(vec![Change { entity: "library:library".into(), field: "label.Green".into(), base: None, value: json!("edited") }]).unwrap();
        Op::SetSyncState { state: Some(Box::new(state)) }
    }
    #[test]
    fn failed_reservation_write_leaves_catalog_outbox_and_journal_untouched() {
        let mut s = session();
        let before = s.catalog.to_snapshot();
        let owner = s.catalog.sync_state().unwrap().local_owner.clone();
        let record = std::fs::read(reservation_path(&owner).unwrap()).unwrap();
        let op = edit(&s);
        let failure = lightcraft_catalog::safe_file::fail_writes_after(0);
        assert!(s.apply_received(op).unwrap_err().to_string().contains("writer identity"));
        drop(failure);
        assert_eq!(s.catalog.to_snapshot(), before);
        assert!(s.pending_log.is_empty());
        assert_eq!(std::fs::read(reservation_path(&owner).unwrap()).unwrap(), record);
    }
    #[test]
    fn reservation_before_failed_catalog_apply_establishes_a_new_epoch_on_retry() {
        let mut s = session();
        let original = s.catalog.sync_state().unwrap().clone();
        reserve(&s, &edit(&s)).unwrap(); // A crash before this operation reaches the journal.
        s.claim_cloud_owner().unwrap();
        let rotated = s.catalog.sync_state().unwrap();
        assert_ne!(rotated.device, original.device);
        assert_eq!(rotated.pending, original.pending);
        assert_eq!(rotated.clock, original.clock);
        assert!(rotated.previous_authors.contains(&original.device));
    }
    #[test]
    fn invalid_or_oversized_reservation_fails_closed_without_catalog_changes() {
        let mut s = session();
        let owner = s.catalog.sync_state().unwrap().local_owner.clone();
        let before = s.catalog.to_snapshot();
        for bytes in [b"future or corrupt".to_vec(), vec![b'x'; 1025]] {
            std::fs::write(reservation_path(&owner).unwrap(), bytes).unwrap();
            assert!(s.claim_cloud_owner().is_err());
            assert_eq!(s.catalog.to_snapshot(), before);
        }
    }
}
