//! Device-local rejoin baseline. The ordinary catalog is the durable local overlay;
//! its shared causal state stays frozen until explicitly re-enabling synchronization.
use crate::{CatalogError, Result, assets::identifier, replication};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Write,
};

pub type Documents = BTreeMap<String, BTreeMap<String, Value>>;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Baseline {
    pub library: String,
    pub connection: String,
    pub clock: replication::Clock,
    pub documents: Documents,
    #[serde(default)]
    pub imports: BTreeSet<String>,
}
impl Baseline {
    pub fn validate(&self) -> Result<()> {
        if !identifier(&self.library)
            || !identifier(&self.connection)
            || self.clock.len() > replication::MAX_DEVICES
            || self.clock.iter().any(|(id, sequence)| !identifier(id) || id.len() > 96 || *sequence == 0)
            || self.documents.len() > replication::MAX_RECORDS
            || self.imports.len() > replication::MAX_RECORDS
            || self.imports.iter().any(|id| !identifier(id))
            || self.documents.iter().any(|(entity, fields)| {
                !replication::entity(entity)
                    || fields.len() > replication::MAX_FIELDS
                    || fields.keys().any(|field| field.is_empty() || field.len() > 1024 || field.chars().any(char::is_control))
            })
        {
            return Err(CatalogError::Invalid("invalid local synchronization baseline; saved local work retained".into()));
        }
        struct Budget(usize);
        impl Write for Budget {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0 = self
                    .0
                    .checked_add(bytes.len())
                    .filter(|n| *n <= replication::MAX_STATE_BYTES)
                    .ok_or_else(|| std::io::Error::other("local synchronization baseline exceeds 64 MiB"))?;
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        serde_json::to_writer(Budget(0), self).map_err(|e| CatalogError::Capacity(format!("cannot preserve local rejoin baseline: {e}")))
    }
}
