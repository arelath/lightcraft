//! Local-only catalog edits rejoin against their original causal base.
#[allow(dead_code)]
#[path = "support/cloud_upload.rs"]
mod support;
use lightcraft_engine::catalog::{Catalog, FsStore, Journal, PhotoId, Source};
use serde_json::{Value, json};
use std::path::Path;

fn catalog(path: &Path) -> Catalog {
    Journal::open(Box::new(FsStore::open(path).unwrap())).unwrap().1
}
fn run(f: &support::Fixture, path: &Path, commands: &[(&str, Value)]) -> Vec<Value> {
    let (output, rows) = f.run(path, commands);
    assert!(output.status.success(), "{rows:?}: {}", String::from_utf8_lossy(&output.stderr));
    rows
}

#[test]
fn local_edits_new_photos_and_concurrent_remote_values_survive_rejoin_on_both_protocols() {
    for protocol in ["conditional-head-v1", "append-log-v1"] {
        let f = support::Fixture::new();
        f.state.lock().unwrap().append_mode = protocol == "append-log-v1";
        let local = f.dir.join("local");
        let peer = f.dir.join("peer");
        let (import, _) = f.import_unsynchronized(&local, false);
        run(
            &f,
            &local,
            &[
                ("cloud.syncEnable", json!({"connection":"personal","protocol":protocol})),
                ("cloud.upload", json!({"imports":[import]})),
                ("cloud.sync", json!({})),
            ],
        );
        let library = catalog(&local).sync_state().unwrap().library.clone();
        run(
            &f,
            &peer,
            &[
                (
                    "cloud.configure",
                    json!({"id":"personal","bucket":"photos","region":"us-east-1","prefix":"library","endpoint":f.endpoint,"profile":"acceptance"}),
                ),
                ("cloud.syncEnable", json!({"connection":"personal","library":library,"protocol":protocol})),
                ("cloud.sync", json!({})),
            ],
        );
        assert_eq!(run(&f, &local, &[("cloud.syncDisable", json!({"confirm":true,"wait":true}))])[0]["result"]["stage"], "complete");
        let source = f.dir.join("new.png");
        let image = lightcraft_raster::Rgba8::from_fn(32, 24, |x, y| [x as u8 * 4, y as u8 * 6, 75, 255]);
        std::fs::write(&source, lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&image), &Default::default()).unwrap()).unwrap();
        let frozen = catalog(&local).sync_state().unwrap().clone();
        let requests = f.state.lock().unwrap().requests.len();
        run(
            &f,
            &local,
            &[
                ("photo.rate", json!({"ids":[1],"rating":2})),
                ("library.import", json!({"paths":[source]})),
                ("photo.rate", json!({"ids":[2],"rating":4})),
            ],
        );
        assert_eq!(f.state.lock().unwrap().requests.len(), requests);
        assert_eq!(catalog(&local).sync_state().unwrap(), &frozen);
        assert!(catalog(&local).local_sync_baseline().is_some());
        run(&f, &peer, &[("photo.rate", json!({"ids":[1],"rating":5})), ("cloud.sync", json!({}))]);
        run(&f, &local, &[("cloud.syncEnable", json!({"connection":"personal"})), ("cloud.sync", json!({}))]);
        let joined = catalog(&local);
        assert!(joined.local_sync_baseline().is_none());
        let entity = format!("photo:{}", joined.cloud_identity("photo:1").unwrap());
        let choices = &joined.sync_state().unwrap().documents[&entity]["rating"].0;
        assert!(choices.values().any(|value| value == 2));
        assert!(choices.values().any(|value| value == 5));
        assert_eq!(joined.photo(PhotoId(2)).unwrap().rating, 4);
        assert!(matches!(joined.photo(PhotoId(2)).unwrap().source, Source::File { .. }));
        run(&f, &local, &[("cloud.adopt", json!({"connection":"personal","ids":[2],"wait":true}))]);
        let imports =
            catalog(&local).upload_intents().iter().filter(|(_, intent)| intent.photo_id == PhotoId(2)).map(|(id, _)| id.clone()).collect::<Vec<_>>();
        assert!(!imports.is_empty());
        run(&f, &local, &[("cloud.upload", json!({"imports":imports})), ("cloud.sync", json!({}))]);
        run(&f, &peer, &[("cloud.sync", json!({}))]);
        let received = catalog(&peer);
        let new = received.photos().find(|photo| photo.file_name == "new.png").unwrap();
        assert_eq!(new.rating, 4);
        assert!(matches!(new.source, Source::Managed { .. }));
        assert_eq!(f.state.lock().unwrap().signature_errors, 0);
    }
}
