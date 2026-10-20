//! B2's verified unconditional catalog writes. This is separate from the append
//! validator and never grants mutable heads, descriptors, media or arbitrary keys.
use super::{Batch, Checkpoint, LegacyIntake, MAX_INTAKE, RecordRef};
use crate::assets::sha256;
use crate::cloud_control::Blocked;

pub fn validate(prefix: &str, key: &str, bytes: &[u8]) -> Result<(), Blocked> {
    let invalid = |message: &str| Blocked::Invalid(message.into());
    let reference = RecordRef { key: key.into(), hash: sha256(bytes), size: bytes.len() as u64 };
    let (kind, limit) = if reference.validate_scope(prefix, "changes", 2 << 20).is_ok() {
        ("changes", 2 << 20)
    } else if reference.validate_scope(prefix, "checkpoints", 64 << 20).is_ok() {
        ("checkpoints", 64 << 20)
    } else if reference.validate_scope(prefix, "legacy", MAX_INTAKE).is_ok_and(|epoch| epoch == 1) {
        ("legacy", MAX_INTAKE)
    } else {
        return Err(invalid("unconditional catalog record is not content addressed and scoped"));
    };
    let value: serde_json::Value = serde_json::from_slice(bytes).map_err(|_| invalid("malformed immutable catalog record"))?;
    let maximum = if kind == "changes" { 3 } else { 1 };
    if value.get("version").and_then(serde_json::Value::as_u64).is_some_and(|version| version > maximum) {
        return Err(Blocked::UpdateToRead(format!("immutable catalog {kind} format")));
    }
    match kind {
        "changes" => {
            let batch: Batch = serde_json::from_value(value).map_err(|_| invalid("catalog batch"))?;
            let library = batch.mutations.first().map_or("marker", |m| m.library.as_str());
            batch.validate_at(prefix, library, &reference).map_err(Blocked::Invalid)
        }
        "checkpoints" => {
            let checkpoint: Checkpoint = serde_json::from_value(value).map_err(|_| invalid("catalog checkpoint"))?;
            checkpoint.state.validate().map_err(|e| Blocked::Invalid(e.to_string()))?;
            let epoch = reference.validate_scope(prefix, kind, limit).map_err(Blocked::Invalid)?;
            if checkpoint.version != 1
                || checkpoint.generation == 0
                || !checkpoint.imports.is_empty()
                || checkpoint.covers.validate_scope(prefix, "changes", 2 << 20).map_err(Blocked::Invalid)? != epoch
            {
                return Err(invalid("catalog checkpoint scope"));
            }
            Ok(())
        }
        _ => {
            let intake: LegacyIntake = serde_json::from_value(value).map_err(|_| invalid("unsupported legacy intake fields"))?;
            intake.validate(&intake.library)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cloud_records::object_key;
    use lightcraft_catalog::replication::SyncState;
    #[test]
    fn exact_catalog_records_share_the_native_and_browser_validation_contract() {
        let marker = Batch { version: 3, parent: None, imports: vec![], mutations: vec![], checkpoint_marker: true };
        let bytes = serde_json::to_vec(&marker).unwrap();
        let hash = sha256(&bytes);
        let tip = RecordRef { key: object_key("p", 2, "changes", &hash.0), hash, size: bytes.len() as u64 };
        validate("p", &tip.key, &bytes).unwrap();
        assert!(crate::cloud_append::immutable_metadata(&tip.key, &bytes).is_err(), "append grants stay separate");
        let state = SyncState::new("library".into(), "device".into(), "personal".into()).unwrap().portable();
        let mut checkpoint = Checkpoint { version: 1, generation: 1, covers: tip.clone(), state, imports: Default::default() };
        let bytes = serde_json::to_vec(&checkpoint).unwrap();
        validate("p", &object_key("p", 2, "checkpoints", &sha256(&bytes).0), &bytes).unwrap();
        assert!(validate("p", &object_key("p", 3, "checkpoints", &sha256(&bytes).0), &bytes).is_err());
        checkpoint.generation = 0;
        let bytes = serde_json::to_vec(&checkpoint).unwrap();
        assert!(validate("p", &object_key("p", 2, "checkpoints", &sha256(&bytes).0), &bytes).is_err());
        let intake = LegacyIntake::new("library", Default::default()).unwrap();
        let bytes = serde_json::to_vec(&intake).unwrap();
        validate("p", &object_key("p", 1, "legacy", &sha256(&bytes).0), &bytes).unwrap();
        for key in [
            "p/catalog/head.json".into(),
            "p/catalog/b2-authority.json".into(),
            object_key("p", 1, "authority", &sha256(&bytes).0),
            object_key("p", 2, "legacy", &sha256(&bytes).0),
            object_key("other", 1, "legacy", &sha256(&bytes).0),
            object_key("p", 1, "legacy", &"a".repeat(64)),
        ] {
            assert!(validate("p", &key, &bytes).is_err(), "{key}");
        }
    }
    #[test]
    fn immutable_future_formats_preserve_update_recovery() {
        for (kind, version) in [("changes", 4), ("checkpoints", 2), ("legacy", 2)] {
            let bytes = serde_json::to_vec(&serde_json::json!({"version":version,"futureField":true})).unwrap();
            let key = object_key("p", 1, kind, &sha256(&bytes).0);
            assert!(matches!(validate("p", &key, &bytes), Err(Blocked::UpdateToRead(_))));
        }
    }
}
