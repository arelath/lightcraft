use super::*;
use crate::{Services, headless::Headless, state::ViewMode};
use lightcraft_catalog::{
    Op,
    replication::{Change, SyncState},
};
use std::time::Duration;
const TIMEOUT: Duration = Duration::from_secs(60);
fn fixture(size: [f32; 2]) -> (Headless, Value, Value) {
    let mut session = lightcraft_engine::Session::with_demo();
    let id = session.active().unwrap();
    let mut retained = (*session.develop_of(id).unwrap()).clone();
    retained.light.exposure = 2.;
    retained.orientation = lightcraft_geom::Orientation::Rotate90;
    retained.crop.geometry.rect = lightcraft_geom::Rect { x0: 0.15, y0: 0.2, x1: 0.85, y1: 0.6 };
    let mut state = SyncState::new("preview-library".into(), "this-device".into(), "personal".into()).unwrap();
    for (writer, develop, caption) in [
        ("writer-a", serde_json::to_value(session.develop_of(id).unwrap()).unwrap(), "Same prefix ".repeat(20) + "full first caption tail"),
        ("writer-b", serde_json::to_value(&retained).unwrap(), "Same prefix ".repeat(20) + "full second caption tail"),
    ] {
        let mut peer = SyncState::new("preview-library".into(), writer.into(), "personal".into()).unwrap();
        let changes = [("develop", develop), ("meta.caption", json!(caption))]
            .into_iter()
            .map(|(field, value)| Change { entity: "photo:retained".into(), field: field.into(), base: None, value })
            .collect();
        state.receive(&peer.enqueue(changes).unwrap().unwrap()).unwrap();
    }
    session
        .apply_received(Op::Batch {
            ops: vec![
                Op::SetSyncState { state: Some(Box::new(state)) },
                Op::SetCloudIdentity { entity: format!("photo:{}", id.0), id: Some("retained".into()) },
            ],
        })
        .unwrap();
    let query = |field| json!({"entity":"photo:retained","field":field,"revision":"writer-b-1"});
    let mut h = Headless::new(LightcraftApp::new(session, Services::default()), size, 1.);
    h.step();
    (h, query("develop"), query("meta.caption"))
}
fn open(h: &mut Headless, query: &Value, mode: &str) {
    let mut query = query.clone();
    query["mode"] = json!(mode);
    h.app.run("dialog.cloudChoice", query).unwrap();
    assert!(h.settle(TIMEOUT));
}
#[test]
fn cloud_choice_full_metadata_and_exact_compare_are_read_only() {
    let (mut h, develop, caption) = fixture([1200., 900.]);
    let before = h.app.session.catalog.to_snapshot();
    let active = h.app.session.active();
    let original_before = h.app.session.before.clone();
    h.app.run("dialog.cloudConflicts", json!({})).unwrap();
    open(&mut h, &caption, "details");
    assert!(h.painted_text().iter().any(|text| text.contains("full second caption tail")));
    let Some(Dialog::CloudChoice { view }) = &h.app.ui.dialog else { panic!("choice dialog") };
    assert_eq!(view.choice.device.as_deref(), Some("writer-b"));
    assert_eq!(view.choice.sequence, Some(1));
    h.request("ui.dialog.confirm", json!({}), TIMEOUT);
    assert_eq!(h.app.ui.dialog, Some(Dialog::CloudConflicts));
    open(&mut h, &develop, "compare");
    let current = h.app.renderer.textures.get(&Slot::CloudChoice(0)).unwrap();
    let retained = h.app.renderer.textures.get(&Slot::CloudChoice(1)).unwrap();
    assert_ne!(current.key, retained.key);
    let aspect = |size: [usize; 2]| size[0] as f32 / size[1] as f32;
    assert!((aspect(current.size) - aspect(retained.size)).abs() > 0.2, "each pane uses its own crop and rotation");
    assert_eq!(h.app.session.active(), active);
    assert_eq!(h.app.session.before, original_before);
    assert!(h.app.session.catalog.to_snapshot() == before, "viewing never changes catalog, history or outbox");
    h.request("ui.dialog.confirm", json!({}), TIMEOUT);
    assert_eq!(h.app.ui.dialog, Some(Dialog::CloudConflicts));
    assert!(h.app.renderer.wanted(Slot::CloudChoice(1)).is_none());
}
#[test]
fn cloud_choice_preview_closes_and_retires_renders_after_context_changes() {
    for change in 0..5 {
        let (mut h, query, _) = fixture([900., 700.]);
        let id = h.app.session.active().unwrap();
        if change == 0 {
            let mut request = query.clone();
            request["mode"] = json!("preview");
            h.app.run("dialog.cloudChoice", request).unwrap();
            h.step();
            assert!(h.app.renderer.wanted(Slot::CloudChoice(1)).is_some(), "preview requested before cancellation");
        } else {
            open(&mut h, &query, "preview");
        }
        match change {
            0 => h.app.ui.view = if h.app.ui.view == ViewMode::Detail { ViewMode::PhotoGrid } else { ViewMode::Detail },
            1 => h.app.session.selection.active = None,
            2 => {
                h.app.session.apply_received(Op::SetRating { id, rating: 3 }).unwrap();
                // Unrelated metadata is allowed; actual settings changes close the preview.
                assert!(matches!(&h.app.ui.dialog,Some(Dialog::CloudChoice{view}) if view.valid(&h.app)));
                h.app.session.selection.active = Some(id);
                let before = h.app.session.catalog.photo(id).unwrap().develop.hash64();
                let exposure = h.app.session.catalog.photo(id).unwrap().develop.light.exposure;
                let exposure = if exposure > 0. { exposure - 0.25 } else { exposure + 0.25 };
                let mut settings = (*h.app.session.catalog.photo(id).unwrap().develop).clone();
                settings.light.exposure = exposure;
                h.app
                    .session
                    .apply_received(Op::SetDevelop { id, settings: std::sync::Arc::new(settings), label: "received edit".into(), edited: None })
                    .unwrap();
                assert_ne!(h.app.session.catalog.photo(id).unwrap().develop.hash64(), before);
            }
            3 => {
                h.app
                    .session
                    .apply_received(Op::Relink {
                        id,
                        file_name: "replacement".into(),
                        source: Source::File { path: "unavailable-replacement.png".into() },
                        format: None,
                    })
                    .unwrap();
            }
            _ => {
                let mut state = h.app.session.catalog.sync_state().unwrap().clone();
                state.documents.clear();
                h.app.session.apply_received(Op::SetSyncState { state: Some(Box::new(state)) }).unwrap();
            }
        }
        observe(&mut h.app);
        assert!(h.app.ui.dialog.is_none(), "context change {change}");
        assert!(h.app.renderer.wanted(Slot::CloudChoice(1)).is_none());
        assert!(h.app.renderer.failure(Slot::CloudChoice(1)).is_none(), "closed previews discard failure state too");
        assert!(h.settle(TIMEOUT));
        assert!(!h.app.renderer.textures.contains_key(&Slot::CloudChoice(1)), "late completion cannot restore the closed pane");
    }
}
#[test]
fn cloud_choice_small_window_keeps_done_reachable_and_library_switch_dismisses_it() {
    use lightcraft_catalog::MemStore;
    use lightcraft_engine::library::LibraryStores;
    let (mut h, _, caption) = fixture([480., 460.]);
    open(&mut h, &caption, "details");
    let widgets = h.request("ui.widgets", json!({"filter":"button:dialogOk"}), TIMEOUT);
    let done = widgets["result"].as_array().unwrap().iter().find(|widget| widget["id"] == "button:dialogOk").unwrap();
    assert!(done["rect"][1].as_f64().unwrap() + done["rect"][3].as_f64().unwrap() <= 460., "{done}");
    h.app
        .session
        .open_library_in(
            LibraryStores {
                dir: "second-preview-library".into(),
                catalog: Box::new(MemStore::new()),
                files: Box::new(MemStore::new()),
                on_disk: false,
            },
            false,
        )
        .unwrap();
    h.step();
    assert!(h.app.ui.dialog.is_none());
    assert!(h.app.renderer.wanted(Slot::CloudChoice(1)).is_none());
}
