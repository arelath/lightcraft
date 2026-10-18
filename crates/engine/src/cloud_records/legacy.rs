//! Durable bounded discovery of the retired append root. Its reference travels
//! through the same conditional head as accepted causal data, never a local hint.
use crate::cloud_control::Blocked;
use lightcraft_catalog::replication::{AppendProgress, AppendScan, Portable, SyncProtocol, SyncState};
use serde::{Deserialize, Serialize};

pub const MAX_INTAKE: u64 = 64 << 20;
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LegacyIntake {
    pub version: u32,
    pub library: String,
    /// Original append manifest; fixed even as cursors and head epochs advance.
    pub manifest: String,
    pub scan: AppendScan,
}

impl LegacyIntake {
    pub fn new(library: &str, scan: AppendScan) -> Result<Self, Blocked> {
        let bytes = crate::cloud_append::manifest(library).map_err(|_| Blocked::Invalid("invalid legacy source library".into()))?;
        let value = Self { version: 1, library: library.into(), manifest: crate::assets::sha256(&bytes).0, scan };
        value.validate(library)?;
        Ok(value)
    }
    pub fn validate(&self, library: &str) -> Result<(), Blocked> {
        if self.version > 1 {
            return Err(Blocked::UpdateToRead("legacy append recovery needs a newer LightCraft".into()));
        }
        if self.version != 1 {
            return Err(Blocked::Invalid("invalid legacy intake version".into()));
        }
        if self.library != library || !lightcraft_catalog::assets::identifier(library) {
            return Err(Blocked::Invalid("legacy intake belongs to another library".into()));
        }
        let bytes = crate::cloud_append::manifest(library).map_err(|_| Blocked::Invalid("invalid legacy source library".into()))?;
        if crate::assets::sha256(&bytes).0 != self.manifest {
            return Err(Blocked::Invalid("legacy intake manifest does not match its library".into()));
        }
        self.scan.validate().map_err(|_| Blocked::Invalid("invalid legacy discovery cursor".into()))
    }
    pub fn decode(bytes: &[u8], library: &str) -> Result<Self, Blocked> {
        if bytes.len() as u64 > MAX_INTAKE {
            return Err(Blocked::Invalid("legacy intake exceeds its bounded length".into()));
        }
        let value: serde_json::Value = serde_json::from_slice(bytes).map_err(|_| Blocked::Invalid("malformed legacy intake".into()))?;
        match value.get("version").and_then(serde_json::Value::as_u64) {
            Some(1) => {}
            Some(version) if version > 1 => return Err(Blocked::UpdateToRead("legacy append recovery needs a newer LightCraft".into())),
            _ => return Err(Blocked::Invalid("invalid legacy intake version".into())),
        }
        let intake: Self = serde_json::from_value(value).map_err(|_| Blocked::Invalid("unsupported legacy intake fields".into()))?;
        intake.validate(library)?;
        Ok(intake)
    }
    /// A temporary read-only append view starts from all currently committed
    /// causal knowledge. It never authors a mutation or changes pending IDs.
    pub fn probe(&self, remote: &Portable, connection: &str) -> Result<SyncState, Blocked> {
        self.validate(&remote.library)?;
        let mut local = SyncState::restore_durable(remote, "recovery".into(), connection.into()).map_err(|e| Blocked::Invalid(e.to_string()))?;
        local.protocol = SyncProtocol::AppendLog;
        local.require_existing = true;
        local.append = Some(Box::new(AppendProgress {
            version: 1,
            manifest: self.manifest.clone(),
            frontiers: lightcraft_catalog::replication::publication_prefixes(remote).map_err(|e| Blocked::Invalid(e.to_string()))?,
            remote: remote.clone(),
            gaps: Default::default(),
            scan: self.scan.clone(),
        }));
        local.validate().map_err(|e| Blocked::Invalid(e.to_string()))?;
        Ok(local)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lightcraft_catalog::replication::{AppendAudit, Change};
    use serde_json::json;

    #[test]
    fn intake_round_trip_preserves_scan_and_original_mutation_proofs() {
        let mut original = SyncState::new("library".into(), "camera".into(), "connection".into()).unwrap();
        let mutation = original
            .enqueue(vec![Change { entity: "library:library".into(), field: "name".into(), base: None, value: json!("Photos") }])
            .unwrap()
            .unwrap();
        let scan = AppendScan { audit: AppendAudit { round: 7, complete: true, after: None }, ..Default::default() };
        let intake = LegacyIntake::new("library", scan.clone()).unwrap();
        let bytes = serde_json::to_vec(&intake).unwrap();
        let back = LegacyIntake::decode(&bytes, "library").unwrap();
        assert_eq!(back, intake);
        let probe = back.probe(&original.portable(), "connection").unwrap();
        probe.validate().unwrap();
        assert!(probe.pending.is_empty());
        assert!(probe.require_existing);
        assert_eq!(probe.protocol, SyncProtocol::AppendLog);
        assert_eq!(probe.append.as_ref().unwrap().scan, scan);
        assert_eq!(probe.portable(), original.portable());
        assert_eq!(probe.received[&mutation.revision.id()].digest, mutation.digest().unwrap());
        assert_eq!(original.pending[&mutation.revision.id()], mutation);
    }
    #[test]
    fn malformed_or_cross_library_intake_fails_closed() {
        let intake = LegacyIntake::new("library", Default::default()).unwrap();
        let bytes = serde_json::to_vec(&intake).unwrap();
        assert!(LegacyIntake::decode(&bytes, "another").is_err());
        for bad in [b"null".as_slice(), b"{}", b"{\"version\":0}", b"{\"version\":\"1\"}"] {
            assert!(matches!(LegacyIntake::decode(bad, "library"), Err(Blocked::Invalid(_))));
        }
        let mut bad = intake.clone();
        bad.manifest = "0".repeat(64);
        assert!(bad.validate("library").is_err());
        bad = intake;
        bad.scan.audit = AppendAudit { round: 1, complete: false, after: Some("../other/ops/a.json".into()) };
        assert!(bad.validate("library").is_err());
        assert!(matches!(LegacyIntake::decode(b"{\"version\":2,\"unknown\":true}", "library"), Err(Blocked::UpdateToRead(_))));
    }
    #[test]
    fn legacy_reference_stays_in_original_root_across_head_epochs() {
        use crate::cloud_control::Control;
        use crate::cloud_records::{Head, RecordRef, object_key};
        use lightcraft_catalog::assets::BlobId;
        let hash = "a".repeat(64);
        let reference = |epoch, kind| RecordRef { key: object_key("p", epoch, kind, &hash), hash: BlobId(hash.clone()), size: 10 };
        let mut head = Head::next(None, reference(1, "changes"), "library", None, Control::initial("library")).unwrap();
        assert_eq!(head.version, 4, "supporting head v5 must not promote ordinary writers");
        head.version = 5;
        head.legacy_append = Some(reference(1, "legacy"));
        head.validate_scope_typed("p", "library").unwrap();
        let mut control = head.control().unwrap();
        control.epoch = 4;
        control.active_metadata_root = crate::cloud_control::epoch_root(4);
        let mut next = Head::next(Some(&head), reference(4, "changes"), "library", None, control).unwrap();
        assert_eq!(next.version, 5);
        assert_eq!(next.legacy_append, head.legacy_append);
        next.validate_scope_typed("p", "library").unwrap();
        next.legacy_append = Some(reference(4, "legacy"));
        assert!(next.validate_scope_typed("p", "library").is_err());
        next = head;
        next.version = 4;
        assert!(next.validate_scope_typed("p", "library").is_err());
    }
}
