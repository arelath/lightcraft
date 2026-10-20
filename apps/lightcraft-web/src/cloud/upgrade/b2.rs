//! One native authority elects an append snapshot. Local pending mutations are
//! never part of the frozen seed; ordinary sync publishes them after rejoin.
use super::*;
use lightcraft_engine::cloud_records::{Checkpoint, CheckpointRef, LegacyIntake};
use lightcraft_engine::cloud_status::Recovery;

pub(super) async fn run(provider: &Provider, local: &SyncState, owner: &str) -> Result<Outcome, CloudFailure> {
    if !provider.connection.supports_b2_authority() {
        return Err(CloudFailure::new(FailureKind::Unsupported, "this destination does not support fenced append upgrades"));
    }
    local.validate().map_err(CloudFailure::from)?;
    for _ in 0..ATTEMPTS {
        if let Some(snapshot) = provider.b2_read(true).await? {
            snapshot.head.validate_scope_typed(&provider.connection.prefix, &local.library)?;
            let control = snapshot.head.control()?;
            control.may_read()?;
            provider.b2_fence(&snapshot.token, &local.library).await?;
            return match control.state {
                State::Active => super::super::protocol::receive_only(provider, local).await,
                State::Upgrading => rebase::run(provider, local, owner).await,
                _ => Err(CloudFailure::integrity("B2 authority is neither active nor upgrading")),
            };
        }
        let initial = match initial(provider, local, owner).await {
            Ok(initial) => initial,
            Err(failure) if failure.remedy == Some(Recovery::Rejoin) || failure.kind == FailureKind::Contention => {
                if provider.b2_read(true).await?.is_some() {
                    continue;
                }
                return Err(failure);
            }
            Err(failure) => return Err(failure),
        };
        match provider.b2_initialize(&initial).await {
            Err(failure) if super::super::protocol::retry_cas(&failure) => {}
            result => result?,
        }
        // Reread even after success or an uncertain response; only the elected
        // checkpoint and current lease may activate. No outcome/ACK yet.
    }
    Err(CloudFailure::new(FailureKind::Contention, "B2 upgrade initialization remained busy; retry to resume it"))
}
async fn initial(provider: &Provider, local: &SyncState, owner: &str) -> Result<Head, CloudFailure> {
    let control = super::super::append::library_control(provider, &local.library).await?;
    control.may_publish()?;
    lightcraft_engine::cloud_control::MigrationSpec::resolve(Plan::Rebase, control.schema, lightcraft_engine::cloud_control::SCHEMA)?;
    let source = super::super::append::receive_only(provider, local).await?;
    if let Some(failure) = source.publication().failure() {
        return Err(failure);
    }
    let progress = source.append_progress().ok_or("verified append progress is unavailable")?;
    let intake = LegacyIntake::new(&local.library, progress.scan.clone())?;
    let first =
        rebase::record(provider, 1, "changes", &Batch { version: 3, parent: None, imports: vec![], mutations: vec![], checkpoint_marker: true })
            .await?;
    let payload = Checkpoint { version: 1, generation: 1, covers: first.clone(), state: source.state().clone(), imports: Default::default() };
    let object = rebase::record(provider, 1, "checkpoints", &payload).await?;
    let checkpoint = CheckpointRef { object, covers: first.clone(), generation: 1 };
    let written: Checkpoint = serde_json::from_slice(&super::super::protocol::object(provider, &checkpoint.object, 64 << 20).await?)
        .map_err(|_| CloudFailure::integrity("malformed frozen append checkpoint"))?;
    if written.state != *source.state() || written.covers != first || written.generation != 1 || written.version != 1 || !written.imports.is_empty() {
        return Err(CloudFailure::integrity("frozen append checkpoint was not verified"));
    }
    let legacy = rebase::record(provider, 1, "legacy", &intake).await?;
    let token = super::super::host::random_identity().map_err(CloudFailure::local)?;
    let control = Control::initial(&local.library)
        .begin(
            &format!("b2-{}", token.get(..16).unwrap_or("upgrade")),
            Plan::Rebase,
            Boundary { tip: first.hash.0.clone(), generation: 1 },
            owner,
            &token,
            now_ms(),
        )?
        .begin_output(&token, now_ms())?;
    let tip = rebase::record(
        provider,
        1,
        "changes",
        &Batch { version: 3, parent: Some(first), imports: vec![], mutations: vec![], checkpoint_marker: true },
    )
    .await?;
    let mut head = Head::next(None, tip, &local.library, Some(checkpoint), control)?;
    head.version = 5;
    head.generation = 2;
    head.legacy_append = Some(legacy);
    head.validate_scope_typed(&provider.connection.prefix, &local.library)?;
    Ok(head)
}
