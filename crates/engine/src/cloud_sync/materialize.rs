//! Validate and install portable documents without echoing them into the local outbox.
use super::shared_projection::{fields, stacks as shared_stacks};
use super::*;
use lightcraft_catalog::assets::{Asset, AssetId};
use lightcraft_catalog::replication::Mutation;
use lightcraft_catalog::{Album, AlbumId, Meta, Photo, PhotoId, Stack, StackId, Version};
use std::sync::Arc;

#[cfg(test)]
thread_local! { static PROJECTED_PHOTOS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) }; }

#[cfg(test)]
mod tests;

fn decoded<T: serde::de::DeserializeOwned>(value: Value) -> Result<T> {
    serde_json::from_value(value).map_err(|e| error(format!("unsupported or malformed cloud metadata: {e}")))
}
fn local(catalog: &Catalog, kind: &str, global: &str) -> Option<u64> {
    catalog.cloud_local_identity(global)?.strip_prefix(kind)?.strip_prefix(':')?.parse().ok()
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
fn album_reference(catalog: &mut Catalog, ops: &mut Vec<Op>, global: &str) -> Result<AlbumId> {
    if let Some(id) = local(catalog, "album", global) {
        return Ok(AlbumId(id));
    }
    // A reference can arrive before its target or outlive a tombstone. Reserve an
    // identity, never a live album; later delivery/restoration uses this same ID.
    let id = catalog.alloc_album_id();
    apply(catalog, ops, Op::SetCloudIdentity { entity: format!("album:{}", id.0), id: Some(global.into()) })?;
    Ok(id)
}
fn filter_to_local(value: &mut Value, catalog: &mut Catalog, ops: &mut Vec<Op>, depth: usize) -> Result<()> {
    if depth > 64 {
        return Err(error("cloud smart filter exceeds 64 nesting levels"));
    }
    if let Some(map) = value.as_object_mut() {
        if map.contains_key("folder") || map.contains_key("libraryFolder") {
            return Err(error("cloud smart filter contains a device-local path"));
        }
        if let Some(only) = map.get_mut("only").and_then(Value::as_array_mut) {
            let restricted = !only.is_empty();
            *only = only
                .iter()
                .map(|value| photo_reference(catalog, value))
                .collect::<Result<Vec<_>>>()?
                .into_iter()
                .flatten()
                .map(|id| json!(id))
                .collect();
            if restricted && only.is_empty() {
                map.insert("onlyRestricted".into(), json!(true));
            } else if !only.is_empty() {
                // Nonempty legacy selections already constrain matching. Preserve
                // their original local representation on self-synchronization.
                map.remove("onlyRestricted");
            }
        }
        if let Some(album) = map.get_mut("album")
            && let Some(global) = album.as_str()
        {
            let id = album_reference(catalog, ops, global)?;
            *album = json!(id);
        }
        if map.get("field").and_then(Value::as_str) == Some("album")
            && let Some(value) = map.get_mut("value")
            && let Some(global) = value.as_str()
        {
            let id = album_reference(catalog, ops, global)?;
            *value = json!(id);
        }
        for child in map.values_mut() {
            filter_to_local(child, catalog, ops, depth + 1)?;
        }
    } else if let Some(values) = value.as_array_mut() {
        for child in values {
            filter_to_local(child, catalog, ops, depth + 1)?;
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

fn album_parents(catalog: &Catalog, state: &SyncState, ids: &BTreeMap<String, AlbumId>) -> Result<BTreeMap<AlbumId, Option<AlbumId>>> {
    let shared = super::shared_projection::album_parents(state)?;
    let mut parents: BTreeMap<_, _> = catalog.albums().map(|album| (album.id, album.parent)).collect();
    for (entity, id) in ids {
        let parent = shared.get(entity).and_then(|parent| parent.as_ref()).and_then(|parent| ids.get(parent)).copied();
        parents.insert(*id, parent);
    }
    // Shared cycle choices are already fixed globally. Validate the remaining local
    // graph independently; a local node never chooses which shared edge is severed.
    let mut visited = BTreeSet::new();
    for start in parents.keys().copied().collect::<Vec<_>>() {
        let mut path = BTreeSet::new();
        let mut current = Some(start);
        while let Some(id) = current {
            if visited.contains(&id) {
                break;
            }
            if !path.insert(id) {
                return Err(error("device-local album graph contains a cycle"));
            }
            if path.len() > parents.len() {
                return Err(error("album graph traversal limit reached"));
            }
            current = parents.get(&id).copied().flatten();
        }
        visited.extend(path);
    }
    Ok(parents)
}

/// Cross-entity constraints can require a projection different from the selected
/// register value. Report both choices without altering the retained causal data.
pub fn structural_conflicts(catalog: &Catalog) -> Vec<Value> {
    let Some(state) = catalog.sync_state() else { return Vec::new() };
    let index: BTreeMap<_, _> = catalog
        .cloud_identities()
        .iter()
        .filter_map(|(key, global)| {
            let (kind, local) = key.split_once(':')?;
            Some(((kind, global.as_str()), local.parse::<u64>().ok()?))
        })
        .collect();
    let mut conflicts = Vec::new();
    for entity in state.documents.keys() {
        if state.winning(entity, "deleted").is_some_and(|(_, v)| v.as_bool() == Some(true)) {
            continue;
        }
        if let Some(global) = entity.strip_prefix("album:") {
            let Some((_, proposed)) = state.winning(entity, "parent") else {
                continue;
            };
            let Some(id) = index.get(&("album", global)).copied().map(AlbumId) else {
                continue;
            };
            let projected = catalog
                .album(id)
                .and_then(|album| album.parent)
                .and_then(|parent| catalog.cloud_identity(&format!("album:{}", parent.0)))
                .map_or(Value::Null, |global| json!(global));
            if &projected != proposed {
                conflicts.push(json!({"entity":entity,"field":"parent","name":catalog.album(id).map(|album|&album.name),"proposed":proposed,"projected":projected}));
            }
        } else if let Some(global) = entity.strip_prefix("stack:") {
            let Some((_, proposed)) = state.winning(entity, "photos") else {
                continue;
            };
            let projected: Vec<_> = index
                .get(&("stack", global))
                .copied()
                .map(StackId)
                .and_then(|id| catalog.stack(id))
                .into_iter()
                .flat_map(|stack| &stack.photos)
                .filter(|id| {
                    catalog.photo(**id).is_some_and(|photo| {
                        let Source::Managed { asset_id } = &photo.source else { return false };
                        catalog.asset(asset_id).is_some_and(|asset| asset.original.remote.connection == state.connection)
                    })
                })
                .filter_map(|id| catalog.cloud_identity(&format!("photo:{}", id.0)))
                .map(|global| json!(global))
                .collect();
            if &json!(projected) != proposed {
                conflicts.push(json!({"entity":entity,"field":"photos","proposed":proposed,"projected":projected}));
            }
        }
    }
    conflicts
}
fn device_local_photo(catalog: &Catalog, connection: &str, id: PhotoId) -> bool {
    catalog.photo(id).is_some_and(|photo| {
        let Source::Managed { asset_id } = &photo.source else { return true };
        catalog.asset(asset_id).is_none_or(|asset| asset.original.remote.connection != connection)
    })
}

fn install(catalog: &Catalog, state: &SyncState) -> Result<Vec<Op>> {
    let old = catalog.sync_state();
    let mut selected: BTreeSet<_> = state
        .documents
        .iter()
        .filter(|(entity, fields)| old.and_then(|old| old.documents.get(*entity)) != Some(*fields))
        .map(|(entity, _)| entity.as_str())
        .collect();
    if selected.is_empty() {
        return Ok(Vec::new());
    }
    // Arrival/content/deletion can resolve retained references without changing
    // the referencing document. Ordinary metadata edits only decode that photo.
    let reference_targets: BTreeSet<_> = selected
        .iter()
        .filter_map(|entity| {
            entity.strip_prefix("photo:").filter(|_| {
                ["content", "deleted"].iter().any(|field| {
                    old.and_then(|old| old.documents.get(*entity)).and_then(|fields| fields.get(*field))
                        != state.documents.get(*entity).and_then(|fields| fields.get(*field))
                })
            })
        })
        .collect();
    let references = !reference_targets.is_empty();
    let albums = references || selected.iter().any(|entity| entity.starts_with("album:"));
    let stacks = references || selected.iter().any(|entity| entity.starts_with("stack:"));
    selected.extend(
        state
            .documents
            .iter()
            .filter(|(entity, _)| {
                (albums && entity.starts_with("album:"))
                    || (references
                        && entity.starts_with("photo:")
                        && state
                            .winning(entity, "copy_of")
                            .and_then(|(_, value)| value.as_str())
                            .is_some_and(|global| reference_targets.contains(global)))
            })
            .map(|(entity, _)| entity.as_str()),
    );
    let mut next = catalog.clone();
    let mut ops = Vec::new();
    let device_local = device_local_smart_albums(catalog, &state.connection);
    // Allocate all photo/album identities first so virtual copies and nested filters can
    // resolve references regardless of lexical/global-ID order.
    let mut photo_ids = BTreeMap::new();
    let mut album_ids = BTreeMap::new();
    for entity in &selected {
        let entity = *entity;
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
                photo_ids.insert(entity.to_owned(), id);
            }
            "album" => {
                let known = local(&next, kind, global).map(AlbumId);
                let id = known.unwrap_or_else(|| next.alloc_album_id());
                if known.is_none() {
                    apply(&mut next, &mut ops, Op::SetCloudIdentity { entity: format!("album:{}", id.0), id: Some(global.into()) })?;
                }
                if deleted {
                    continue;
                }
                if device_local.contains(&id) {
                    continue;
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
                album_ids.insert(entity.to_owned(), id);
            }
            "photo" | "stack" | "keyword" | "library" => {}
            _ => return Err(error("unsupported synchronized entity type")),
        }
    }
    for (entity, id) in &photo_ids {
        #[cfg(test)]
        PROJECTED_PHOTOS.with(|count| count.set(count.get() + 1));
        let values = fields(state, entity);
        let content = values.get("content").and_then(Value::as_object).ok_or_else(|| error("cloud photo has no immutable content description"))?;
        let asset_id: AssetId = decoded(content.get("assetId").cloned().ok_or_else(|| error("cloud photo has no asset identity"))?)?;
        let asset: Asset = decoded(content.get("asset").cloned().ok_or_else(|| error("cloud photo has no asset record"))?)?;
        let asset = asset.rebound(&state.connection).map_err(error)?;
        if next.asset(&asset_id).is_some_and(|old| !old.same_manifest(&asset)) {
            return Err(error("remote immutable asset identity describes different data; local data retained"));
        }
        if next.asset(&asset_id) != Some(&asset) {
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
    let parents = if albums { album_parents(&next, state, &album_ids)? } else { BTreeMap::new() };
    let moved: Vec<_> = parents
        .iter()
        .filter(|(id, parent)| next.album(**id).is_some_and(|album| album.parent != **parent))
        .map(|(id, parent)| (*id, *parent))
        .collect();
    for (id, _) in &moved {
        if next.album(*id).is_some_and(|album| album.parent.is_some()) {
            apply(&mut next, &mut ops, Op::MoveAlbum { id: *id, parent: None })?;
        }
    }
    for (id, parent) in moved {
        if parent.is_some() {
            apply(&mut next, &mut ops, Op::MoveAlbum { id, parent })?;
        }
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
                    value = encode(&parents.get(id).copied().flatten())?;
                }
                "cover" => value = encode(&photo_reference(&next, &value)?)?,
                "smart" if !value.is_null() => filter_to_local(&mut value, &mut next, &mut ops, 0)?,
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
    if stacks {
        let shared = shared_stacks(state)?;
        let synchronized_stacks: BTreeSet<_> = shared
            .keys()
            .filter_map(|entity| entity.strip_prefix("stack:"))
            .filter_map(|global| local(&next, "stack", global).map(StackId))
            .collect();
        let mut desired_stacks = Vec::new();
        let mut claimed = BTreeSet::new();
        for (entity, shared) in &shared {
            let global = entity.strip_prefix("stack:").ok_or_else(|| error("invalid stack identity"))?;
            let known = local(&next, "stack", global).map(StackId);
            let mut photos: Vec<_> = shared
                .photos
                .iter()
                .map(|global| {
                    local(&next, "photo", global)
                        .map(PhotoId)
                        .filter(|id| next.photo(*id).is_some())
                        .ok_or_else(|| error("live shared stack photo did not materialize"))
                })
                .collect::<Result<Vec<_>>>()?;
            if let Some(stack) = known.and_then(|id| next.stack(id)) {
                photos.extend(stack.photos.iter().copied().filter(|id| device_local_photo(&next, &state.connection, *id)));
            }
            let mut unique = BTreeSet::new();
            photos.retain(|id| !claimed.contains(id) && unique.insert(*id));
            if photos.len() < 2 {
                continue;
            }
            claimed.extend(photos.iter().copied());
            let id = known.unwrap_or_else(|| next.alloc_stack_id());
            if known.is_none() {
                apply(&mut next, &mut ops, Op::SetCloudIdentity { entity: format!("stack:{}", id.0), id: Some(global.into()) })?;
            }
            let collapsed =
                if shared.photos.is_empty() { known.and_then(|id| next.stack(id)).is_some_and(|stack| stack.collapsed) } else { shared.collapsed };
            desired_stacks.push(Stack { id, photos, collapsed });
        }
        // Unsynchronized mixed stacks relinquish photos owned by a shared winner.
        // Their remaining local relationships survive when at least two members remain.
        for stack in next.stacks().filter(|stack| !synchronized_stacks.contains(&stack.id)) {
            let photos: Vec<_> = stack.photos.iter().copied().filter(|id| !claimed.contains(id)).collect();
            if photos.len() >= 2 {
                claimed.extend(photos.iter().copied());
                desired_stacks.push(Stack { photos, ..stack.clone() });
            }
        }
        let desired_by_id: BTreeMap<_, _> = desired_stacks.iter().map(|stack| (stack.id, stack)).collect();
        let removed: Vec<_> =
            next.stacks().filter(|stack| desired_by_id.get(&stack.id).is_none_or(|desired| **desired != **stack)).map(|stack| stack.id).collect();
        for id in removed {
            apply(&mut next, &mut ops, Op::RemoveStack { id })?;
        }
        for stack in desired_stacks {
            if next.stack(stack.id).is_none() {
                apply(&mut next, &mut ops, Op::AddStack { stack })?;
            }
        }
    }
    for entity in &selected {
        let entity = *entity;
        let Some((kind, global)) = entity.split_once(':') else { continue };
        let values = fields(state, entity);
        match kind {
            "album" if values.get("deleted").and_then(Value::as_bool) == Some(true) => {
                if let Some(id) = local(&next, "album", global).map(AlbumId).filter(|id| next.album(*id).is_some() && !device_local.contains(id)) {
                    let children: Vec<_> = next.albums().filter(|a| a.parent == Some(id)).map(|a| a.id).collect();
                    for child in children {
                        apply(&mut next, &mut ops, Op::MoveAlbum { id: child, parent: None })?;
                    }
                    apply(&mut next, &mut ops, Op::RemoveAlbum { id })?;
                }
            }
            "stack" => {}
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

/// Explicitly accept a valid projected relationship, including a dissolved stack
/// that no longer has a local object to edit. Retained immutable history is untouched.
pub fn accept_structure(catalog: &Catalog, entity: &str) -> Result<Op> {
    let conflict = structural_conflicts(catalog)
        .into_iter()
        .find(|value| value.get("entity").and_then(Value::as_str) == Some(entity))
        .ok_or_else(|| error("no current structural conflict for this entity; refresh synchronization status"))?;
    let field = conflict.get("field").and_then(Value::as_str).ok_or_else(|| error("structural conflict has no field"))?;
    let value = conflict.get("projected").cloned().ok_or_else(|| error("structural conflict has no projection"))?;
    let mut state = catalog.sync_state().cloned().ok_or_else(|| error("enable synchronization first"))?;
    state.enqueue(vec![Change { entity: entity.into(), field: field.into(), base: state.winning(entity, field).map(|(stamp, _)| stamp), value }])?;
    let mut ops = install(catalog, &state)?;
    ops.push(Op::SetSyncState { state: Some(Box::new(state)) });
    Ok(Op::Batch { ops })
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
    crate::cloud_outcome::HeadPosition::new(cursor.clone(), generation).map_err(error)?;
    let mut state = catalog.sync_state().cloned().ok_or_else(|| error("enable synchronization first"))?;
    let uploads = acknowledged_uploads(catalog, remote, acknowledged, Some(generation))?;
    state.merge(remote)?;
    state.acknowledge(acknowledged, cursor, generation)?;
    let mut ops = install(catalog, &state)?;
    ops.extend(uploads);
    ops.push(Op::SetSyncState { state: Some(Box::new(state)) });
    Ok(Op::Batch { ops })
}

/// A verified worker result may finish after a newer result. The worker's captured
/// generation distinguishes this from a remote scan rolling back its own baseline.
pub fn completed_checkpoint(
    catalog: &Catalog,
    remote: &lightcraft_catalog::replication::Portable,
    acknowledged: &BTreeSet<String>,
    cursor: String,
    generation: u64,
    started_generation: u64,
) -> Result<Op> {
    crate::cloud_outcome::HeadPosition::new(cursor.clone(), generation).map_err(error)?;
    let current = catalog.sync_state().ok_or_else(|| error("enable synchronization first"))?;
    if generation < started_generation {
        return Err(error("remote catalog rolled back below the worker's acknowledged generation; pending work retained"));
    }
    if generation < current.generation {
        // Both completions were certified against their captured baselines. Preserve
        // the newer durable cursor while still reconciling this result's exact ACKs.
        let newer = current.cursor.clone().ok_or_else(|| error("acknowledged cloud head has no cursor"))?;
        incoming_checkpoint(catalog, remote, acknowledged, newer, current.generation)
    } else {
        incoming_checkpoint(catalog, remote, acknowledged, cursor, generation)
    }
}

/// Incoming effects and exact publication receipts commit in the same journal record.
pub fn incoming_append(catalog: &Catalog, progress: lightcraft_catalog::replication::AppendProgress, ids: &BTreeSet<String>) -> Result<Op> {
    let mut state = catalog.sync_state().cloned().ok_or_else(|| error("enable synchronization first"))?;
    let uploads = acknowledged_uploads(catalog, &progress.remote, ids, None)?;
    state.merge(&progress.remote)?;
    state.acknowledge_append(ids, progress)?;
    let mut ops = install(catalog, &state)?;
    ops.extend(uploads);
    ops.push(Op::SetSyncState { state: Some(Box::new(state)) });
    Ok(Op::Batch { ops })
}

/// One mutation can introduce many photos. Exact receipt proof acknowledges every
/// covered, fully byte-verified upload, including a sibling worker's waiting intent.
fn acknowledged_uploads(
    catalog: &Catalog,
    remote: &lightcraft_catalog::replication::Portable,
    ids: &BTreeSet<String>,
    generation: Option<u64>,
) -> Result<Vec<Op>> {
    let Some(state) = catalog.sync_state() else { return Ok(Vec::new()) };
    let mut content: BTreeMap<_, BTreeMap<String, String>> = BTreeMap::new();
    for id in ids {
        let Some(mutation) = state.pending.get(id) else { continue };
        let digest = mutation.digest()?;
        if remote.received.get(id).is_none_or(|receipt| receipt.digest != digest) {
            return Err(error("upload acknowledgement lacks exact publication proof"));
        }
        for change in &mutation.changes {
            if change.field == "content"
                && let Some(asset) = change.value.get("assetId").and_then(Value::as_str)
            {
                content.entry((change.entity.clone(), asset.to_owned())).or_default().insert(id.clone(), digest.clone());
            }
        }
    }
    Ok(catalog
        .upload_intents()
        .iter()
        .filter_map(|(id, intent)| {
            let global = catalog.cloud_identity(&format!("photo:{}", intent.photo_id.0))?;
            let asset = catalog.asset(&intent.asset_id)?;
            if intent.state == lightcraft_catalog::uploads::UploadState::Synchronized
                || asset.original.remote.connection != state.connection
                || !content.contains_key(&(format!("photo:{global}"), intent.asset_id.0.clone()))
                || asset.blobs().any(|blob| !intent.verified.contains(&blob.remote.key))
            {
                return None;
            }
            let mut intent = intent.clone();
            intent.state = lightcraft_catalog::uploads::UploadState::Synchronized;
            intent.error = None;
            intent.finalization = Some(lightcraft_catalog::uploads::UploadFinalization {
                version: 1,
                protocol: state.protocol,
                library: state.library.clone(),
                generation,
                proofs: content.get(&(format!("photo:{global}"), intent.asset_id.0.clone()))?.clone(),
            });
            Some(Op::SetUploadIntent { id: id.clone(), intent: Some(Box::new(intent)) })
        })
        .collect())
}

/// Apply certified evidence with an explicit protocol shape. Browser callers first
/// pass no ACKs, flush received state, then finish their durable ACK receipt.
pub fn completed_outcome(catalog: &Catalog, outcome: &crate::cloud_outcome::Outcome, ids: &BTreeSet<String>, started_generation: u64) -> Result<Op> {
    if !ids.is_subset(outcome.acknowledged()) {
        return Err(error("result does not certify the requested acknowledgements"));
    }
    if let Some(progress) = outcome.append_progress() {
        return incoming_append(catalog, progress.clone(), ids);
    }
    if let Some(position) = outcome.head_position() {
        return completed_checkpoint(catalog, outcome.state(), ids, position.cursor().to_owned(), position.generation(), started_generation);
    }
    if started_generation > 0 {
        return Err(error("remote catalog rolled back to an empty head; pending work retained"));
    }
    let mut state = catalog.sync_state().cloned().ok_or_else(|| error("enable synchronization first"))?;
    if !state.protocol.is_head() {
        return Err(error("empty conditional head cannot update an append library"));
    }
    state.merge(outcome.state())?;
    let mut ops = install(catalog, &state)?;
    ops.push(Op::SetSyncState { state: Some(Box::new(state)) });
    Ok(Op::Batch { ops })
}
