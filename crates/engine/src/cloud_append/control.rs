//! Append-log library control: immutable, revisioned `control/` records carrying the same
//! envelope as the conditional head. Append mode has no compare-and-swap, so this is a
//! cooperative barrier: every conforming writer reads the latest record before each
//! publication pass and stops publishing when it is not Active (or needs a newer writer).
//! An election-capable provider adapter (B2 conditional bucket info, or a commit
//! coordinator) is required before append maintenance can be a hard barrier.
use crate::assets::{AssetError, Result, sha256};
use crate::cloud_control::{Blocked, Control};
use std::collections::BTreeSet;

/// Records fetched from the greatest revision; more distinct ones are a conflict.
pub const MAX_LATEST: usize = 16;
pub const MAX_CONTROL_BYTES: usize = 16 << 10;

pub fn prefix(storage: &str, library: &str) -> Result<String> {
    Ok(format!("{}/control/", super::root(storage, library)?))
}
pub fn encode(control: &Control) -> Result<Vec<u8>> {
    control.validate().map_err(|b| AssetError::Invalid(b.to_string()))?;
    let bytes = serde_json::to_vec(control).map_err(|_| AssetError::Invalid("cannot encode library control".into()))?;
    if bytes.len() > MAX_CONTROL_BYTES {
        return Err(AssetError::Invalid("library control exceeds 16 KiB".into()));
    }
    Ok(bytes)
}
pub fn key(storage: &str, control: &Control) -> Result<String> {
    Ok(format!("{}{:020}-{}.json", prefix(storage, &control.library)?, control.revision, sha256(&encode(control)?).0))
}
/// Keys at the greatest revision (names sort by zero-padded revision), at most
/// `MAX_LATEST + 1` so a caller can detect an unbounded conflict.
pub fn latest(keys: &BTreeSet<String>) -> Vec<String> {
    let revision = |key: &String| key.rsplit('/').next().and_then(|name| name.split('-').next()).map(str::to_owned);
    let Some(top) = keys.iter().next_back().and_then(revision) else { return Vec::new() };
    keys.iter().rev().take_while(|key| revision(key).as_deref() == Some(top.as_str())).take(MAX_LATEST + 1).cloned().collect()
}
/// The effective control from the latest records: none means the implicit Active epoch 1.
pub fn effective(storage: &str, library: &str, records: &[(String, Vec<u8>)]) -> std::result::Result<Control, Blocked> {
    if records.len() > MAX_LATEST {
        return Err(Blocked::Conflict("too many control records at one revision".into()));
    }
    let mut controls = Vec::new();
    for (key, bytes) in records {
        if bytes.len() > MAX_CONTROL_BYTES {
            return Err(Blocked::Invalid("oversized library control record".into()));
        }
        let value: serde_json::Value = serde_json::from_slice(bytes).map_err(|_| Blocked::Invalid("malformed library control record".into()))?;
        let control = Control::decode(&value)?;
        let canonical = encode(&control).map_err(|e| Blocked::Invalid(e.to_string()))?;
        if control.library != library || canonical != *bytes || self::key(storage, &control).ok().as_deref() != Some(key.as_str()) {
            return Err(Blocked::Invalid("library control record does not match its key".into()));
        }
        controls.push(control);
    }
    let Some(first) = controls.first() else { return Ok(Control::initial_append(library)) };
    if controls.iter().any(|c| c != first) {
        return Err(Blocked::Conflict(format!("{} records at revision {}", controls.len(), first.revision)));
    }
    Ok(first.clone())
}
/// Reading is refused for a newer envelope/reader or an invalid record; any other
/// blocked state permits receiving but not publication.
pub fn publication(control: std::result::Result<Control, Blocked>) -> std::result::Result<Option<String>, Blocked> {
    match control.and_then(|c| c.may_publish().map(|_| c)) {
        Ok(_) => Ok(None),
        Err(blocked @ (Blocked::UpdateToRead(_) | Blocked::Invalid(_) | Blocked::Deleted)) => Err(blocked),
        Err(blocked) => Ok(Some(blocked.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cloud_control::State;
    fn record(c: &Control) -> (String, Vec<u8>) {
        (key("p", c).unwrap(), encode(c).unwrap())
    }
    #[test]
    fn latest_records_decide_append_publication_and_conflicts_pause_it() {
        assert_eq!(effective("p", "library", &[]).unwrap(), Control::initial_append("library"));
        let mut maintenance = Control::initial_append("library");
        maintenance.revision = 3;
        maintenance.state = State::Retired;
        maintenance.successor = Some(crate::cloud_control::Successor { protocol: "append-log-v2".into(), epoch: 2, manifest: "d".repeat(64) });
        let mut other = Control::initial_append("library");
        other.revision = 3;
        other.minimum_writer = 1;
        other.schema = 2;
        let old = Control::initial_append("library");
        let keys: BTreeSet<_> = [record(&old).0, record(&maintenance).0, record(&other).0].into_iter().collect();
        let top = latest(&keys);
        assert_eq!(top.len(), 2, "only the greatest revision is fetched");
        assert!(matches!(publication(effective("p", "library", &[record(&maintenance)])), Ok(Some(why)) if why.contains("moved")));
        assert!(matches!(effective("p", "library", &[record(&maintenance), record(&other)]), Err(Blocked::Conflict(_))));
        assert_eq!(publication(effective("p", "library", &[record(&other)])), Ok(None));
        let mut tampered = record(&other);
        tampered.1.push(b' ');
        assert!(publication(effective("p", "library", &[tampered])).is_err());
        let mut newer = serde_json::to_value(&other).unwrap();
        newer["minimumReader"] = serde_json::json!(crate::cloud_control::READER + 1);
        let bytes = serde_json::to_vec(&newer).unwrap();
        assert!(
            publication(effective("p", "library", &[(format!("p/sync-v2/library/control/{:020}-x.json", 3), bytes)]))
                .unwrap_err()
                .to_string()
                .contains("update")
        );
    }
}
