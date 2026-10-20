//! Read provisional old-root input, commit it through B2 CAS, then reread before
//! exposing any outcome or acknowledgement to the browser catalog owner.
use super::{Provider, protocol};
use lightcraft_engine::assets::sha256;
use lightcraft_engine::catalog::replication::SyncState;
use lightcraft_engine::cloud_records::{Batch, Head, LegacyIntake, RecordRef, object_key};
use lightcraft_engine::cloud_status::CloudFailure;

pub(super) async fn intake(provider: &Provider, head: &Head) -> Result<LegacyIntake, CloudFailure> {
    let library = head.library.as_deref().ok_or("legacy authority has no library identity")?;
    let reference = head.legacy_append.as_ref().ok_or("authority has no legacy source")?;
    if reference.validate_scope(&provider.connection.prefix, "legacy", lightcraft_engine::cloud_records::MAX_INTAKE)? != 1 {
        return Err(CloudFailure::integrity("legacy source moved out of its original namespace"));
    }
    let bytes = protocol::object(provider, reference, lightcraft_engine::cloud_records::MAX_INTAKE as usize).await?;
    let intake = LegacyIntake::decode(&bytes, library)?;
    let key = lightcraft_engine::cloud_append::manifest_key(&provider.connection.prefix, library).map_err(CloudFailure::from)?;
    let expected = lightcraft_engine::cloud_append::manifest(library).map_err(CloudFailure::from)?;
    let read = provider.request("GET", &key, None, None, None, 16 << 10).await?;
    if read.status != 200 || read.bytes != expected {
        return Err(CloudFailure::integrity("original append manifest is absent or changed"));
    }
    Ok(intake)
}
pub(super) async fn admit(provider: &Provider, remote: &protocol::Remote, audit: bool) -> Result<bool, CloudFailure> {
    let Some((head, token)) = &remote.head else { return Ok(false) };
    if !token.starts_with("b2:") {
        return Err(CloudFailure::integrity("legacy input has no B2 commit authority"));
    }
    let old = intake(provider, head).await?;
    let committed = remote.state.portable();
    let received = super::append::receive_legacy(provider, head, &committed, audit).await?;
    let changed = committed != received.state;
    if !changed && old == received.intake {
        return Ok(false);
    }
    let bytes = protocol::encode(&received.intake, lightcraft_engine::cloud_records::MAX_INTAKE as usize)?;
    let hash = sha256(&bytes);
    let reference = RecordRef { key: object_key(&provider.connection.prefix, 1, "legacy", &hash.0), size: bytes.len() as u64, hash };
    provider.put_catalog(&reference.key, &bytes).await?;
    let mut next = if changed {
        let control = head.control()?;
        let mut state =
            SyncState::restore_durable(&received.state, "legacy-recovery".into(), provider.connection.id.clone()).map_err(CloudFailure::from)?;
        let checkpoint = protocol::checkpoint(provider, &mut state, head.tip.clone(), head.generation, control.epoch).await?;
        let bytes = protocol::encode(
            &Batch { version: 3, parent: Some(head.tip.clone()), imports: vec![], mutations: vec![], checkpoint_marker: true },
            2 << 20,
        )?;
        let hash = sha256(&bytes);
        let tip = RecordRef { key: object_key(&provider.connection.prefix, control.epoch, "changes", &hash.0), size: bytes.len() as u64, hash };
        provider.put_catalog(&tip.key, &bytes).await?;
        Head::next(Some(head), tip, &remote.state.library, Some(checkpoint), control)?
    } else {
        head.clone()
    };
    next.legacy_append = Some(reference);
    protocol::write_head(provider, Some(token), &next).await?;
    Ok(true)
}
