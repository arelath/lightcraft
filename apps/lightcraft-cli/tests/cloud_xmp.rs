//! Supporting XMP survives real-binary publication, revision and fresh-device access.
#[allow(dead_code)]
#[path = "support/cloud_upload.rs"]
mod support;
use lightcraft_engine::catalog::{Catalog, Flag, FsStore, Journal, Photo, PhotoId, Source};
use serde_json::{Value, json};
use std::path::Path;
use std::sync::Arc;
use support::Fixture;

fn run(f: &Fixture, library: &Path, commands: &[(&str, Value)]) -> Vec<Value> {
    let (output, rows) = f.run(library, commands);
    assert!(output.status.success(), "{rows:?} {}", String::from_utf8_lossy(&output.stderr));
    rows
}
fn catalog(library: &Path) -> Catalog {
    Journal::open(Box::new(FsStore::open(library).unwrap())).unwrap().1
}

#[test]
fn supporting_packets_follow_revisions_and_are_readable_after_fresh_join_in_both_protocols() {
    for append in [false, true] {
        let f = Fixture::new();
        if append {
            let mut state = f.state.lock().unwrap();
            state.append_mode = true;
            state.unsupported_conditional = true;
        }
        let creator = f.dir.join("creator");
        let peer = f.dir.join("peer");
        let source = f.dir.join("shared.png");
        let image = lightcraft_raster::Rgba8::from_fn(32, 24, |x, y| [x as u8 * 5, y as u8 * 7, 100, 255]);
        std::fs::write(&source, lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&image), &Default::default()).unwrap()).unwrap();
        let mut template = Photo::new(PhotoId(1), Source::File { path: "shared.png".into() }, "shared.png", "PNG", 32, 24, "");
        template.rating = 4;
        template.flag = Flag::Pick;
        template.meta.title = "Harbour".into();
        Arc::make_mut(&mut template.develop).light.exposure = 0.75;
        let foreign = r#"<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description xmlns:foreign="urn:lightcraft:test:foreign" foreign:note="retained-on-new-device"/></rdf:RDF></x:xmpmeta>"#;
        let packet = lightcraft_engine::sidecar::merge_managed_packet(
            foreign.as_bytes(),
            &lightcraft_engine::sidecar::sidecar_packet(&template, &Catalog::new()),
        )
        .unwrap();
        std::fs::write(source.with_extension("xmp"), &packet).unwrap();
        let config = json!({"id":"personal","bucket":"photos","region":"us-east-1","prefix":"library","endpoint":f.endpoint,"profile":"acceptance"});
        let rows = run(
            &f,
            &creator,
            &[
                ("cloud.configure", config.clone()),
                ("cloud.syncEnable", json!({"connection":"personal","protocol":if append {"append-log-v1"} else {"conditional-head-v1"}})),
                ("cloud.import", json!({"connection":"personal","paths":[source]})),
                ("cloud.upload", json!({})),
                ("photo.rate", json!({"ids":[1],"rating":2})),
                ("photo.saveMetadataToFile", json!({"ids":[1]})),
                ("cloud.upload", json!({})),
                ("photo.editExternal", json!({"ids":[1],"format":"tiff","bitDepth":16})),
            ],
        );
        let library = rows[1]["result"]["library"].clone();
        let derived = rows[7]["result"]["id"].as_u64().unwrap();
        let working = std::path::PathBuf::from(rows[7]["result"]["path"].as_str().unwrap());
        assert_eq!(catalog(&creator).photo(PhotoId(derived)).unwrap().develop.light.exposure, 0.0);
        std::fs::remove_file(source.with_extension("xmp")).unwrap();
        std::fs::remove_file(source).unwrap();
        let returned = lightcraft_codecs::encode_png(
            &lightcraft_codecs::EncodeImage::rgba8(&lightcraft_raster::Rgba8::from_fn(13, 9, |_, _| [60, 120, 180, 255])),
            &Default::default(),
        )
        .unwrap();
        std::fs::write(working, returned).unwrap();
        run(&f, &creator, &[("photo.reload", json!({"ids":[derived]})), ("cloud.upload", json!({})), ("cloud.sync", json!({}))]);
        let creator_catalog = catalog(&creator);
        let derived_photo = creator_catalog.photo(PhotoId(derived)).unwrap();
        let Source::Managed { asset_id } = &derived_photo.source else { panic!("managed") };
        let associated = creator_catalog.asset(asset_id).unwrap().associated["xmp"].clone();
        let key = format!("/photos/{}", associated.remote.key);
        {
            let state = f.state.lock().unwrap();
            let bytes = &state.objects[&key];
            assert_eq!(support::hash(bytes), associated.id.0);
            assert!(std::str::from_utf8(bytes).unwrap().contains("retained-on-new-device"));
        }
        run(
            &f,
            &peer,
            &[
                ("cloud.configure", config),
                (
                    "cloud.syncEnable",
                    json!({"connection":"personal","library":library,"protocol":if append {"append-log-v1"} else {"conditional-head-v1"}}),
                ),
                ("cloud.sync", json!({})),
            ],
        );
        let peer_id = catalog(&peer).photos().find(|p| p.file_name == derived_photo.file_name).unwrap().id;
        let start = f.state.lock().unwrap().requests.len();
        run(
            &f,
            &peer,
            &[
                ("photo.readMetadataFromFile", json!({"ids":[peer_id.0]})),
                ("app.export", json!({"ids":[peer_id.0],"path":f.dir.join("peer-export.png"),"format":"png"})),
            ],
        );
        let peer_catalog = catalog(&peer);
        let photo = peer_catalog.photo(peer_id).unwrap();
        assert_eq!((photo.width, photo.height), (13, 9));
        assert_eq!(photo.meta.title, "Harbour");
        assert_eq!((photo.rating, photo.flag), (2, Flag::Pick));
        assert_eq!(photo.develop.light.exposure, 0.0, "readback cannot reapply the baked source edit");
        let state = f.state.lock().unwrap();
        assert_eq!(state.signature_errors, 0);
        assert!(
            state.requests[start..].iter().any(|(method, path)| method == "GET" && path == &key),
            "fresh device must acquire the retained document"
        );
    }
}
