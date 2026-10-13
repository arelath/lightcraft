//! Installation identity stays outside catalog backups. A copied catalog claims a new author.
use crate::{EngineError, Result, Session};
use lightcraft_catalog::replication::SyncProtocol;
use std::io::{Read, Write};
use std::path::Path;
fn error(message: impl Into<String>) -> EngineError {
    EngineError::Other(message.into())
}
fn io(e: std::io::Error) -> EngineError {
    error(format!("installation identity: {e}"))
}
pub fn random() -> Result<String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|_| error("secure identity generation failed"))?;
    Ok(crate::assets::sha256(&bytes).0)
}
pub fn identity(dir: &Path) -> Result<String> {
    let config = crate::config::config_dir()
        .ok_or_else(|| error("append mode requires a persistent installation identity; configure LIGHTCRAFT_CONFIG_DIR"))?;
    std::fs::create_dir_all(&config).map_err(io)?;
    let path = config.join("cloud-installation-id");
    match std::fs::OpenOptions::new().write(true).create_new(true).open(&path) {
        Ok(mut f) => {
            let id = random()?;
            f.write_all(id.as_bytes()).map_err(io)?;
            f.sync_all().map_err(io)?;
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(io(e)),
    }
    let mut id = String::new();
    std::fs::File::open(path).map_err(io)?.take(65).read_to_string(&mut id).map_err(io)?;
    if id.len() != 64 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(error("invalid installation identity; catalog retained"));
    }
    let canonical = std::fs::canonicalize(dir).map_err(io)?;
    Ok(crate::assets::sha256(format!("{id}/{}", canonical.display()).as_bytes()).0)
}
impl Session {
    pub(crate) fn claim_cloud_owner(&mut self) -> Result<()> {
        let Some(mut state) = self.catalog.sync_state().filter(|s| s.protocol == SyncProtocol::AppendLog).cloned() else { return Ok(()) };
        let Some(lib) = self.library.as_ref().filter(|l| l.on_disk) else { return Ok(()) };
        if state.claim_owner(identity(&lib.dir)?, random()?)? {
            self.apply_system(lightcraft_catalog::Op::SetSyncState { state: Some(Box::new(state)) })?;
            self.persist()?;
        }
        Ok(())
    }
}
