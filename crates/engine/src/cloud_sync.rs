//! Shared portable synchronization projection and atomic local outbox capture.
//! Network adapters consume immutable mutations; catalog persistence remains a local journal.
use crate::{EngineError, Result};
use lightcraft_catalog::replication::{Change, SyncState};
use lightcraft_catalog::{Catalog, ColorLabel, Op, Source};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

type Documents = BTreeMap<String, BTreeMap<String, Value>>;
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
// Smart filters contain local photo/album IDs at their schema-defined reference positions.
// Paths such as a Local filesystem folder are device-local and cannot be portable filters.
fn portable_filter(value: &mut Value, catalog: &Catalog, ids: &Catalog, connection: &str, depth: usize) -> Result<()> {
    if depth > 64 {
        return Err(error("smart-album cloud filter exceeds 64 nesting levels"));
    }
    if let Some(map) = value.as_object_mut() {
        for key in ["folder", "libraryFolder"] {
            map.remove(key);
        }
        if let Some(only) = map.get_mut("only").and_then(Value::as_array_mut) {
            *only = only.iter().filter_map(|id| id.as_u64().and_then(|id| mapped_photo(catalog, ids, connection, id))).map(Value::String).collect();
        }
        if let Some(album) = map.get_mut("album")
            && let Some(id) = album.as_u64()
        {
            *album = Value::String(global(ids, "album", id)?);
        }
        if map.get("field").and_then(Value::as_str) == Some("album")
            && let Some(value) = map.get_mut("value")
            && let Some(id) = value.as_u64()
        {
            *value = Value::String(global(ids, "album", id)?);
        }
        for child in map.values_mut() {
            portable_filter(child, catalog, ids, connection, depth + 1)?;
        }
    } else if let Some(values) = value.as_array_mut() {
        for child in values {
            portable_filter(child, catalog, ids, connection, depth + 1)?;
        }
    }
    Ok(())
}
fn project(catalog: &Catalog, ids: &Catalog, state: &SyncState, scope: Option<&Scope>) -> Result<Documents> {
    let mut docs = Documents::new();
    let photos: Vec<_> = scope.map_or_else(|| catalog.photos().collect(), |s| s.photos.iter().filter_map(|id| catalog.photo(*id)).collect());
    for photo in photos {
        let Source::Managed { asset_id } = &photo.source else { continue };
        let Some(asset) = catalog.asset(asset_id).filter(|a| a.original.remote.connection == state.connection) else { continue };
        let mut value = encode(photo)?;
        let map = value.as_object_mut().ok_or_else(|| error("photo metadata is not an object"))?;
        map.remove("id");
        map.remove("local");
        map.remove("local_baseline");
        map.remove("source");
        let mut content = serde_json::Map::new();
        content.insert("assetId".into(), encode(asset_id)?);
        let mut portable_asset = asset.clone();
        portable_asset.original.remote.connection = "library".into();
        if let Some(proxy) = &mut portable_asset.smart_preview {
            proxy.remote.connection = "library".into();
        }
        if let Some(thumbnail) = &mut portable_asset.thumbnail {
            thumbnail.remote.connection = "library".into();
        }
        for document in portable_asset.associated.values_mut() {
            document.remote.connection = "library".into();
        }
        content.insert("asset".into(), encode(&portable_asset)?);
        for key in ["format", "kind", "width", "height", "file_size", "content_hash", "preview_only", "embedded_lens", "as_shot_wb", "duration"] {
            if let Some(value) = map.remove(key) {
                content.insert(key.into(), value);
            }
        }
        map.insert("content".into(), Value::Object(content));
        if let Some(id) = photo.copy_of {
            map.insert("copy_of".into(), mapped_photo(catalog, ids, &state.connection, id.0).map_or(Value::Null, Value::String));
        }
        let mut meta = map.remove("meta").and_then(|v| v.as_object().cloned()).ok_or_else(|| error("photo metadata is not an object"))?;
        for field in ["usage_terms", "copyright_url"] {
            meta.entry(field).or_insert_with(|| json!(""));
        }
        meta.entry("regions").or_insert_with(|| json!([]));
        meta.entry("copyright_status").or_insert(encode(&lightcraft_catalog::Meta::default().copyright_status)?);
        let mut fields: BTreeMap<_, _> = map.iter().map(|(key, value)| (key.clone(), value.clone())).collect();
        for (key, value) in meta {
            fields.insert(format!("meta.{key}"), value);
        }
        docs.insert(format!("photo:{}", global(ids, "photo", photo.id.0)?), fields);
    }
    let albums: Vec<_> = scope.map_or_else(|| catalog.albums().collect(), |s| s.albums.iter().filter_map(|id| catalog.album(*id)).collect());
    for album in albums {
        let mut value = encode(album)?;
        let map = value.as_object_mut().ok_or_else(|| error("album metadata is not an object"))?;
        map.remove("id");
        map.remove("photos");
        map.insert("parent".into(), album.parent.map(|id| global(ids, "album", id.0)).transpose()?.map_or(Value::Null, Value::String));
        map.insert("cover".into(), album.cover.and_then(|id| mapped_photo(catalog, ids, &state.connection, id.0)).map_or(Value::Null, Value::String));
        if let Some(smart) = map.get_mut("smart") {
            portable_filter(smart, catalog, ids, &state.connection, 0)?;
        }
        let mut fields: BTreeMap<_, _> = map.iter().map(|(key, value)| (key.clone(), value.clone())).collect();
        fields.insert("deleted".into(), json!(false));
        for id in &album.photos {
            if let Some(id) = mapped_photo(catalog, ids, &state.connection, id.0) {
                fields.insert(format!("member:{id}"), json!(true));
            }
        }
        docs.insert(format!("album:{}", global(ids, "album", album.id.0)?), fields);
    }
    let stacks: Vec<_> = scope.map_or_else(|| catalog.stacks().collect(), |s| s.stacks.iter().filter_map(|id| catalog.stack(*id)).collect());
    for stack in stacks {
        let photos: Vec<_> = stack.photos.iter().filter_map(|id| mapped_photo(catalog, ids, &state.connection, id.0)).collect();
        if photos.len() < 2 {
            continue;
        }
        docs.insert(
            format!("stack:{}", global(ids, "stack", stack.id.0)?),
            BTreeMap::from([("photos".into(), json!(photos)), ("collapsed".into(), json!(stack.collapsed)), ("deleted".into(), json!(false))]),
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
    let scope = affected(catalog, &next, &op);
    let mut metadata = ensure_ids(&mut next, &state, Some(&scope))?;
    let before = project(catalog, &next, &state, Some(&scope))?;
    let after = project(&next, &next, &state, Some(&scope))?;
    let mut changes = differences(&before, &after, &state);
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

fn user_inverse(op: Op) -> Op {
    match op {
        Op::SetCloudIdentity { .. } | Op::SetSyncState { .. } => Op::Batch { ops: Vec::new() },
        Op::Batch { ops } => Op::Batch { ops: ops.into_iter().map(user_inverse).collect() },
        op => op,
    }
}

pub(crate) fn undo_base(catalog: &Catalog, op: &Op) -> Option<UndoBase> {
    let state = catalog.sync_state()?;
    let scope = affected(catalog, catalog, op);
    let mut entities = BTreeSet::new();
    for (kind, locals) in [
        ("photo", scope.photos.iter().map(|id| id.0).collect::<Vec<_>>()),
        ("album", scope.albums.iter().map(|id| id.0).collect()),
        ("stack", scope.stacks.iter().map(|id| id.0).collect()),
    ] {
        for local in locals {
            if let Some(id) = catalog.cloud_identity(&format!("{kind}:{local}")) {
                entities.insert(format!("{kind}:{id}"));
            }
        }
    }
    Some(
        state
            .documents
            .iter()
            .filter(|(entity, _)| {
                entities.contains(*entity) || (scope.labels && entity.starts_with("library:")) || (scope.keywords && entity.starts_with("keyword:"))
            })
            .flat_map(|(entity, fields)| {
                fields.iter().map(move |(field, register)| ((entity.clone(), field.clone()), register.0.keys().cloned().collect()))
            })
            .collect(),
    )
}
pub(crate) fn guard_undo(catalog: &Catalog, op: &Op, expected: Option<&UndoBase>) -> Result<()> {
    let Some(state) = catalog.sync_state() else {
        return Ok(());
    };
    let mut next = catalog.clone();
    next.apply(op.clone())?;
    let scope = affected(catalog, &next, op);
    let before = project(catalog, catalog, state, Some(&scope))?;
    let after = project(&next, catalog, state, Some(&scope))?;
    for change in differences(&before, &after, state) {
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
    if catalog.sync_state().is_some() {
        return Err(error("synchronization is already enabled; keep its durable device identity"));
    }
    let mut next = catalog.clone();
    let mut state = SyncState::new(library.clone(), device, connection)?;
    state.protocol = protocol;
    state.local_owner = owner;
    let mut ops = ensure_ids(&mut next, &state, None)?;
    let identity = Op::SetCloudIdentity { entity: "library".into(), id: Some(library) };
    next.apply(identity.clone())?;
    ops.push(identity);
    let mut initial = project(&next, &next, &state, None)?;
    if let Some(library) = initial.get_mut(&format!("library:{}", state.library)) {
        library.retain(|_, value| !value.is_null());
        library.insert("identity".into(), json!(state.library));
    }
    let changes = differences(&Documents::new(), &initial, &state);
    enqueue_changes(&mut state, changes)?;
    ops.push(Op::SetSyncState { state: Some(Box::new(state)) });
    Ok(Op::Batch { ops })
}

mod materialize;
#[cfg(test)]
mod tests;
pub use materialize::accept_structure;
pub use materialize::incoming;
pub use materialize::incoming_append;
pub use materialize::incoming_checkpoint;
pub use materialize::resolve;
pub use materialize::restore_deleted;
pub use materialize::structural_conflicts;

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
        && local.cursor.as_deref().unwrap_or("") == cursor
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
            return Err(error(format!(
                "cloud entity exceeds the bounded mutation size: {entity}{label}; {} changes (limit {MAX_ENTITY_CHANGES}), {size} bytes (limit {MAX_ENTITY_BYTES} / 1 MiB); {recovery}; current library and outbox retained",
                group.len()
            )));
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
