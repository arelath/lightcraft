use super::support::Fixture;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

/// Valid empty marker batches isolate traversal/replay costs from media and edits.
pub fn extend_markers(f: &Fixture, count: usize) -> BTreeMap<String, u64> {
    let mut storage = f.state.lock().unwrap();
    let mut head: Value = serde_json::from_slice(&storage.objects["/photos/library/catalog/head.json"]).unwrap();
    let mut sizes = BTreeMap::new();
    for _ in 0..count {
        let bytes = serde_json::to_vec(&json!({"version":3,"parent":head["tip"],"imports":[],"checkpointMarker":true})).unwrap();
        let hash = super::support::hash(&bytes);
        let key = format!("library/catalog/changes/{hash}.json");
        head["tip"] = json!({"key":key,"hash":hash,"size":bytes.len()});
        head["generation"] = json!(head["generation"].as_u64().unwrap() + 1);
        sizes.insert(format!("/photos/{key}"), bytes.len() as u64);
        storage.objects.insert(format!("/photos/{key}"), bytes);
    }
    storage.objects.insert("/photos/library/catalog/head.json".into(), serde_json::to_vec(&head).unwrap());
    sizes
}
pub fn assert_cost(f: &Fixture, sizes: &BTreeMap<String, u64>, each: u64) {
    let storage = f.state.lock().unwrap();
    let mut costs = BTreeMap::<&str, (u64, u64)>::new();
    for request in &storage.traffic {
        if request.method == "GET" && sizes.contains_key(&request.target) {
            let cost = costs.entry(&request.target).or_default();
            cost.0 += 1;
            cost.1 += request.response_bytes;
            assert_eq!(request.status, 200);
        }
    }
    if each == 0 {
        assert!(costs.is_empty(), "{costs:?}");
    } else {
        assert_eq!(costs.keys().copied().collect::<BTreeSet<_>>(), sizes.keys().map(String::as_str).collect());
        for (key, size) in sizes {
            assert_eq!(costs.get(key.as_str()), Some(&(each, each * size)), "{key}");
        }
    }
    assert_eq!(storage.signature_errors, 0);
    eprintln!("history replay: batches={}, gets={}, response_bytes={}", sizes.len(), sizes.len() as u64 * each, sizes.values().sum::<u64>() * each);
}
