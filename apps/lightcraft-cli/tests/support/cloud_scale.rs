//! Minimal valid metadata at scale, with real bounded mutations and head objects.
use crate::{lightcraft_catalog as catalog, saved_catalog, support};
use catalog::{
    Catalog, Photo, PhotoId, Source,
    assets::AssetId,
    replication::{Change, Mutation, Receipt, Register, Stamp},
};
use serde_json::{Value, json};
use std::{collections::BTreeMap, path::Path};

pub fn seed(f: &support::Fixture, directory: &Path, count: usize) -> Catalog {
    let base = saved_catalog(directory);
    let mut state = base.sync_state().unwrap().clone();
    let source = base.assets().values().next().unwrap();
    let asset = catalog::assets::Asset { original: source.original.clone(), smart_preview: None, thumbnail: None, associated: BTreeMap::new() };
    let mut value = serde_json::to_value(&base).unwrap();
    value["assets"]["scale_asset"] = json!(asset);
    let mut mutations = Vec::new();
    for start in (1..count).step_by(100) {
        let sequence = mutations.len() as u64 + 1;
        let mut changes = Vec::new();
        for n in start..(start + 100).min(count) {
            let global = format!("scale_{n:06}");
            let id = PhotoId(n as u64 + 1);
            let photo = Photo::new(id, Source::Managed { asset_id: AssetId("scale_asset".into()) }, "scale.png", "PNG", 32, 24, "");
            value["photos"][id.0.to_string()] = json!(photo);
            value["cloud_identities"][format!("photo:{}", id.0)] = json!(global);
            for (field, value) in [
                ("content", json!({"assetId":"scale_asset","asset":asset,"format":"PNG","width":32,"height":24})),
                ("file_name", json!("scale.png")),
                ("rating", json!(0)),
            ] {
                changes.push(Change { entity: format!("photo:{global}"), field: field.into(), base: None, value });
            }
        }
        let context = state.clock.clone();
        let mutation = Mutation {
            version: 2,
            library: state.library.clone(),
            revision: Stamp { device: "scale_fixture".into(), sequence },
            context: context.clone(),
            changes,
        };
        mutation.validate().unwrap();
        let stamp = mutation.revision.id();
        state.received.insert(stamp.clone(), Receipt { context, digest: mutation.digest().unwrap() });
        for change in &mutation.changes {
            state
                .documents
                .entry(change.entity.clone())
                .or_default()
                .insert(change.field.clone(), Register(BTreeMap::from([(stamp.clone(), change.value.clone())])));
        }
        state.clock.insert("scale_fixture".into(), sequence);
        mutations.push(mutation);
    }
    {
        let mut storage = f.state.lock().unwrap();
        let mut head: Value = serde_json::from_slice(&storage.objects["/photos/library/catalog/head.json"]).unwrap();
        let mut tip = head["tip"].clone();
        let mut generation = head["generation"].as_u64().unwrap();
        for mutation in mutations {
            let bytes = serde_json::to_vec(&json!({"version":3,"parent":tip,"imports":[],"mutations":[mutation]})).unwrap();
            assert!(bytes.len() <= 2 << 20);
            let hash = support::hash(&bytes);
            tip = json!({"key":format!("library/catalog/changes/{hash}.json"),"hash":hash,"size":bytes.len()});
            storage.objects.insert(format!("/photos/{}", tip["key"].as_str().unwrap()), bytes);
            generation += 1;
        }
        let checkpoint = json!({"version":1,"generation":generation,"covers":tip,"imports":{},"state":state.portable()});
        let _: lightcraft_engine::cloud_replication::Checkpoint = serde_json::from_value(checkpoint.clone()).unwrap();
        let bytes = serde_json::to_vec(&checkpoint).unwrap();
        let hash = support::hash(&bytes);
        let object = json!({"key":format!("library/catalog/checkpoints/{hash}.json"),"hash":hash,"size":bytes.len()});
        storage.objects.insert(format!("/photos/{}", object["key"].as_str().unwrap()), bytes);
        head["checkpoint"] = json!({"generation":generation,"covers":tip,"object":object});
        let marker = serde_json::to_vec(&json!({"version":3,"parent":tip,"imports":[],"checkpointMarker":true})).unwrap();
        let hash = support::hash(&marker);
        head["tip"] = json!({"key":format!("library/catalog/changes/{hash}.json"),"hash":hash,"size":marker.len()});
        storage.objects.insert(format!("/photos/{}", head["tip"]["key"].as_str().unwrap()), marker);
        head["generation"] = json!(generation + 1);
        state.cursor = Some(hash);
        state.generation = generation + 1;
        storage.objects.insert("/photos/library/catalog/head.json".into(), serde_json::to_vec(&head).unwrap());
    }
    state.validate().unwrap();
    value["sync_state"] = json!(state);
    value["next_photo"] = json!(count + 1);
    let catalog = Catalog::from_snapshot(&value.to_string()).unwrap();
    assert_eq!(catalog.photos().count(), count);
    let (mut journal, _, _) = catalog::Journal::open(Box::new(catalog::FsStore::open(directory).unwrap())).unwrap();
    journal.snapshot(&catalog).unwrap();
    catalog
}
