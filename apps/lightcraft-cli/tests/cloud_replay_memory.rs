//! Isolated Windows child measurement: provider fixture allocations are excluded.
#![cfg(windows)]
#[path = "support/cloud_upload.rs"]
mod support;
use lightcraft_engine::catalog::{
    FsStore, Journal, PhotoId,
    replication::{Change, Mutation, Stamp},
};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::Command;
use support::Fixture;
fn powershell() -> Command {
    let mut command = Command::new("pwsh");
    command.creation_flags(0x0800_0000);
    command.args(["-NoLogo", "-NoProfile", "-NonInteractive"]);
    command
}
fn catalog(path: &Path) -> lightcraft_engine::catalog::Catalog {
    Journal::open(Box::new(FsStore::open(path).unwrap())).unwrap().1
}
fn run(f: &Fixture, path: &Path, commands: &[(&str, Value)]) {
    let (out, rows) = f.run(path, commands);
    assert!(out.status.success(), "{rows:?} {}", String::from_utf8_lossy(&out.stderr));
}
#[test]
fn superseded_head_payloads_do_not_accumulate_in_the_reader_process() {
    if !powershell().args(["-Command", "if ($PSVersionTable.PSVersion.Major -lt 7) { exit 1 }"]).output().is_ok_and(|output| output.status.success())
    {
        eprintln!("resident memory measurement skipped: PowerShell 7 unavailable; bounded replay unit tests still run");
        return;
    }
    let mut measured = Vec::new();
    for count in [100u64, 1500] {
        let f = Fixture::new();
        let path = f.dir.join("replay-memory");
        f.import(&path, false);
        run(&f, &path, &[("cloud.upload", json!({})), ("cloud.sync", json!({}))]);
        let base = catalog(&path);
        let state = base.sync_state().unwrap();
        let entity = format!("photo:{}", base.cloud_identity("photo:1").unwrap());
        let original_base = state.winning(&entity, "meta.caption").map(|(stamp, _)| stamp.clone());
        let mut sizes = BTreeMap::new();
        {
            let mut storage = f.state.lock().unwrap();
            let key = "/photos/library/catalog/head.json";
            let mut head: Value = serde_json::from_slice(&storage.objects[key]).unwrap();
            for n in 0..count {
                let mut context = state.clock.clone();
                if n > 0 {
                    context.insert("history-writer".into(), n);
                }
                let mutation = Mutation {
                    version: 2,
                    library: state.library.clone(),
                    revision: Stamp { device: "history-writer".into(), sequence: n + 1 },
                    context,
                    changes: vec![Change {
                        entity: entity.clone(),
                        field: "meta.caption".into(),
                        base: if n == 0 { original_base.clone() } else { Some(Stamp { device: "history-writer".into(), sequence: n }) },
                        value: json!(format!("{n:06}:{}", "z".repeat((256 << 10) - 7))),
                    }],
                };
                mutation.validate().unwrap();
                let bytes = serde_json::to_vec(&json!({"version":3,"parent":head["tip"],"imports":[],"mutations":[mutation]})).unwrap();
                let hash = support::hash(&bytes);
                let batch_key = format!("library/catalog/changes/{hash}.json");
                head["tip"] = json!({"key":batch_key,"hash":hash,"size":bytes.len()});
                head["generation"] = json!(head["generation"].as_u64().unwrap() + 1);
                sizes.insert(format!("/photos/{batch_key}"), bytes.len() as u64);
                storage.objects.insert(format!("/photos/{batch_key}"), bytes);
            }
            storage.objects.insert(key.into(), serde_json::to_vec(&head).unwrap());
            storage.requests.clear();
            storage.traffic.clear();
        }
        let mut child = f.command();
        child.args(["run", "--library"]).arg(&path).args(["cloud.sync", "{\"checkpoint\":true}"]);
        let request = f.dir.join("profile.json");
        std::fs::write(&request, serde_json::to_vec(&json!({"executable":child.get_program().to_str().unwrap(),"arguments":child.get_args().map(|arg|arg.to_str().unwrap()).collect::<Vec<_>>()})).unwrap()).unwrap();
        let mut profiler = powershell();
        profiler
            .arg("-File")
            .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/cloud-replay-memory.ps1"))
            .arg("-RequestPath")
            .arg(&request);
        for (key, value) in child.get_envs() {
            if let Some(value) = value {
                profiler.env(key, value);
            } else {
                profiler.env_remove(key);
            }
        }
        let output = profiler.output().unwrap();
        assert!(output.status.success(), "{} {}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
        let report: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(report["exit"], 0, "{report}");
        assert!(report["samples"].as_u64().unwrap() > 2, "{report}");
        let final_catalog = catalog(&path);
        let caption = &final_catalog.photo(PhotoId(1)).unwrap().meta.caption;
        assert_eq!(caption.len(), 256 << 10);
        assert!(caption.starts_with(&format!("{:06}:", count - 1)));
        let state_bytes = serde_json::to_vec(final_catalog.sync_state().unwrap()).unwrap().len();
        assert!(state_bytes < 1 << 20, "final state must remain small");
        let storage = f.state.lock().unwrap();
        let mut gets = BTreeMap::<&str, (u64, u64)>::new();
        for row in &storage.traffic {
            if row.method == "GET" && sizes.contains_key(&row.target) {
                let cost = gets.entry(&row.target).or_default();
                cost.0 += 1;
                cost.1 += row.response_bytes;
            }
        }
        for (key, size) in &sizes {
            assert_eq!(gets.get(key.as_str()), Some(&(1, *size)), "{key}");
        }
        assert_eq!(storage.signature_errors, 0);
        eprintln!("replay memory: records={count}, history_bytes={}, state_bytes={state_bytes}, peak={report}", sizes.values().sum::<u64>());
        measured.push((report["working"].as_u64().unwrap(), report["commit"].as_u64().unwrap()));
    }
    assert!(measured[1].0.saturating_sub(measured[0].0) < 64 << 20, "resident growth: {measured:?}");
    assert!(measured[1].1.saturating_sub(measured[0].1) < 96 << 20, "commit growth: {measured:?}");
}
