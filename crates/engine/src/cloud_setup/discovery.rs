//! Read-only discovery validates complete library authorities, never directory names.
use super::{FoundLibrary, Probe};
use crate::assets::{AssetError, s3::S3Connection};
use crate::cloud_append::{self as append, control, listing::ListRequest};
use crate::cloud_control::State;
use lightcraft_catalog::replication::SyncProtocol;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, Ordering};

const MAX_REQUESTS: usize = 8192;
const MAX_LIBRARIES: usize = 1024;
struct Scan<'a> {
    connection: &'a S3Connection,
    cancel: &'a AtomicBool,
    requests: usize,
}
impl Scan<'_> {
    fn admit(&mut self) -> Result<(), String> {
        if self.cancel.load(Ordering::Relaxed) {
            return Err(AssetError::Cancelled.to_string());
        }
        self.requests = self.requests.saturating_add(1);
        if self.requests > MAX_REQUESTS {
            return Err("Cloud library discovery exceeded its request limit; narrow the storage folder and retry.".into());
        }
        Ok(())
    }
    fn list(&mut self, prefix: String, delimiter: bool) -> Result<(BTreeSet<String>, BTreeSet<String>), String> {
        let mut request = ListRequest { prefix, delimiter, continuation: None, start_after: None };
        let mut keys = BTreeSet::new();
        let mut prefixes = BTreeSet::new();
        let mut tokens = BTreeSet::new();
        loop {
            self.admit()?;
            let page = self.connection.list_objects_v2(&request, self.cancel).map_err(|e| super::explain(self.connection, e))?;
            keys.extend(page.keys);
            prefixes.extend(page.prefixes);
            if keys.len().saturating_add(prefixes.len()) > append::MAX_RECORDS {
                return Err("Cloud discovery listing exceeds its safety limit; narrow the storage folder.".into());
            }
            let Some(next) = page.next else { return Ok((keys, prefixes)) };
            if !tokens.insert(next.clone()) {
                return Err("Cloud storage repeated a discovery continuation token; retry the connection check.".into());
            }
            request.continuation = Some(next);
        }
    }
    fn append(&mut self, library: &str, head: Option<&(crate::cloud_records::Head, String)>) -> Result<bool, String> {
        self.admit()?;
        let key = append::manifest_key(&self.connection.prefix, library).map_err(|e| e.to_string())?;
        let bytes = self
            .connection
            .get_small(&key, 16 << 10, self.cancel)
            .map_err(|e| super::explain(self.connection, e))?
            .ok_or_else(|| format!("Cloud library {library} has no supported manifest; repair that library or choose another storage folder."))?;
        if bytes.bytes != append::manifest(library).map_err(|e| e.to_string())? {
            return Err(format!("Cloud library {library} has an invalid or unsupported manifest; local data is retained."));
        }
        let (keys, prefixes) = self.list(control::prefix(&self.connection.prefix, library).map_err(|e| e.to_string())?, false)?;
        if !prefixes.is_empty()
            || keys.iter().any(|key| {
                let Some(name) = key.rsplit('/').next() else { return true };
                let Some((revision, digest)) = name.strip_suffix(".json").and_then(|name| name.split_once('-')) else { return true };
                revision.len() != 20
                    || !revision.bytes().all(|b| b.is_ascii_digit())
                    || revision.parse::<u64>().is_err()
                    || digest.len() != 64
                    || !digest.bytes().all(|b| b.is_ascii_digit() || matches!(b, b'a'..=b'f'))
            })
        {
            return Err(format!("Cloud library {library} contains an unsupported control record; local data is retained."));
        }
        let latest = control::latest(&keys);
        if latest.len() > control::MAX_LATEST {
            return Err(format!("Cloud library {library} has conflicting control records; repair it before joining."));
        }
        let mut records = Vec::new();
        for key in latest {
            self.admit()?;
            let bytes = self
                .connection
                .get_small(&key, control::MAX_CONTROL_BYTES, self.cancel)
                .map_err(|e| super::explain(self.connection, e))?
                .ok_or_else(|| format!("Cloud library {library} control disappeared; retry the connection check."))?;
            records.push((key, bytes.bytes));
        }
        // Readable maintenance/newer-writer controls are valid discoveries. Unknown
        // epochs, deleted libraries and ambiguous controls must not become new libraries.
        let effective = control::effective(&self.connection.prefix, library, &records).map_err(|e| e.to_string())?;
        if let Some((head, token)) = head
            && token.starts_with("b2:")
            && head.library.as_deref() == Some(library)
            && head.legacy_append.is_some()
        {
            let manifest = crate::assets::sha256(&append::manifest(library).map_err(|e| e.to_string())?).0;
            if effective.state == State::Retired
                && effective.successor.as_ref().is_some_and(|s| s.protocol == "conditional-head-v1" && s.epoch == 2 && s.manifest == manifest)
            {
                return Ok(false);
            }
            // Initialization may have committed before the compatibility fence.
            // Its head is already authoritative; resuming the upgrade installs
            // the fence before activating any admitted publication epoch.
            effective.may_publish().map_err(|e| e.to_string())?;
            return Ok(false);
        }
        if effective.state == State::Retired {
            return Err(format!("Cloud library {library} moved to another format; update LightCraft before joining."));
        }
        control::publication(Ok(effective)).map_err(|e| e.to_string())?;
        Ok(true)
    }
}
pub(super) fn probe(connection: &S3Connection, cancel: &AtomicBool) -> Result<Probe, String> {
    let root = if connection.prefix.is_empty() { "sync-v2/".to_owned() } else { format!("{}/sync-v2/", connection.prefix) };
    let mut scan = Scan { connection, cancel, requests: 0 };
    let (keys, candidates) = scan.list(root.clone(), true)?;
    if !keys.is_empty() {
        return Err("This storage folder contains unsupported library metadata; repair it or choose another folder.".into());
    }
    if candidates.len() > MAX_LIBRARIES {
        return Err("Too many cloud libraries in this storage folder; choose a narrower folder.".into());
    }
    scan.admit()?;
    let head = crate::cloud_publication::read_head(connection, cancel).map_err(|e| super::explain(connection, e))?;
    let mut found = BTreeMap::new();
    for candidate in candidates {
        let id = candidate
            .strip_prefix(&root)
            .and_then(|s| s.strip_suffix('/'))
            .filter(|id| lightcraft_catalog::assets::identifier(id))
            .ok_or("Storage returned an invalid cloud library folder; choose another folder or repair its contents.")?;
        if scan.append(id, head.as_ref())? {
            found.insert(id.to_owned(), FoundLibrary { id: id.to_owned(), protocol: SyncProtocol::AppendLog });
        }
    }
    let head_taken = head.is_some();
    let mut libraries = Vec::new();
    if let Some((head, _)) = head {
        let id = head.library.as_deref().ok_or("Cloud head has no library identity; local data retained.")?;
        let control = head.control()?;
        control.may_read().map_err(|e| e.to_string())?;
        if control.state == State::Retired {
            let manifest = crate::assets::sha256(&append::manifest(id).map_err(|e| e.to_string())?).0;
            if !control.successor.as_ref().is_some_and(|s| s.protocol == "append-log-v1" && s.epoch == 1 && s.manifest == manifest) {
                return Err("This cloud library moved to an unsupported format; update LightCraft before joining.".into());
            }
            if !found.contains_key(id) {
                return Err("The retired cloud library's successor is missing; finish or repair its upgrade before joining.".into());
            }
        } else {
            if found.contains_key(id) {
                return Err(
                    "This library has both conditional and append metadata without confirmed retirement; finish its upgrade before joining.".into()
                );
            }
            libraries.push(FoundLibrary { id: id.to_owned(), protocol: SyncProtocol::ConditionalHead });
        }
    }
    libraries.extend(found.into_values());
    Ok(Probe { libraries, head_taken })
}
