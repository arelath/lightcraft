#[allow(dead_code)]
#[path = "support/cloud_upload.rs"]
mod support;
use serde_json::{Value, json};

#[test]
fn relative_library_conversion_persists_absolute_originals_and_reopens_from_another_directory() {
    let f = support::Fixture::new();
    let library = f.dir.join("photos");
    let (import, source) = f.import(&library, false);
    let (output, rows) = f.run(&library, &[("cloud.upload", json!({"imports":[import]})), ("cloud.sync", json!({}))]);
    assert!(output.status.success(), "{rows:?}");
    let (_, saved, _) = lightcraft_engine::catalog::Journal::open(Box::new(lightcraft_engine::catalog::FsStore::open(&library).unwrap())).unwrap();
    let original = saved.assets().values().next().unwrap().original.clone();
    let output = f
        .command()
        .current_dir(&f.dir)
        .args(["run", "--library", "./photos", "cloud.syncDisablePreview", "{}", "cloud.syncDisable", r#"{"confirm":true,"wait":true}"#])
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let rows = String::from_utf8_lossy(&output.stdout).lines().map(|line| serde_json::from_str::<Value>(line).unwrap()).collect::<Vec<_>>();
    assert_eq!(rows[1]["result"]["stage"], "complete", "{rows:?}");
    let destination = library.join("Local Originals");
    assert_eq!(rows[0]["result"]["destination"], json!(destination));
    let mode: Value = serde_json::from_slice(&std::fs::read(library.join("cloud-mode.json")).unwrap()).unwrap();
    assert_eq!(mode["transition"]["destination"], json!(destination));
    assert_eq!(std::fs::read(destination.join("blobs").join(original.id.0)).unwrap(), std::fs::read(source).unwrap());
    f.state.lock().unwrap().requests.clear();
    let output = f
        .command()
        .current_dir(f.dir.parent().unwrap())
        .args(["run", "--library"])
        .arg(&library)
        .args(["cloud.syncStatus", "{}", "cloud.transitionStatus", "{}"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let rows = String::from_utf8_lossy(&output.stdout).lines().map(|line| serde_json::from_str::<Value>(line).unwrap()).collect::<Vec<_>>();
    assert_eq!(rows[0]["result"]["enabled"], false);
    assert_eq!(rows[1]["result"]["destination"], json!(destination));
    assert!(f.state.lock().unwrap().requests.is_empty(), "offline reopen makes no storage requests");
}
