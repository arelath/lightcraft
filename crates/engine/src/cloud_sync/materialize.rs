//! Validate and install portable documents without echoing them into the local outbox.
use super::*;
use lightcraft_catalog::assets::{Asset, AssetId};
use lightcraft_catalog::replication::Mutation;
use lightcraft_catalog::{Album, AlbumId, Meta, Photo, PhotoId, Stack, StackId, Version};
use std::sync::Arc;

fn decoded<T: serde::de::DeserializeOwned>(value: Value) -> Result<T> {
    serde_json::from_value(value).map_err(|e| error(format!("unsupported or malformed cloud metadata: {e}")))
}
fn local(catalog: &Catalog, kind: &str, global: &str) -> Option<u64> {
    catalog.cloud_identities().iter().find_map(|(key, id)| {
        let (prefix, local) = key.split_once(':')?;
        (prefix == kind && id == global).then(|| local.parse().ok()).flatten()
    })
}
fn fields(state: &SyncState, entity: &str) -> BTreeMap<String, Value> {
    state
        .documents
        .get(entity)
        .into_iter()
        .flat_map(|fields| fields.keys())
        .filter_map(|field| state.winning(entity, field).map(|(_, v)| (field.clone(), v.clone())))
        .collect()
}
fn apply(catalog: &mut Catalog, ops: &mut Vec<Op>, op: Op) -> Result<()> {
    catalog.apply(op.clone())?;
    ops.push(op);
    Ok(())
}
fn photo_reference(catalog: &Catalog, value: &Value) -> Result<Option<PhotoId>> {
    if value.is_null() {
        return Ok(None);
    }
    let global = value.as_str().ok_or_else(|| error("invalid global photo reference"))?;
    Ok(local(catalog, "photo", global).map(PhotoId))
}
fn filter_to_local(value: &mut Value, catalog: &Catalog, depth: usize) -> Result<()> {
    if depth > 64 {
        return Err(error("cloud smart filter exceeds 64 nesting levels"));
    }
    if let Some(map) = value.as_object_mut() {
        if map.contains_key("folder") || map.contains_key("libraryFolder") {
            return Err(error("cloud smart filter contains a device-local path"));
        }
        if let Some(only) = map.get_mut("only").and_then(Value::as_array_mut) {
            *only = only
                .iter()
                .map(|value| photo_reference(catalog, value))
                .collect::<Result<Vec<_>>>()?
                .into_iter()
                .flatten()
                .map(|id| json!(id))
                .collect();
        }
        if let Some(album) = map.get_mut("album")
            && let Some(global) = album.as_str()
        {
            let id = local(catalog, "album", global).ok_or_else(|| error("cloud filter refers to an unknown album"))?;
            *album = json!(id);
        }
        if map.get("field").and_then(Value::as_str) == Some("album")
            && let Some(value) = map.get_mut("value")
            && let Some(global) = value.as_str()
        {
            let id = local(catalog, "album", global).ok_or_else(|| error("cloud rule refers to an unknown album"))?;
            *value = json!(id);
        }
        for child in map.values_mut() {
            filter_to_local(child, catalog, depth + 1)?;
        }
    } else if let Some(values) = value.as_array_mut() {
        for child in values {
            filter_to_local(child, catalog, depth + 1)?;
        }
    }
    Ok(())
}
fn merged_array(state: &SyncState, entity: &str, field: &str) -> Result<Value> {
    let mut values = Vec::new();
    if let Some(register) = state.documents.get(entity).and_then(|fields| fields.get(field)) {
        for value in register.0.values() {
            for value in value.as_array().ok_or_else(|| error("cloud history/version value is not a list"))? {
                if !values.contains(value) {
                    values.push(value.clone());
                }
            }
        }
    }
    Ok(Value::Array(values))
}
fn install(catalog: &Catalog, state: &SyncState) -> Result<Vec<Op>> {
    let mut next = catalog.clone();
    let mut ops = Vec::new();
    // Allocate all photo/album identities first so virtual copies and nested filters can
    // resolve references regardless of lexical/global-ID order.
    let mut photo_ids = BTreeMap::new();
    let mut album_ids = BTreeMap::new();
    for entity in state.documents.keys() {
        let Some((kind, global)) = entity.split_once(':') else { return Err(error("invalid cloud entity")) };
        let values = fields(state, entity);
        let deleted = values.get("deleted").and_then(Value::as_bool) == Some(true);
        match kind {
            "photo" if values.contains_key("content") => {
                let known = local(&next, kind, global).map(PhotoId);
                if deleted && known.is_none_or(|id| next.photo(id).is_none()) {
                    continue;
                }
                let id = known.unwrap_or_else(|| next.alloc_photo_id());
                if known.is_none() {
                    apply(&mut next, &mut ops, Op::SetCloudIdentity { entity: format!("photo:{}", id.0), id: Some(global.into()) })?;
                }
                photo_ids.insert(entity.clone(), id);
            }
            "album" if !deleted => {
                let known = local(&next, kind, global).map(AlbumId);
                let id = known.unwrap_or_else(|| next.alloc_album_id());
                if known.is_none() {
                    apply(&mut next, &mut ops, Op::SetCloudIdentity { entity: format!("album:{}", id.0), id: Some(global.into()) })?;
                }
                if next.album(id).is_none() {
                    let mut album = Album::new(id, values.get("name").and_then(Value::as_str).unwrap_or("Cloud album"));
                    album.folder = values.get("folder").and_then(Value::as_bool).unwrap_or(false);
                    album.quick = values.get("quick").and_then(Value::as_bool).unwrap_or(false);
                    if values.get("smart").is_some_and(|value| !value.is_null()) {
                        album.smart = Some(Box::new(lightcraft_catalog::Filter::default()));
                    }
                    apply(&mut next, &mut ops, Op::AddAlbum { album })?;
                }
                album_ids.insert(entity.clone(), id);
            }
            "photo" | "album" | "stack" | "keyword" | "library" => {}
            _ => return Err(error("unsupported synchronized entity type")),
        }
    }
    for (entity, id) in &photo_ids {
        let values = fields(state, entity);
        let content = values.get("content").and_then(Value::as_object).ok_or_else(|| error("cloud photo has no immutable content description"))?;
        let asset_id: AssetId = decoded(content.get("assetId").cloned().ok_or_else(|| error("cloud photo has no asset identity"))?)?;
        let mut asset: Asset = decoded(content.get("asset").cloned().ok_or_else(|| error("cloud photo has no asset record"))?)?;
        asset.original.remote.connection = state.connection.clone();
        if let Some(proxy) = &mut asset.smart_preview {
            proxy.remote.connection = state.connection.clone();
        }
        if let Some(thumbnail) = &mut asset.thumbnail {
            thumbnail.remote.connection = state.connection.clone();
        }
        for associated in asset.associated.values_mut() {
            associated.remote.connection = state.connection.clone();
        }
        asset.validate().map_err(error)?;
        if next.asset(&asset_id).is_some_and(|old| old != &asset) {
            return Err(error("remote immutable asset identity describes different data; local data retained"));
        }
        if next.asset(&asset_id).is_none() {
            apply(&mut next, &mut ops, Op::SetAsset { id: asset_id.clone(), asset: Some(Box::new(asset)) })?;
        }
        let seed = next
            .photo(*id)
            .map(|p| (**p).clone())
            .unwrap_or_else(|| Photo::new(*id, Source::Managed { asset_id: asset_id.clone() }, "cloud-photo", "RAW", 1, 1, ""));
        let mut value = encode(&seed)?;
        let map = value.as_object_mut().ok_or_else(|| error("invalid photo document"))?;
        map.insert("source".into(), encode(&Source::Managed { asset_id })?);
        map.insert("local".into(), json!(false));
        map.remove("local_baseline");
        for field in ["content_hash", "preview_only", "embedded_lens", "as_shot_wb", "duration"] {
            map.remove(field);
        }
        for (field, value) in content {
            if matches!(field.as_str(), "assetId" | "asset") {
                continue;
            }
            if !matches!(
                field.as_str(),
                "format" | "kind" | "width" | "height" | "file_size" | "content_hash" | "preview_only" | "embedded_lens" | "as_shot_wb" | "duration"
            ) {
                return Err(error("unsupported cloud content field"));
            }
            map.insert(field.clone(), value.clone());
        }
        let mut meta = map.remove("meta").and_then(|v| v.as_object().cloned()).ok_or_else(|| error("invalid photo metadata"))?;
        let meta_schema = encode(&Meta::default())?;
        for (field, value) in &values {
            if let Some(field) = field.strip_prefix("meta.") {
                if meta_schema.get(field).is_none() && !matches!(field, "copyright_status" | "usage_terms" | "copyright_url" | "regions") {
                    return Err(error("unsupported cloud metadata field"));
                }
                meta.insert(field.into(), value.clone());
                continue;
            }
            if field == "content" {
                continue;
            }
            if !matches!(
                field.as_str(),
                "file_name"
                    | "captured"
                    | "imported"
                    | "rating"
                    | "flag"
                    | "label"
                    | "develop"
                    | "edited"
                    | "versions"
                    | "history"
                    | "deleted"
                    | "copy_of"
                    | "copy_name"
                    | "import_look"
                    | "analysis"
            ) {
                return Err(error("unsupported cloud photo field"));
            }
            let value = match field.as_str() {
                "copy_of" => encode(&photo_reference(&next, value)?)?,
                "versions" | "history" => merged_array(state, entity, field)?,
                _ => value.clone(),
            };
            map.insert(field.clone(), value);
        }
        map.insert("meta".into(), Value::Object(meta));
        let mut photo: Photo = decoded(value)?;
        if photo.id != *id
            || photo.rating > 5
            || photo.width == 0
            || photo.height == 0
            || photo.width > 65535
            || photo.height > 65535
            || u64::from(photo.width).saturating_mul(u64::from(photo.height)) > 100_000_000
            || photo.file_name.is_empty()
            || photo.file_name.len() > 1024
            || photo.file_name.contains(['/', '\\'])
            || photo.file_name.chars().any(char::is_control)
        {
            return Err(error("invalid cloud photo dimensions, rating or filename"));
        }
        if let Some(register) = state.documents.get(entity).and_then(|v| v.get("develop")) {
            let winner = state.winning(entity, "develop").map(|(stamp, _)| stamp.id());
            for (revision, value) in &register.0 {
                if Some(revision) == winner.as_ref() {
                    continue;
                }
                if state.winning(entity, "develop").is_some_and(|(_, winner)| winner == value) {
                    continue;
                }
                let name = format!("Cloud conflict {revision}");
                if !photo.versions.iter().any(|v| v.name == name) {
                    photo.versions.push(Version { name, created: "".into(), settings: Arc::new(decoded(value.clone())?), auto: false });
                }
            }
        }
        if next.photo(*id).is_some_and(|current| **current == photo) {
            continue;
        }
        if next.photo(*id).is_some() {
            apply(&mut next, &mut ops, Op::RemovePhoto { id: *id })?;
        }
        apply(&mut next, &mut ops, Op::AddPhoto { photo: Box::new(photo) })?;
    }
    for (entity, id) in &album_ids {
        let values = fields(state, entity);
        let mut value = encode(next.album(*id).ok_or_else(|| error("cloud album disappeared"))?)?;
        let map = value.as_object_mut().ok_or_else(|| error("invalid cloud album document"))?;
        let mut photos = Vec::new();
        for (field, value) in &values {
            if let Some(global) = field.strip_prefix("member:") {
                if value.as_bool().ok_or_else(|| error("invalid album membership operation"))?
                    && let Some(id) = local(&next, "photo", global).map(PhotoId)
                    && next.photo(id).is_some()
                {
                    photos.push(id);
                }
                continue;
            }
            if field == "deleted" {
                continue;
            }
            if !matches!(field.as_str(), "name" | "parent" | "folder" | "cover" | "smart" | "quick" | "order") {
                return Err(error("unsupported cloud album field"));
            }
            let mut value = value.clone();
            match field.as_str() {
                "parent" => {
                    value = value
                        .as_str()
                        .and_then(|global| local(&next, "album", global))
                        .filter(|id| next.album(AlbumId(*id)).is_some())
                        .map_or(Value::Null, |id| json!(id))
                }
                "cover" => value = encode(&photo_reference(&next, &value)?)?,
                "smart" if !value.is_null() => filter_to_local(&mut value, &next, 0)?,
                _ => {}
            }
            map.insert(field.clone(), value);
        }
        map.insert("photos".into(), json!(photos));
        let desired: Album = decoded(value)?;
        let current = next.album(*id).ok_or_else(|| error("cloud album disappeared"))?.clone();
        if desired.folder != current.folder || desired.quick != current.quick {
            return Err(error("cloud album kind changed; data retained for resolution"));
        }
        if desired.name != current.name {
            apply(&mut next, &mut ops, Op::RenameAlbum { id: *id, name: desired.name })?;
        }
        if desired.parent != current.parent {
            apply(&mut next, &mut ops, Op::MoveAlbum { id: *id, parent: desired.parent })?;
        }
        if desired.order != current.order {
            apply(&mut next, &mut ops, Op::SetAlbumOrder { id: *id, order: desired.order })?;
        }
        let mut photos = desired.photos;
        photos.extend(current.photos.iter().copied().filter(|id| {
            next.photo(*id).is_some_and(|photo| {
                let Source::Managed { asset_id } = &photo.source else { return true };
                next.asset(asset_id).is_none_or(|asset| asset.original.remote.connection != state.connection)
            })
        }));
        if photos != current.photos {
            apply(&mut next, &mut ops, Op::SetAlbumPhotos { id: *id, photos })?;
        }
        if desired.cover != current.cover {
            apply(&mut next, &mut ops, Op::SetAlbumCover { id: *id, cover: desired.cover })?;
        }
        if desired.smart != current.smart {
            let rules = desired.smart.ok_or_else(|| error("remote smart-album kind changed"))?;
            apply(&mut next, &mut ops, Op::SetAlbumRules { id: *id, rules })?;
        }
    }
    for entity in state.documents.keys() {
        let Some((kind, global)) = entity.split_once(':') else { continue };
        let values = fields(state, entity);
        match kind {
            "album" if values.get("deleted").and_then(Value::as_bool) == Some(true) => {
                if let Some(id) = local(&next, "album", global).map(AlbumId).filter(|id| next.album(*id).is_some()) {
                    let children: Vec<_> = next.albums().filter(|a| a.parent == Some(id)).map(|a| a.id).collect();
                    for child in children {
                        apply(&mut next, &mut ops, Op::MoveAlbum { id: child, parent: None })?;
                    }
                    apply(&mut next, &mut ops, Op::RemoveAlbum { id })?;
                }
            }
            "stack" => {
                let known = local(&next, "stack", global).map(StackId);
                let mut photos: Vec<_> = values
                    .get("photos")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .map(|v| photo_reference(&next, v))
                    .collect::<Result<Vec<_>>>()?
                    .into_iter()
                    .flatten()
                    .filter(|id| next.photo(*id).is_some())
                    .collect();
                if let Some(stack) = known.and_then(|id| next.stack(id)) {
                    photos.extend(stack.photos.iter().copied().filter(|id| {
                        next.photo(*id).is_some_and(|p| {
                            let Source::Managed { asset_id } = &p.source else { return true };
                            next.asset(asset_id).is_none_or(|a| a.original.remote.connection != state.connection)
                        })
                    }));
                }
                if values.get("deleted").and_then(Value::as_bool) == Some(true) || photos.len() < 2 {
                    if let Some(id) = known.filter(|id| next.stack(*id).is_some()) {
                        apply(&mut next, &mut ops, Op::RemoveStack { id })?;
                    }
                    continue;
                }
                let collapsed = values.get("collapsed").and_then(Value::as_bool).unwrap_or(false);
                let id = known.unwrap_or_else(|| next.alloc_stack_id());
                if known.is_none() {
                    apply(&mut next, &mut ops, Op::SetCloudIdentity { entity: format!("stack:{}", id.0), id: Some(global.into()) })?;
                }
                let stack = Stack { id, photos, collapsed };
                if next.stack(id) == Some(&stack) {
                    continue;
                }
                let op = if next.stack(id).is_some() { Op::SetStack { id, photos: stack.photos, collapsed } } else { Op::AddStack { stack } };
                apply(&mut next, &mut ops, op)?;
            }
            "keyword" => {
                let path = values.get("path").and_then(Value::as_str).ok_or_else(|| error("cloud keyword has no path"))?;
                if crate::assets::sha256(path.to_lowercase().as_bytes()).0 != global {
                    return Err(error("cloud keyword identity mismatch"));
                }
                let info = if values.get("deleted").and_then(Value::as_bool) == Some(true) {
                    None
                } else {
                    Some(decoded(values.get("info").cloned().ok_or_else(|| error("cloud keyword attributes missing"))?)?)
                };
                if next.keyword_info(path) != info.as_ref() {
                    apply(&mut next, &mut ops, Op::SetKeyword { path: path.into(), info })?;
                }
            }
            "library" => {
                if global != state.library {
                    return Err(error("library metadata identity mismatch"));
                }
                for (field, value) in values {
                    if field == "identity" {
                        if value.as_str() != Some(state.library.as_str()) {
                            return Err(error("library identity field mismatch"));
                        }
                        continue;
                    }
                    let label = ColorLabel::ALL
                        .iter()
                        .copied()
                        .find(|label| field == format!("label.{label:?}"))
                        .ok_or_else(|| error("unsupported library metadata field"))?;
                    let name =
                        if value.is_null() { None } else { Some(value.as_str().ok_or_else(|| error("invalid colour label name"))?.to_owned()) };
                    if next.custom_label_name(label) != name.as_deref() {
                        apply(&mut next, &mut ops, Op::SetLabelName { label, name })?;
                    }
                }
            }
            _ => {}
        }
    }
    Ok(ops)
}

/// Returned batch contains both incoming logical changes and durable cursor/acknowledgements.
/// The owner must append it before publishing success, and apply it through `apply_received`.
pub fn resolve(catalog: &Catalog, entity: &str, field: &str, revision: &str) -> Result<Op> {
    let mut state = catalog.sync_state().cloned().ok_or_else(|| error("enable synchronization first"))?;
    let value = state
        .documents
        .get(entity)
        .and_then(|fields| fields.get(field))
        .and_then(|register| register.0.get(revision))
        .cloned()
        .ok_or_else(|| error("conflict revision is no longer available; refresh synchronization status"))?;
    let base = state.winning(entity, field).map(|(stamp, _)| stamp);
    let mut changes = vec![Change { entity: entity.into(), field: field.into(), base, value }];
    if field == "develop"
        && let Some((kind, global)) = entity.split_once(':')
        && kind == "photo"
        && let Some(photo) = local(catalog, kind, global).and_then(|id| catalog.photo(PhotoId(id)))
    {
        changes.push(Change {
            entity: entity.into(),
            field: "versions".into(),
            base: state.winning(entity, "versions").map(|(stamp, _)| stamp),
            value: encode(&photo.versions)?,
        });
    }
    state.enqueue(changes)?;
    let mut ops = install(catalog, &state)?;
    ops.push(Op::SetSyncState { state: Some(Box::new(state)) });
    Ok(Op::Batch { ops })
}

pub fn incoming(catalog: &Catalog, mutations: &[Mutation], acknowledged: &BTreeSet<String>, cursor: String, generation: u64) -> Result<Op> {
    let mut state = catalog.sync_state().cloned().ok_or_else(|| error("enable synchronization first"))?;
    for mutation in mutations {
        state.receive(mutation)?;
    }
    state.acknowledge(acknowledged, cursor, generation)?;
    let mut ops = install(catalog, &state)?;
    ops.push(Op::SetSyncState { state: Some(Box::new(state)) });
    Ok(Op::Batch { ops })
}

/// Explicit recovery observes known tombstones and preserves their retained offline edits.
pub fn restore_deleted(catalog: &Catalog, entity: &str) -> Result<Op> {
    let mut state = catalog.sync_state().cloned().ok_or_else(|| error("enable synchronization first"))?;
    if !entity.starts_with("photo:") || state.winning(entity, "deleted").is_none_or(|(_, v)| v.as_bool() != Some(true)) {
        return Err(error("choose a retained deleted cloud photo"));
    }
    let base = state.winning(entity, "deleted").map(|(stamp, _)| stamp);
    state.enqueue(vec![Change { entity: entity.into(), field: "deleted".into(), base, value: json!(false) }])?;
    let mut ops = install(catalog, &state)?;
    ops.push(Op::SetSyncState { state: Some(Box::new(state)) });
    Ok(Op::Batch { ops })
}

pub fn incoming_checkpoint(
    catalog: &Catalog,
    remote: &lightcraft_catalog::replication::Portable,
    acknowledged: &BTreeSet<String>,
    cursor: String,
    generation: u64,
) -> Result<Op> {
    let mut state = catalog.sync_state().cloned().ok_or_else(|| error("enable synchronization first"))?;
    state.merge(remote)?;
    state.acknowledge(acknowledged, cursor, generation)?;
    let mut ops = install(catalog, &state)?;
    ops.push(Op::SetSyncState { state: Some(Box::new(state)) });
    Ok(Op::Batch { ops })
}

/// Incoming effects and exact publication receipts commit in the same journal record.
pub fn incoming_append(catalog: &Catalog, progress: lightcraft_catalog::replication::AppendProgress, ids: &BTreeSet<String>) -> Result<Op> {
    let mut state = catalog.sync_state().cloned().ok_or_else(|| error("enable synchronization first"))?;
    state.merge(&progress.remote)?;
    state.acknowledge_append(ids, progress)?;
    let mut ops = install(catalog, &state)?;
    ops.push(Op::SetSyncState { state: Some(Box::new(state)) });
    Ok(Op::Batch { ops })
}
