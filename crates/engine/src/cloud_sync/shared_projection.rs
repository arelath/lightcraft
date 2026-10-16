//! Global structural projection has no catalog, local-ID or device-overlay inputs.
use super::error;
use crate::Result;
use lightcraft_catalog::replication::SyncState;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
pub(super) fn fields(state: &SyncState, entity: &str) -> BTreeMap<String, Value> {
    state
        .documents
        .get(entity)
        .into_iter()
        .flat_map(|fields| fields.keys())
        .filter_map(|field| state.winning(entity, field).map(|(_, v)| (field.clone(), v.clone())))
        .collect()
}

/// Resolve only the global graph. Local IDs and overlays cannot choose an edge.
pub(super) fn album_parents(state: &SyncState) -> Result<BTreeMap<String, Option<String>>> {
    let live: BTreeMap<_, _> = state
        .documents
        .keys()
        .filter(|entity| entity.starts_with("album:") && state.winning(entity, "deleted").is_none_or(|(_, value)| value.as_bool() != Some(true)))
        .map(|entity| (entity.clone(), fields(state, entity)))
        .collect();
    let folders: BTreeSet<_> =
        live.iter().filter(|(_, values)| values.get("folder").and_then(Value::as_bool) == Some(true)).map(|(entity, _)| entity.clone()).collect();
    let mut parents = BTreeMap::new();
    for (entity, values) in &live {
        let value = values.get("parent").cloned().unwrap_or(Value::Null);
        let reference: Option<super::portable::AlbumRef> = super::portable::decode(value)?;
        let parent = reference.map(|reference| format!("album:{}", reference.id())).filter(|parent| live.contains_key(parent));
        if parent.as_ref().is_some_and(|parent| !folders.contains(parent)) {
            return Err(error("cloud album parent is not a folder"));
        }
        parents.insert(entity.clone(), parent);
    }
    let mut visited = BTreeSet::new();
    for start in parents.keys().cloned().collect::<Vec<_>>() {
        let mut path = Vec::new();
        let mut seen = BTreeMap::new();
        let mut current = Some(start);
        while let Some(entity) = current {
            if visited.contains(&entity) {
                break;
            }
            if let Some(index) = seen.get(&entity).copied() {
                let chosen = path
                    .get(index..)
                    .and_then(|cycle: &[String]| cycle.iter().max())
                    .cloned()
                    .ok_or_else(|| error("invalid shared album graph traversal"))?;
                parents.insert(chosen, None);
                break;
            }
            if path.len() > parents.len() {
                return Err(error("shared album graph traversal limit reached"));
            }
            seen.insert(entity.clone(), path.len());
            path.push(entity.clone());
            current = parents.get(&entity).cloned().flatten();
        }
        visited.extend(path);
    }
    Ok(parents)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct SharedStack {
    pub(super) photos: Vec<String>,
    pub(super) collapsed: bool,
}

/// Resolve shared ownership before consulting any device-local objects. A
/// singleton cannot claim a photo merely because this device has a local overlay.
pub(super) fn stacks(state: &SyncState) -> Result<BTreeMap<String, SharedStack>> {
    let live: BTreeSet<_> = state
        .documents
        .iter()
        .filter(|(entity, fields)| {
            entity.starts_with("photo:")
                && fields.contains_key("content")
                && state.winning(entity, "deleted").is_none_or(|(_, value)| value.as_bool() != Some(true))
        })
        .filter_map(|(entity, _)| entity.strip_prefix("photo:"))
        .collect();
    let mut claimed = BTreeSet::new();
    let mut result = BTreeMap::new();
    for entity in state.documents.keys().filter(|entity| entity.starts_with("stack:")) {
        let values = fields(state, entity);
        let mut photos = Vec::new();
        let mut unique = BTreeSet::new();
        if let Some(value) = values.get("photos") {
            for value in value.as_array().ok_or_else(|| error("cloud stack membership is not a list"))? {
                let global = value.as_str().ok_or_else(|| error("invalid global stack photo reference"))?;
                let _: super::portable::PhotoRef = super::portable::decode(value.clone())?;
                if live.contains(global) && !claimed.contains(global) && unique.insert(global) {
                    photos.push(global.to_owned());
                }
            }
        }
        if values.get("deleted").and_then(Value::as_bool) == Some(true) || photos.len() < 2 {
            photos.clear();
        } else {
            claimed.extend(photos.iter().cloned());
        }
        result.insert(entity.clone(), SharedStack { photos, collapsed: values.get("collapsed").and_then(Value::as_bool).unwrap_or(false) });
    }
    Ok(result)
}

#[cfg(test)]
mod tests;
