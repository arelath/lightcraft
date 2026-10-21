//! The B2 head adapter. Immutable heads are visible only through bucket revision
//! CAS. Initialization is restricted to an explicit, frozen append upgrade.
use super::{MAX_AUTHORITY, Observed, Scope, invalid, validate_head};
use crate::assets::s3::S3Connection;
use crate::assets::{AssetError, Result, sha256};
use crate::cloud_control::{Exposure, Plan, SCHEMA, State};
use crate::cloud_records::{Head, LegacyIntake, RecordRef};
use std::sync::atomic::AtomicBool;

impl Scope {
    fn descriptor_key(&self) -> String {
        format!("{}/b2-authority.json", self.root)
    }
    fn descriptor(&self) -> Result<Vec<u8>> {
        serde_json::to_vec(&serde_json::json!({"version":1,"protocol":"b2-bucket-head-v1","binding":self.binding}))
            .map_err(|_| invalid("descriptor encoding"))
    }
    fn verify_descriptor(&self, bytes: &[u8]) -> Result<()> {
        let value: serde_json::Value = serde_json::from_slice(bytes).map_err(|_| invalid("authority descriptor encoding"))?;
        if value.get("version").and_then(serde_json::Value::as_u64).is_some_and(|version| version > 1) {
            return Err(AssetError::Blocked(crate::cloud_control::Blocked::UpdateToRead("Backblaze authority descriptor format".into())));
        }
        if bytes != self.descriptor()? {
            return Err(invalid("authority descriptor binding or fields changed"));
        }
        Ok(())
    }
}
impl Observed {
    fn head_token(&self) -> Result<String> {
        let hash = &self.head().ok_or_else(|| invalid("missing head concurrency token"))?.hash.0;
        Ok(format!("b2:{}:{hash}", self.revision()))
    }
}
impl S3Connection {
    /// Check the immutable marker before requesting native bucket permissions.
    /// Its absence keeps existing append-only keys and ordinary S3 heads usable.
    pub fn b2_head_enabled(&self, cancel: &AtomicBool) -> Result<bool> {
        if self.prefix.is_empty() || self.endpoint.as_deref().and_then(crate::cloud_setup::b2_runtime_endpoint).is_none() {
            return Ok(false);
        }
        let scope = Scope::new(self)?;
        let bytes = scope.descriptor()?;
        let reference = RecordRef { key: scope.descriptor_key(), size: bytes.len() as u64, hash: sha256(&bytes) };
        match self.metadata_record(&reference, 4096, cancel, |bytes| scope.verify_descriptor(bytes))? {
            None => Ok(false),
            Some(_) => Ok(true),
        }
    }
    fn b2_observed_head(&self, observed: &Observed, cancel: &AtomicBool) -> Result<Head> {
        let reference =
            observed.head().ok_or_else(|| AssetError::Blocked(crate::cloud_control::Blocked::FinishUpgrade("B2 initialization".into())))?;
        let head = self
            .metadata_record(reference, MAX_AUTHORITY as usize, cancel, |bytes| {
                let head = Head::decode(bytes).map_err(AssetError::Blocked)?;
                validate_head(self, &head)?;
                Ok(head)
            })?
            .ok_or(AssetError::Missing)?
            .value;
        if head.control.as_ref().is_none_or(|c| c.state != State::Deleted) {
            if head.control.as_ref().is_some_and(|c| c.state == State::Active && c.epoch < 2) {
                return Err(invalid("active authority precedes the upgraded epoch"));
            }
            self.b2_legacy_intake(&head, cancel)?;
        }
        Ok(head)
    }
    /// Returns the verified physical head as well as its concurrency token, so
    /// erasure can retain the exact terminal object. A marker without a pointer
    /// is an interrupted upgrade, never permission to create a fresh library.
    pub fn b2_read_head(&self, cancel: &AtomicBool) -> Result<(Head, String, RecordRef)> {
        self.b2_try_read_head(cancel)?.ok_or_else(|| invalid("authority descriptor is absent"))
    }
    /// `None` means this destination still uses its original protocol. Once the
    /// descriptor exists, any missing or invalid authority is an error.
    pub fn b2_try_read_head(&self, cancel: &AtomicBool) -> Result<Option<(Head, String, RecordRef)>> {
        if !self.b2_head_enabled(cancel)? {
            return Ok(None);
        }
        let observed = self.b2_read_authority(cancel)?;
        let head = self.b2_observed_head(&observed, cancel)?;
        let reference = observed.head().cloned().ok_or_else(|| invalid("head disappeared"))?;
        Ok(Some((head, observed.head_token()?, reference)))
    }
    /// B2 rejects S3 create-if-absent. Only exact content-addressed catalog
    /// records may use verified unconditional PUT; mutable heads never do.
    pub fn b2_put_catalog_immutable(&self, key: &str, bytes: &[u8], cancel: &AtomicBool) -> Result<()> {
        Scope::new(self)?;
        validate_catalog_record(self, key, bytes)?;
        self.put_verified_metadata(key, bytes, cancel)
    }
    /// The exact original manifest remains a permanent barrier to older head
    /// writers. Only a verified intake authorizes bypassing that barrier.
    pub fn b2_legacy_intake(&self, head: &Head, cancel: &AtomicBool) -> Result<LegacyIntake> {
        let library = head.library.as_deref().ok_or_else(|| invalid("head library"))?;
        let reference = head.legacy_append.as_ref().ok_or_else(|| invalid("head has no legacy intake"))?;
        if reference.validate_scope(self.prefix.trim_end_matches('/'), "legacy", crate::cloud_records::MAX_INTAKE).map_err(AssetError::Invalid)? != 1
        {
            return Err(invalid("legacy intake namespace"));
        }
        let intake = self
            .metadata_record(reference, crate::cloud_records::MAX_INTAKE as usize, cancel, |bytes| {
                LegacyIntake::decode(bytes, library).map_err(AssetError::Blocked)
            })?
            .ok_or(AssetError::Missing)?
            .value;
        let key = crate::cloud_append::manifest_key(&self.prefix, library)?;
        let expected = crate::cloud_append::manifest(library)?;
        let manifest = RecordRef { key, size: expected.len() as u64, hash: sha256(&expected) };
        if self
            .metadata_record(
                &manifest,
                16 << 10,
                cancel,
                |bytes| {
                    if bytes == expected { Ok(()) } else { Err(invalid("original append manifest changed")) }
                },
            )?
            .is_none()
        {
            return Err(invalid("original append manifest is absent or changed"));
        }
        if let Some(operation) = &self.metadata.operation {
            *operation.legacy_pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner) =
                Some((reference.clone(), !intake.scan.audit.complete));
        }
        Ok(intake)
    }
    pub(crate) fn b2_legacy_pending(&self, head: &Head, cancel: &AtomicBool) -> Result<bool> {
        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            return Err(AssetError::Cancelled);
        }
        let Some(reference) = &head.legacy_append else { return Ok(false) };
        if let Some(operation) = &self.metadata.operation
            && let Some((saved, pending)) = &*operation.legacy_pending.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
            && saved == reference
        {
            return Ok(*pending);
        }
        Ok(!self.b2_legacy_intake(head, cancel)?.scan.audit.complete)
    }
    /// Install the first authority only for an explicitly frozen append source.
    /// No active or unrelated raw S3 head may be bridged or overwritten.
    pub fn b2_initialize_head(&self, next: &Head, cancel: &AtomicBool) -> Result<RecordRef> {
        self.authority_context().b2_initialize_in_scope(next, cancel)
    }
    fn b2_initialize_in_scope(&self, next: &Head, cancel: &AtomicBool) -> Result<RecordRef> {
        let _write = self.authority_write();
        validate_initial(self, next)?;
        // Validate descriptor and native permissions before staging any output.
        self.b2_head_enabled(cancel)?;
        let observed = self.b2_read_authority(cancel)?;
        observed.require_writable()?;
        if observed.head().is_some() {
            return Err(AssetError::Conflict);
        }
        let intake = self.b2_legacy_intake(next, cancel)?;
        let library = next.library.as_deref().ok_or_else(|| invalid("head library"))?;
        let source = crate::cloud_append::native::library_control(self, library, cancel)?;
        if let Some(blocked) = crate::cloud_append::control::publication(Ok(source.clone())).map_err(AssetError::Blocked)? {
            return Err(AssetError::Blocked(blocked));
        }
        crate::cloud_control::MigrationSpec::resolve(Plan::Rebase, source.schema, SCHEMA).map_err(AssetError::Blocked)?;
        let raw_key = crate::assets::keys::in_library(&self.prefix, "catalog/head.json")?;
        if let Some(raw) = self.get_small(&raw_key, MAX_AUTHORITY as usize, cancel)? {
            let old = Head::decode(&raw.bytes).map_err(AssetError::Blocked)?;
            old.validate_scope_typed(&self.prefix, library).map_err(AssetError::Blocked)?;
            let control = old.control().map_err(AssetError::Invalid)?;
            if control.state != State::Retired
                || control.successor.as_ref().is_none_or(|s| s.protocol != "append-log-v1" || s.epoch != 1 || s.manifest != intake.manifest)
            {
                return Err(invalid("raw head is not retired to this exact append library"));
            }
        }
        let checkpoint = next.checkpoint.as_ref().ok_or_else(|| invalid("initial source checkpoint"))?;
        let source = crate::cloud_replication::read_checkpoint(self, checkpoint, cancel)?;
        if source.state.library != library {
            return Err(invalid("initial checkpoint library"));
        }
        let first = crate::cloud_publication::read_batch(self, &checkpoint.covers, cancel)?;
        let maintenance = crate::cloud_publication::read_batch(self, &next.tip, cancel)?;
        if !first.checkpoint_marker
            || first.parent.is_some()
            || !first.mutations.is_empty()
            || !maintenance.checkpoint_marker
            || maintenance.parent.as_ref() != Some(&checkpoint.covers)
            || !maintenance.mutations.is_empty()
        {
            return Err(invalid("initial source boundary is not frozen"));
        }
        let scope = Scope::new(self)?;
        let bytes = scope.descriptor()?;
        let descriptor = RecordRef { key: scope.descriptor_key(), size: bytes.len() as u64, hash: sha256(&bytes) };
        self.put_verified_metadata(&descriptor.key, &bytes, cancel)?;
        self.remember_verified_metadata(&descriptor, &bytes, cancel)?;
        let reference = self.b2_stage_head(next, cancel)?;
        self.b2_compare_exchange_authority(&observed, &reference, cancel)?;
        Ok(reference)
    }
    /// Replace only the exact observed authority. Scope, generation, terminal
    /// state and legacy binding are checked before uploading a candidate head.
    pub fn b2_write_head(&self, expected: &str, next: &Head, cancel: &AtomicBool) -> Result<RecordRef> {
        self.authority_context().b2_write_in_scope(expected, next, cancel)
    }
    fn b2_write_in_scope(&self, expected: &str, next: &Head, cancel: &AtomicBool) -> Result<RecordRef> {
        let _write = self.authority_write();
        if !self.b2_head_enabled(cancel)? {
            return Err(invalid("authority descriptor is absent"));
        }
        let observed = self.b2_read_authority(cancel)?;
        observed.require_writable()?;
        if observed.head_token()? != expected {
            return Err(AssetError::Conflict);
        }
        let previous = self.b2_observed_head(&observed, cancel)?;
        validate_transition(self, &previous, next)?;
        let old_control = previous.control().map_err(AssetError::Invalid)?;
        let new_control = next.control().map_err(AssetError::Invalid)?;
        if old_control != new_control && new_control.state != State::Deleted {
            let batch = crate::cloud_publication::read_batch(self, &next.tip, cancel)?;
            if !batch.checkpoint_marker
                || !batch.mutations.is_empty()
                || (old_control.epoch == new_control.epoch && batch.parent.as_ref() != Some(&previous.tip))
            {
                return Err(invalid("maintenance transition has non-marker changes"));
            }
        }
        if next.control.as_ref().is_none_or(|c| c.state != State::Deleted) {
            let intake = self.b2_legacy_intake(next, cancel)?;
            if old_control.state == State::Upgrading && new_control.state == State::Active && new_control.epoch > old_control.epoch {
                let source = crate::cloud_append::native::library_control(self, &intake.library, cancel)?;
                require_source_fence(&source, &intake, old_control.migration.as_ref().map_or("upgrade", |m| m.id.as_str()))?;
            }
        }
        let reference = self.b2_stage_head(next, cancel)?;
        self.b2_compare_exchange_authority(&observed, &reference, cancel)?;
        Ok(reference)
    }
}

fn require_source_fence(source: &crate::cloud_control::Control, intake: &LegacyIntake, migration: &str) -> Result<()> {
    source.may_read().map_err(AssetError::Blocked)?;
    if source.library != intake.library
        || source.epoch != 1
        || source.schema != SCHEMA
        || source.active_metadata_root != crate::cloud_control::append_root(&intake.library, 1)
    {
        return Err(invalid("original append source identity or namespace changed"));
    }
    if source.state != State::Retired {
        return Err(AssetError::Blocked(crate::cloud_control::Blocked::FinishUpgrade(migration.into())));
    }
    if source.successor.as_ref().is_none_or(|s| s.protocol != "conditional-head-v1" || s.epoch != 2 || s.manifest != intake.manifest) {
        return Err(invalid("original append source is retired to a different successor"));
    }
    Ok(())
}

fn validate_catalog_record(connection: &S3Connection, key: &str, bytes: &[u8]) -> Result<()> {
    crate::cloud_records::b2_catalog::validate(connection.prefix.trim_end_matches('/'), key, bytes).map_err(AssetError::Blocked)
}

fn validate_initial(connection: &S3Connection, head: &Head) -> Result<()> {
    validate_head(connection, head)?;
    let control = head.control().map_err(AssetError::Invalid)?;
    let checkpoint = head.checkpoint.as_ref().ok_or_else(|| invalid("initial source checkpoint"))?;
    let migration = control.migration.as_ref().ok_or_else(|| invalid("initial maintenance lease"))?;
    control.migration_spec().map_err(AssetError::Blocked)?;
    if head.legacy_append.is_none()
        || head.generation != 2
        || checkpoint.generation != 1
        || control.state != State::Upgrading
        || control.epoch != 1
        || control.schema != SCHEMA
        || migration.plan != Plan::Rebase
        || migration.exposure != Exposure::Publishing
        || migration.target_epoch != 2
        || migration.source.generation != 1
        || migration.source.tip != checkpoint.covers.hash.0
    {
        return Err(invalid("initial head is not an exposed append upgrade"));
    }
    Ok(())
}
fn validate_transition(connection: &S3Connection, previous: &Head, next: &Head) -> Result<()> {
    validate_head(connection, previous)?;
    validate_head(connection, next)?;
    if previous.library != next.library {
        return Err(invalid("head library changed"));
    }
    let old = previous.control().map_err(AssetError::Invalid)?;
    let new = next.control().map_err(AssetError::Invalid)?;
    if old.state == State::Deleted {
        return Err(AssetError::Blocked(crate::cloud_control::Blocked::Deleted));
    }
    if new.state == State::Deleted {
        if old.delete().map_err(AssetError::Blocked)? != new {
            return Err(invalid("deletion control changed"));
        }
        return Ok(());
    }
    if old == new {
        old.may_publish().map_err(AssetError::Blocked)?;
    }
    if old.state == State::Upgrading {
        old.migration_spec().map_err(AssetError::Blocked)?;
    }
    if previous.legacy_append.is_none() || next.legacy_append.is_none() || new.epoch < old.epoch || new.revision < old.revision {
        return Err(invalid("head lost its legacy binding or control advanced backwards"));
    }
    if !allowed_control(previous, &old, &new) {
        return Err(invalid("control is not a supported fenced transition"));
    }
    if old != new && previous.legacy_append != next.legacy_append {
        return Err(invalid("maintenance changed the legacy intake"));
    }
    // A scan-only CAS may retain its causal position. Nothing else may change.
    if next.generation == previous.generation {
        if old.state != State::Active || new != old || next.tip != previous.tip || next.checkpoint != previous.checkpoint {
            return Err(invalid("same-generation head changed causal data or control"));
        }
    } else if next.generation < previous.generation {
        return Err(invalid("head generation went backwards"));
    }
    Ok(())
}
fn allowed_control(previous: &Head, old: &crate::cloud_control::Control, new: &crate::cloud_control::Control) -> bool {
    use crate::cloud_control::{Boundary, LEASE_MS};
    if old == new {
        return old.state == State::Active;
    }
    let now = new.migration.as_ref().map_or(0, |m| m.lease_until_ms.saturating_sub(LEASE_MS));
    if old.state == State::Active {
        let Some(m) = &new.migration else { return false };
        return m.plan == Plan::Rebase
            && old
                .begin(&m.id, m.plan, Boundary { tip: previous.tip.hash.0.clone(), generation: previous.generation }, &m.owner, &m.token, now)
                .is_ok_and(|c| &c == new);
    }
    let Some(m) = &old.migration else { return false };
    if let Some(next) = &new.migration {
        return old.renew(&m.token, now).is_ok_and(|c| &c == new)
            || old.begin_output(&m.token, now).is_ok_and(|c| &c == new)
            || old.takeover(&next.owner, &next.token, now, next.owner == m.owner).is_ok_and(|c| &c == new);
    }
    old.cancel(&m.token).is_ok_and(|c| &c == new) || old.activate(&m.token).is_ok_and(|c| &c == new)
}

#[cfg(test)]
mod tests;
