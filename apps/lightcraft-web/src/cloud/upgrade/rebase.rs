//! Frozen-state identity rebase with the same lease and activation fence as native.
use super::*;
use lightcraft_engine::cloud_outcome::{HeadPosition, Publication};

fn owned<'a>(head: &'a Head, token: &str) -> Result<&'a lightcraft_engine::cloud_control::Migration, CloudFailure> {
    head.control.as_ref().and_then(|c| c.migration.as_ref()).filter(|m| m.token == token).ok_or_else(taken_over)
}
pub(super) async fn run(provider: &Provider, local: &SyncState, owner: &str) -> Result<Outcome, CloudFailure> {
    let library = &local.library;
    let token = super::super::host::random_identity().map_err(CloudFailure::local)?;
    let id = format!("rebase-{}", token.get(..16).unwrap_or("upgrade"));
    let acquired = transition(provider, library, |head, c| {
        Ok(match &c.migration {
            None => c.begin(&id, Plan::Rebase, Boundary { tip: head.tip.hash.0.clone(), generation: head.generation }, owner, &token, now_ms()),
            Some(m) if m.plan != Plan::Rebase => Err(Blocked::Maintenance { migration: m.id.clone(), plan: m.plan, owner: m.owner.clone() }),
            Some(m) => c.takeover(owner, &token, now_ms(), m.owner == owner),
        }?)
    })
    .await
    .map_err(Failure::message)?;
    if acquired.legacy_append.is_some() {
        let (_, expected, _) = current(provider, library).await?;
        provider.b2_fence(&expected, library).await?;
    }
    let ownership = RefCell::new(Lease::new(acquired.control()?)?);
    lease::run(provider, library, &ownership, async {
        let migration = owned(&acquired, &token)?.clone();
        let spec = acquired.control()?.migration_spec()?;
        match spec.converter() {
            lightcraft_engine::cloud_control::Converter::Identity => {
                lightcraft_engine::cloud_control::translate_mutation(spec.source_schema(), spec.target_schema())?
            }
        }
        let mut proofs = Default::default();
        let mut source = super::super::protocol::read(provider, library, false, &mut proofs).await?;
        let publishing = transition(provider, library, |head, c| {
            owned(head, &token)?;
            Ok(c.begin_output(&token, now_ms())?)
        })
        .await
        .map_err(Failure::message)?;
        let epoch = migration.target_epoch;
        let first = Batch { version: 3, parent: None, imports: vec![], mutations: vec![], checkpoint_marker: true };
        let first = record(provider, epoch, "changes", &first).await?;
        let base = publishing.generation.checked_add(1).ok_or("generation exhausted")?;
        let checkpoint = super::super::protocol::checkpoint(provider, &mut source.state, first.clone(), base, epoch).await?;
        let written: lightcraft_engine::cloud_records::Checkpoint =
            serde_json::from_slice(&super::super::protocol::object(provider, &checkpoint.object, 64 << 20).await?)
                .map_err(|_| CloudFailure::integrity("malformed replacement checkpoint"))?;
        if written.version != 1
            || written.generation != base
            || written.covers != first
            || !written.imports.is_empty()
            || written.state != source.state.portable()
        {
            return Err(CloudFailure::integrity("replacement checkpoint does not reproduce the frozen source"));
        }
        ownership.borrow_mut().expect_completion(publishing.control()?.activate(&token)?)?;
        let mut activated = false;
        for _ in 0..ATTEMPTS {
            let (head, expected, control) = current(provider, library).await?;
            if control.state == State::Active && control.epoch == epoch && head.checkpoint.as_ref().map(|c| &c.object) == Some(&checkpoint.object) {
                activated = true;
                break;
            }
            owned(&head, &token)?;
            let wanted = control.activate(&token)?;
            let tip = record(
                provider,
                epoch,
                "changes",
                &Batch { version: 3, parent: Some(first.clone()), imports: vec![], mutations: vec![], checkpoint_marker: true },
            )
            .await?;
            let mut next = Head::next(Some(&head), tip, library, Some(checkpoint.clone()), wanted)?;
            next.generation = head.generation.max(base).checked_add(1).ok_or("generation exhausted")?;
            match super::super::protocol::write_head(provider, Some(&expected), &next).await {
                Err(failure) if super::super::protocol::retry_cas(&failure) => {}
                result => result?,
            }
        }
        if !activated {
            return Err(CloudFailure::connectivity("activation was not confirmed; reread before retrying"));
        }
        let confirmed = super::super::protocol::read(provider, library, false, &mut proofs).await?;
        let (head, _) = confirmed.head.ok_or("activated head disappeared")?;
        let control = head.control()?;
        control.may_read()?;
        if control.state != State::Active || control.epoch != epoch || head.checkpoint.as_ref().map(|c| &c.object) != Some(&checkpoint.object) {
            return Err(taken_over());
        }
        let capability = Publication::head(Some(&head), library)?;
        let state = super::super::history::certify(provider, local, &confirmed.state.portable(), &mut proofs).await?;
        let acknowledged = local.pending.keys().filter(|id| confirmed.state.received.contains_key(*id)).cloned().collect();
        Ok(Outcome::head(state, HeadPosition::new(head.tip.hash.0, head.generation)?, acknowledged).with_publication(capability))
    })
    .await
}
pub(super) async fn record(provider: &Provider, epoch: u64, kind: &str, value: &impl serde::Serialize) -> Result<RecordRef, CloudFailure> {
    let bytes = super::super::protocol::encode(value, 64 << 20)?;
    let hash = sha256(&bytes);
    let reference = RecordRef { key: object_key(&provider.connection.prefix, epoch, kind, &hash.0), hash, size: bytes.len() as u64 };
    immutable(provider, &reference.key, &bytes).await?;
    Ok(reference)
}
