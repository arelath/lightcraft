//! Shared portable synchronization projection and atomic local outbox capture.
//! Network adapters consume immutable mutations; catalog persistence remains a local journal.
use crate::{EngineError, Result};
use lightcraft_catalog::replication::{Change, SyncState};
use lightcraft_catalog::{Catalog, ColorLabel, Op, Source};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

type Documents = BTreeMap<String, BTreeMap<String, Value>>;
mod local;
pub(crate) use local::prepare_local;
#[cfg(not(target_arch = "wasm32"))]
pub(crate) use local::{rejoin, resumes_uploads};
pub(crate) type UndoBase = BTreeMap<(String, String), BTreeSet<String>>;
/// Product limits for one entity's bootstrap or one local edit. A remote checkpoint
/// may contain more fields accumulated through smaller edits (up to MAX_FIELDS).
pub const MAX_ENTITY_CHANGES: usize = 4000;
pub const MAX_ENTITY_BYTES: usize = 1 << 20;
#[derive(Default)]
struct Scope {
    photos: BTreeSet<lightcraft_catalog::PhotoId>,
    albums: BTreeSet<lightcraft_catalog::AlbumId>,
    stacks: BTreeSet<lightcraft_catalog::StackId>,
    labels: bool,
    keywords: bool,
}
fn affected(catalog: &Catalog, next: &Catalog, op: &Op) -> Scope {
    let mut scope = Scope::default();
    op.photo_ids(&mut |id| {
        scope.photos.insert(id);
    });
    let mut pending = vec![op];
    while let Some(op) = pending.pop() {
        match op {
            Op::Batch { ops } => pending.extend(ops),
            Op::AddAlbum { album } => {
                scope.albums.insert(album.id);
            }
            Op::RemoveAlbum { id }
            | Op::RenameAlbum { id, .. }
            | Op::MoveAlbum { id, .. }
            | Op::SetAlbumOrder { id, .. }
            | Op::SetAlbumPhotos { id, .. }
            | Op::SetAlbumCover { id, .. }
            | Op::SetAlbumRules { id, .. } => {
                scope.albums.insert(*id);
            }
            Op::AddStack { stack } => {
                scope.stacks.insert(stack.id);
            }
            Op::RemoveStack { id } | Op::SetStack { id, .. } => {
                scope.stacks.insert(*id);
            }
            Op::SetLabelName { .. } => scope.labels = true,
            Op::SetKeyword { .. } => scope.keywords = true,
            Op::SetAsset { id, .. } => {
                scope.photos.extend(
                    catalog
                        .photos()
                        .chain(next.photos())
                        .filter(|photo| photo.source == (Source::Managed { asset_id: id.clone() }))
                        .map(|photo| photo.id),
                );
            }
            _ => {}
        }
    }
    // Adoption/relinking changes which references are portable. Reproject the
    // existing containers too, or their memberships vanish on the first receive.
    let changed_sources: BTreeSet<_> =
        scope.photos.iter().copied().filter(|id| catalog.photo(*id).map(|p| &p.source) != next.photo(*id).map(|p| &p.source)).collect();
    if !changed_sources.is_empty() {
        scope.albums.extend(
            catalog
                .albums()
                .chain(next.albums())
                .filter(|a| a.photos.iter().any(|id| changed_sources.contains(id)) || a.cover.is_some_and(|id| changed_sources.contains(&id)))
                .map(|a| a.id),
        );
        scope.stacks.extend(
            catalog.stacks().chain(next.stacks()).filter(|stack| stack.photos.iter().any(|id| changed_sources.contains(id))).map(|stack| stack.id),
        );
    }
    scope
}
fn error(message: impl Into<String>) -> EngineError {
    EngineError::Other(message.into())
}
fn encode(value: &impl serde::Serialize) -> Result<Value> {
    serde_json::to_value(value).map_err(|e| error(format!("could not encode cloud metadata: {e}")))
}
fn global(catalog: &Catalog, kind: &str, local: u64) -> Result<String> {
    catalog.cloud_identity(&format!("{kind}:{local}")).map(str::to_owned).ok_or_else(|| error(format!("missing synchronized {kind} identity")))
}
fn ensure_id(catalog: &mut Catalog, ops: &mut Vec<Op>, device: &str, kind: &str, local: u64) -> Result<()> {
    let entity = format!("{kind}:{local}");
    if catalog.cloud_identity(&entity).is_some() {
        return Ok(());
    }
    // A device identity is random and durable. Compact IDs are never reused on that device.
    let id = crate::assets::sha256(format!("lightcraft-entity-v1/{device}/{entity}").as_bytes()).0;
    let op = Op::SetCloudIdentity { entity, id: Some(id) };
    catalog.apply(op.clone())?;
    ops.push(op);
    Ok(())
}
fn ensure_ids(catalog: &mut Catalog, state: &SyncState, scope: Option<&Scope>) -> Result<Vec<Op>> {
    let mut keys = Vec::new();
    let photos: Vec<_> = scope.map_or_else(|| catalog.photos().collect(), |s| s.photos.iter().filter_map(|id| catalog.photo(*id)).collect());
    for photo in photos {
        if let Source::Managed { asset_id } = &photo.source
            && catalog.asset(asset_id).is_some_and(|a| a.original.remote.connection == state.connection)
        {
            keys.push(("photo", photo.id.0));
        }
    }
    let albums: Vec<_> = scope.map_or_else(|| catalog.albums().collect(), |s| s.albums.iter().filter_map(|id| catalog.album(*id)).collect());
    let stacks: Vec<_> = scope.map_or_else(|| catalog.stacks().collect(), |s| s.stacks.iter().filter_map(|id| catalog.stack(*id)).collect());
    keys.extend(albums.iter().map(|a| ("album", a.id.0)));
    keys.extend(stacks.iter().map(|s| ("stack", s.id.0)));
    let mut ops = Vec::new();
    for (kind, local) in keys {
        let quick = kind == "album" && catalog.album(lightcraft_catalog::AlbumId(local)).is_some_and(|a| a.quick);
        if quick && catalog.cloud_identity(&format!("album:{local}")).is_none() {
            let id = crate::assets::sha256(format!("lightcraft-quick-v1/{}", state.library).as_bytes()).0;
            let op = Op::SetCloudIdentity { entity: format!("album:{local}"), id: Some(id) };
            catalog.apply(op.clone())?;
            ops.push(op);
        } else {
            ensure_id(catalog, &mut ops, &state.device, kind, local)?;
        }
    }
    Ok(ops)
}

fn mapped_photo(catalog: &Catalog, ids: &Catalog, connection: &str, local: u64) -> Option<String> {
    let photo = catalog.photo(lightcraft_catalog::PhotoId(local))?;
    let Source::Managed { asset_id } = &photo.source else { return None };
    if catalog.asset(asset_id)?.original.remote.connection != connection {
        return None;
    }
    ids.cloud_identity(&format!("photo:{local}")).map(str::to_owned)
}
fn shared_selection(catalog: &Catalog, connection: &str, id: lightcraft_catalog::PhotoId) -> bool {
    if let Some(photo) = catalog.photo(id) {
        let Source::Managed { asset_id } = &photo.source else { return false };
        return catalog.asset(asset_id).is_some_and(|a| a.original.remote.connection == connection);
    }
    // A retained cloud selection can outlive the local photo object.
    catalog.cloud_identity(&format!("photo:{}", id.0)).is_some_and(|global| {
        catalog
            .sync_state()
            .filter(|s| s.connection == connection)
            .is_some_and(|s| s.documents.get(&format!("photo:{global}")).is_some_and(|fields| fields.contains_key("content")))
    })
}
pub(crate) fn device_local_smart_albums(catalog: &Catalog, connection: &str) -> BTreeSet<lightcraft_catalog::AlbumId> {
    let mut excluded = BTreeSet::new();
    let mut dependents = BTreeMap::<_, Vec<_>>::new();
    for album in catalog.albums() {
        let Some(filter) = &album.smart else { continue };
        let references = filter.albums_tested();
        if filter.folder.is_some()
            || filter.library_folder.is_some()
            || filter.sync_status.is_some()
            || portable::has_local_rules(filter.rule_set.as_ref())
            || filter.only.iter().any(|id| !shared_selection(catalog, connection, *id))
            || references.iter().any(|id| catalog.album(*id).is_none() && catalog.cloud_identity(&format!("album:{}", id.0)).is_none())
        {
            excluded.insert(album.id);
        }
        for target in references {
            dependents.entry(target).or_default().push(album.id);
        }
    }
    let mut pending: Vec<_> = excluded.iter().copied().collect();
    while let Some(target) = pending.pop() {
        for dependent in dependents.get(&target).into_iter().flatten() {
            if excluded.insert(*dependent) {
                pending.push(*dependent);
            }
        }
    }
    excluded
}
mod portable;
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn retained_content_asset(value: Value, connection: &str) -> Result<lightcraft_catalog::assets::Asset> {
    portable::decode::<portable::Content>(value)?.asset.rebound(connection).map_err(error)
}
fn project(catalog: &Catalog, ids: &Catalog, state: &SyncState, scope: Option<&Scope>) -> Result<Documents> {
    let mut docs = Documents::new();
    let photos: Vec<_> = scope.map_or_else(|| catalog.photos().collect(), |s| s.photos.iter().filter_map(|id| catalog.photo(*id)).collect());
    for photo in photos {
        let Source::Managed { asset_id } = &photo.source else { continue };
        let Some(asset) = catalog.asset(asset_id).filter(|a| a.original.remote.connection == state.connection) else { continue };
        let fields = portable::PortablePhoto::from_local(photo, asset_id, asset, catalog, ids, &state.connection)?.fields()?;
        docs.insert(format!("photo:{}", global(ids, "photo", photo.id.0)?), fields);
    }
    let albums: Vec<_> = scope.map_or_else(|| catalog.albums().collect(), |s| s.albums.iter().filter_map(|id| catalog.album(*id)).collect());
    let excluded = device_local_smart_albums(catalog, &state.connection);
    for album in albums {
        if excluded.contains(&album.id) {
            continue;
        }
        let album_document = portable::PortableAlbum::from_local(album, catalog, ids, &state.connection)?;
        let mut fields = portable::fields(&album_document)?;
        for id in &album.photos {
            if let Some(reference) = portable::PhotoRef::from_local(catalog, ids, &state.connection, *id) {
                fields.insert(format!("member:{}", reference.id()), json!(true));
            }
        }
        docs.insert(format!("album:{}", global(ids, "album", album.id.0)?), fields);
    }
    let stacks: Vec<_> = scope.map_or_else(|| catalog.stacks().collect(), |s| s.stacks.iter().filter_map(|id| catalog.stack(*id)).collect());
    for stack in stacks {
        let photos: Vec<_> = stack
            .photos
            .iter()
            .filter(|id| catalog.photo(**id).is_some_and(|photo| !photo.deleted))
            .filter_map(|id| portable::PhotoRef::from_local(catalog, ids, &state.connection, *id))
            .collect();
        if photos.len() < 2 {
            continue;
        }
        docs.insert(
            format!("stack:{}", global(ids, "stack", stack.id.0)?),
            portable::fields(&portable::PortableStack { photos, collapsed: stack.collapsed, deleted: false })?,
        );
    }
    let labels: BTreeMap<_, _> = ColorLabel::ALL
        .iter()
        .copied()
        .map(|label| (format!("label.{label:?}"), catalog.custom_label_name(label).map_or(Value::Null, |v| json!(v))))
        .collect();
    if scope.is_none_or(|s| s.labels) {
        docs.insert(format!("library:{}", state.library), labels);
    }
    for keyword in catalog.listed_keywords().filter(|_| scope.is_none_or(|s| s.keywords)) {
        let id = crate::assets::sha256(keyword.path.to_lowercase().as_bytes()).0;
        docs.insert(
            format!("keyword:{id}"),
            BTreeMap::from([("path".into(), json!(keyword.path)), ("info".into(), encode(&keyword.info)?), ("deleted".into(), json!(false))]),
        );
    }
    Ok(docs)
}
fn differences(before: &Documents, after: &Documents, state: &SyncState) -> Vec<Change> {
    let mut changes = Vec::new();
    let entities: BTreeSet<_> = before.keys().chain(after.keys()).cloned().collect();
    for entity in entities {
        let old = before.get(&entity);
        let new = after.get(&entity);
        if new.is_none() {
            changes.push(Change {
                base: state.winning(&entity, "deleted").map(|(stamp, _)| stamp),
                entity,
                field: "deleted".into(),
                value: json!(true),
            });
            continue;
        }
        let fields: BTreeSet<_> = old.into_iter().flat_map(|v| v.keys()).chain(new.into_iter().flat_map(|v| v.keys())).cloned().collect();
        for field in fields {
            let previous = old.and_then(|v| v.get(&field));
            let value = new.and_then(|v| v.get(&field));
            if previous == value {
                continue;
            }
            let value = value.cloned().unwrap_or_else(|| if field.starts_with("member:") { json!(false) } else { Value::Null });
            changes.push(Change { entity: entity.clone(), base: state.winning(&entity, &field).map(|(stamp, _)| stamp), field, value });
        }
    }
    changes
}

/// Prepare before touching the live catalog. Bookkeeping is added to one CRC-framed journal
/// operation; its inverse never enters user Undo, which must create fresh mutation identities.
pub(crate) fn prepare(catalog: &Catalog, op: Op) -> Result<(Op, Op)> {
    let mut next = catalog.clone();
    let inverse = next.apply(op.clone())?;
    let Some(mut state) = catalog.sync_state().cloned() else {
        return Ok((op, inverse));
    };
    let inverse = user_inverse(inverse);
    let mut scope = affected(catalog, &next, &op);
    let was_local = device_local_smart_albums(catalog, &state.connection);
    let now_local = device_local_smart_albums(&next, &state.connection);
    scope.albums.extend(was_local.symmetric_difference(&now_local).copied());
    let mut metadata = ensure_ids(&mut next, &state, Some(&scope))?;
    let before = project(catalog, &next, &state, Some(&scope))?;
    let after = project(&next, &next, &state, Some(&scope))?;
    let mut changes = differences(&before, &after, &state);
    retain_develop_versions(&state, &mut changes, &after);
    if changes.iter().any(|change| change.field == "content") {
        metadata.extend(local::referenced_uploads(catalog, &after, None));
    }
    if enqueue_changes(&mut state, changes)? {
        metadata.push(Op::SetSyncState { state: Some(Box::new(state)) });
    }
    if metadata.is_empty() {
        return Ok((op, inverse));
    }
    let mut ops = vec![op];
    ops.extend(metadata);
    Ok((Op::Batch { ops }, inverse))
}

fn retain_develop_versions(state: &SyncState, changes: &mut Vec<Change>, after: &Documents) {
    // Conflict versions are materialized without an echo. Before a later local recipe
    // causally retires those heads, archive the alternatives in the same outgoing mutation.
    let recipes: Vec<_> = changes.iter().filter(|c| c.field == "develop").map(|c| c.entity.clone()).collect();
    for entity in recipes {
        if state.documents.get(&entity).and_then(|fields| fields.get("develop")).is_some_and(|r| r.0.len() > 1)
            && !changes.iter().any(|c| c.entity == entity && c.field == "versions")
            && let Some(value) = after.get(&entity).and_then(|fields| fields.get("versions"))
        {
            changes.push(Change {
                entity: entity.clone(),
                field: "versions".into(),
                base: state.winning(&entity, "versions").map(|(stamp, _)| stamp),
                value: value.clone(),
            });
        }
    }
}

/// The final local state carrying newly authored IDs, including explicit conflict
/// resolutions that already contain their mutations and therefore bypass capture.
pub(crate) fn authored_state<'a>(catalog: &'a Catalog, op: &'a Op) -> Option<std::borrow::Cow<'a, SyncState>> {
    use std::borrow::Cow;
    let mut pending = vec![op];
    let mut last = None;
    while let Some(op) = pending.pop() {
        match op {
            Op::Batch { ops } => pending.extend(ops.iter().rev()),
            Op::SetSyncState { state: Some(state) } => last = Some(Cow::Borrowed(state.as_ref())),
            Op::SetSyncState { state: None } => last = None,
            Op::PatchSyncState { delta } => {
                let base = last.as_deref().or_else(|| catalog.sync_state())?;
                last = Some(Cow::Owned(delta.updated(base).ok()?));
            }
            _ => {}
        }
    }
    last.filter(|state| {
        state
            .pending
            .iter()
            .any(|(id, mutation)| mutation.revision.device == state.device && catalog.sync_state().is_none_or(|old| !old.pending.contains_key(id)))
    })
}

/// Publish historical authors before revisions whose contexts depend on them.
/// A copied/restored catalog retains its original pending IDs across author rotation.
pub fn publication_order(
    mut pending: Vec<lightcraft_catalog::replication::Mutation>,
    observed: &lightcraft_catalog::replication::Clock,
) -> Result<Vec<lightcraft_catalog::replication::Mutation>> {
    if pending.len() > lightcraft_catalog::replication::MAX_PENDING {
        return Err(lightcraft_catalog::CatalogError::Capacity("pending publication exceeds the bounded outbox".into()).into());
    }
    if observed.len() > lightcraft_catalog::replication::MAX_DEVICES
        || observed.iter().any(|(author, sequence)| !lightcraft_catalog::assets::identifier(author) || author.len() > 96 || *sequence == 0)
    {
        return Err(error("invalid publication frontier; local work retained"));
    }
    for mutation in &pending {
        mutation.validate()?;
    }
    pending.sort_by_key(|mutation| mutation.revision.clone());
    let mut clock = observed.clone();
    let mut ordered = Vec::with_capacity(pending.len());
    while !pending.is_empty() {
        let pass_start = ordered.len();
        let mut waiting = Vec::new();
        for mutation in pending {
            if mutation.context.iter().all(|(author, sequence)| clock.get(author).copied().unwrap_or(0) >= *sequence)
                && clock.get(&mutation.revision.device).copied().unwrap_or(0).checked_add(1) == Some(mutation.revision.sequence)
            {
                clock.insert(mutation.revision.device.clone(), mutation.revision.sequence);
                ordered.push(mutation);
            } else {
                waiting.push(mutation);
            }
        }
        if ordered.len() == pass_start {
            return Err(error(
                "pending publication depends on missing retained causal history; synchronize or restore the complete catalog/outbox; local work retained",
            ));
        }
        pending = waiting;
    }
    Ok(ordered)
}

pub(crate) fn user_inverse(op: Op) -> Op {
    match op {
        Op::SetCloudIdentity { .. } | Op::SetSyncState { .. } | Op::PatchSyncState { .. } | Op::SetLocalSyncBaseline { .. } => {
            Op::Batch { ops: Vec::new() }
        }
        Op::Batch { ops } => Op::Batch { ops: ops.into_iter().map(user_inverse).collect() },
        op => op,
    }
}

pub(crate) fn undo_clock(catalog: &Catalog) -> lightcraft_catalog::replication::Clock {
    catalog.sync_state().map(|state| state.clock.clone()).unwrap_or_default()
}
/// Guard only fields authored by this action, retaining its own revision even
/// when later unrelated actions are folded into the same undo step.
pub(crate) fn undo_base(catalog: &Catalog, before: &lightcraft_catalog::replication::Clock) -> Option<UndoBase> {
    let state = catalog.sync_state()?;
    let changed: BTreeSet<_> = state
        .pending
        .values()
        .filter(|mutation| mutation.revision.sequence > before.get(&mutation.revision.device).copied().unwrap_or(0))
        .flat_map(|mutation| mutation.changes.iter().map(|change| (&change.entity, &change.field)))
        .collect();
    Some(
        changed
            .into_iter()
            .map(|(entity, field)| {
                let revisions = state
                    .documents
                    .get(entity)
                    .and_then(|fields| fields.get(field))
                    .into_iter()
                    .flat_map(|register| register.0.keys().cloned())
                    .collect();
                ((entity.clone(), field.clone()), revisions)
            })
            .collect(),
    )
}
pub(crate) fn guard_undo(catalog: &Catalog, op: &Op, expected: Option<&UndoBase>) -> Result<()> {
    if catalog.local_sync_baseline().is_some() {
        return Ok(());
    }
    let Some(state) = catalog.sync_state() else {
        return Ok(());
    };
    let mut next = catalog.clone();
    next.apply(op.clone())?;
    let scope = affected(catalog, &next, op);
    let before = project(catalog, catalog, state, Some(&scope))?;
    let after = project(&next, catalog, state, Some(&scope))?;
    for change in differences(&before, &after, state) {
        // Recreating a retained tombstone projects its complete object again. Fields
        // identical to every retained head change no shared value and need no new
        // authority. A different saved payload still requires its own revision guard.
        if state
            .documents
            .get(&change.entity)
            .and_then(|fields| fields.get(&change.field))
            .is_some_and(|register| !register.0.is_empty() && register.0.values().all(|value| value == &change.value))
        {
            continue;
        }
        let current: BTreeSet<_> = state
            .documents
            .get(&change.entity)
            .and_then(|fields| fields.get(&change.field))
            .into_iter()
            .flat_map(|register| register.0.keys().cloned())
            .collect();
        if expected.and_then(|base| base.get(&(change.entity.clone(), change.field.clone()))) != Some(&current) {
            return Err(error(format!(
                "cannot undo or redo: {} {} received another revision; inspect its cloud conflict/versions and edit explicitly",
                change.entity, change.field
            )));
        }
    }
    Ok(())
}

pub fn enable(catalog: &Catalog, library: String, device: String, connection: String) -> Result<Op> {
    enable_mode(catalog, library, device, connection, lightcraft_catalog::replication::SyncProtocol::ConditionalHead, String::new())
}
pub fn enable_mode(
    catalog: &Catalog,
    library: String,
    device: String,
    connection: String,
    protocol: lightcraft_catalog::replication::SyncProtocol,
    owner: String,
) -> Result<Op> {
    enroll(catalog, library, device, connection, protocol, owner, false)
}
/// Join existing shared state without authoring concurrent library initialization.
/// Local entity edits retain their frozen IDs; transport must first observe the library.
pub fn join_mode(
    catalog: &Catalog,
    library: String,
    device: String,
    connection: String,
    protocol: lightcraft_catalog::replication::SyncProtocol,
    owner: String,
) -> Result<Op> {
    enroll(catalog, library, device, connection, protocol, owner, true)
}
fn enroll(
    catalog: &Catalog,
    library: String,
    device: String,
    connection: String,
    protocol: lightcraft_catalog::replication::SyncProtocol,
    owner: String,
    joining: bool,
) -> Result<Op> {
    if catalog.sync_state().is_some() {
        return Err(error("synchronization is already enabled; keep its durable device identity"));
    }
    let mut next = catalog.clone();
    let mut state = SyncState::new(library.clone(), device, connection)?;
    state.protocol = protocol;
    state.local_owner = owner;
    state.require_existing = joining;
    let mut ops = ensure_ids(&mut next, &state, None)?;
    let identity = Op::SetCloudIdentity { entity: "library".into(), id: Some(library) };
    next.apply(identity.clone())?;
    ops.push(identity);
    let mut initial = project(&next, &next, &state, None)?;
    if joining {
        initial.remove(&format!("library:{}", state.library));
    } else if let Some(library) = initial.get_mut(&format!("library:{}", state.library)) {
        library.retain(|_, value| !value.is_null());
        library.insert("identity".into(), json!(state.library));
    }
    let changes = differences(&Documents::new(), &initial, &state);
    enqueue_changes(&mut state, changes)?;
    ops.push(Op::SetSyncState { state: Some(Box::new(state)) });
    Ok(Op::Batch { ops })
}

mod materialize;
mod shared_projection;
#[cfg(test)]
mod tests;
pub use materialize::accept_structure;
pub use materialize::incoming;
pub use materialize::incoming_append;
pub use materialize::incoming_checkpoint;
pub use materialize::resolve;
pub use materialize::restore_deleted;
pub use materialize::structural_conflicts;
pub use materialize::{completed_checkpoint, completed_outcome};
#[cfg(not(target_arch = "wasm32"))]
pub(crate) use materialize::{completed_outcome_for_local, materialize_deleted_for_local};

/// Old verified results can lack witnesses for archives installed since they
/// started. Retain newer state and refresh rather than pausing on missing proofs.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn obsolete_completion_needs_proof(catalog: &Catalog, outcome: &crate::cloud_outcome::Outcome, failure: &EngineError) -> bool {
    if !matches!(failure, EngineError::Catalog(lightcraft_catalog::CatalogError::ArchiveRequired(_))) {
        return false;
    }
    let Some(current) = catalog.sync_state() else { return false };
    if let Some(incoming) = outcome.append_progress() {
        current.append.as_ref().is_some_and(|previous| {
            previous.manifest == incoming.manifest
                && incoming.frontiers.iter().all(|(author, sequence)| previous.frontiers.get(author).copied().unwrap_or(0) >= *sequence)
        })
    } else {
        current.protocol.is_head() && outcome.head_position().is_none_or(|position| position.generation() < current.generation)
    }
}

/// Fast owner-thread guard for an already certified conditional-head outcome.
/// Compare every durable portable component without cloning/materializing it.
/// Transport witnesses are temporary; acknowledgement, cursor and generation
/// changes must still pass through the durable receive/ACK transaction.
pub fn unchanged_head(
    catalog: &Catalog,
    remote: &lightcraft_catalog::replication::Portable,
    acknowledged: &BTreeSet<String>,
    cursor: &str,
    generation: u64,
) -> bool {
    let Some(local) = catalog.sync_state() else { return false };
    local.protocol.is_head()
        && local.pending.is_empty()
        && acknowledged.is_empty()
        && generation > 0
        && local.cursor.as_deref() == Some(cursor)
        && local.generation == generation
        && local.version == remote.version
        && local.library == remote.library
        && local.clock == remote.clock
        && local.documents == remote.documents
        && local.received == remote.received
        && local.archives == remote.archives
}

// Keep an entity's bootstrap fields together. Bulk migrations may span bounded mutations;
// the containing catalog journal operation remains atomic.
struct EntityBytes(usize);
impl std::io::Write for EntityBytes {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0 =
            self.0.checked_add(bytes.len()).ok_or_else(|| std::io::Error::other("cloud entity serialized size overflow; local data retained"))?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
fn enqueue_changes(state: &mut SyncState, changes: Vec<Change>) -> Result<bool> {
    let any = !changes.is_empty();
    let mut groups: BTreeMap<String, Vec<Change>> = BTreeMap::new();
    for change in changes {
        groups.entry(change.entity.clone()).or_default().push(change);
    }
    let mut chunk = Vec::new();
    let mut bytes = 0usize;
    for (entity, group) in groups {
        // Measure even rejected input without allocating another oversized buffer.
        let mut measured = EntityBytes(0);
        serde_json::to_writer(&mut measured, &group).map_err(|e| error(e.to_string()))?;
        let size = measured.0;
        if size > MAX_ENTITY_BYTES || group.len() > MAX_ENTITY_CHANGES {
            let name = group
                .iter()
                .find(|c| matches!(c.field.as_str(), "name" | "file_name"))
                .and_then(|c| c.value.as_str())
                .or_else(|| state.winning(&entity, "name").and_then(|(_, v)| v.as_str()));
            let label = name.map(|name| format!(" ({})", name.chars().take(100).collect::<String>())).unwrap_or_default();
            let recovery = if entity.starts_with("album:") {
                "split large albums before enabling sync, or split membership edits into smaller commands; removed synchronized membership fields stay retained"
            } else {
                "shorten large metadata values or split the entity's bulk edit into smaller commands before retrying"
            };
            return Err(lightcraft_catalog::CatalogError::Capacity(format!(
                "cloud entity exceeds the bounded mutation size: {entity}{label}; {} changes (limit {MAX_ENTITY_CHANGES}), {size} bytes (limit {MAX_ENTITY_BYTES} / 1 MiB); {recovery}; current library and outbox retained",
                group.len()
            )).into());
        }
        if bytes.saturating_add(size) > MAX_ENTITY_BYTES || chunk.len().saturating_add(group.len()) > MAX_ENTITY_CHANGES {
            state.enqueue(std::mem::take(&mut chunk))?;
            bytes = 0;
        }
        bytes = bytes.saturating_add(size);
        chunk.extend(group);
    }
    state.enqueue(chunk)?;
    Ok(any)
}

/// A transport's unchanged hint never bypasses comparison with the current owner.
pub fn unchanged_outcome(catalog: &Catalog, outcome: &crate::cloud_outcome::Outcome) -> bool {
    outcome
        .head_position()
        .is_some_and(|position| unchanged_head(catalog, outcome.state(), outcome.acknowledged(), position.cursor(), position.generation()))
}
