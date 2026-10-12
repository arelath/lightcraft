//! Acquire remote sources before synchronous native UI commands. The command resumes on the
//! owner thread with leases held, and native dispatch prohibits further network waits.

use crate::{LightcraftApp, control};
use lightcraft_catalog::{PhotoId, Source, assets::Asset};
use lightcraft_engine::assets::{AssetAvailability, AssetLease, AssetResolver, SourceRequirement, TransferManager};
use serde_json::{Value, json};
use std::sync::Arc;

struct Plan {
    resolver: Arc<AssetResolver>,
    assets: Vec<(PhotoId, Source, Asset)>,
    requirement: SourceRequirement,
    library: Arc<()>,
    params: Value,
}

fn plan(app: &LightcraftApp, command: &str, params: &Value) -> Option<Plan> {
    if app.headless_host {
        return None;
    }
    let expanded;
    let params = if command == "app.export" {
        expanded = app.session.export_params(params).ok()?;
        &expanded
    } else {
        params
    };
    let single = matches!(command, "ui.render" | "dialog.allMetadata" | "photo.allMetadata");
    let requirement = match command {
        "ui.render" if params.get("size").and_then(Value::as_u64).unwrap_or(1600) <= 2560 => SourceRequirement::PreviewAllowed,
        "ui.render" | "dialog.allMetadata" | "photo.allMetadata" | "photo.editExternal" | "photo.editInExternal" | "photo.editInPhotoCraft" => {
            SourceRequirement::OriginalRequired
        }
        "app.export" if params.get("background").and_then(Value::as_bool) != Some(true) => {
            if params.get("allowSmartPreview").and_then(Value::as_bool) == Some(true) {
                SourceRequirement::PreviewAllowed
            } else {
                SourceRequirement::OriginalRequired
            }
        }
        _ => return None,
    };
    let mut captured = params.as_object()?.clone();
    if params.get("id").is_some_and(|id| id.as_u64().is_none()) {
        return None; // malformed commands go through normal validation without acquisition
    }
    let ids =
        if single { vec![params.get("id").and_then(Value::as_u64).map(PhotoId).or(app.session.active())?] } else { app.session.targets(params) };
    if ids.is_empty() || ids.len() > 1000 {
        return None;
    }
    if single {
        captured.insert("id".into(), json!(ids.first()?.0));
    } else {
        captured.insert("ids".into(), json!(ids.iter().map(|id| id.0).collect::<Vec<_>>()));
    }
    let resolver = app.session.media.asset_resolver.clone()?;
    let assets: Vec<_> = ids
        .into_iter()
        .filter_map(|id| {
            let photo = app.session.catalog.photo(id)?;
            let Source::Managed { asset_id } = &photo.source else { return None };
            let asset = app.session.catalog.asset(asset_id)?.clone();
            Some((id, photo.source.clone(), asset))
        })
        .collect();
    let ready = |asset: &Asset| {
        resolver.availability(&asset.original.id) == AssetAvailability::Cached
            || (requirement == SourceRequirement::PreviewAllowed
                && asset.smart_preview.as_ref().is_some_and(|blob| resolver.availability(&blob.id) == AssetAvailability::Cached))
    };
    if assets.is_empty() || assets.iter().all(|(_, _, asset)| ready(asset)) {
        return None;
    }
    Some(Plan { resolver, assets, requirement, library: app.session.library_token(), params: Value::Object(captured) })
}

fn still_current(app: &LightcraftApp, library: &Arc<()>, sources: &[(PhotoId, Source, Asset)]) -> bool {
    Arc::ptr_eq(library, &app.session.library_token())
        && sources.iter().all(|(id, source, asset)| {
            let Source::Managed { asset_id } = source else { return false };
            app.session.catalog.photo(*id).is_some_and(|p| !p.deleted && &p.source == source) && app.session.catalog.asset(asset_id) == Some(asset)
        })
}

fn start(
    app: &mut LightcraftApp,
    plan: Plan,
    finish: impl FnOnce(&mut LightcraftApp, &egui::Context, Value, Result<Vec<AssetLease>, String>) + Send + 'static,
) -> Result<u64, String> {
    const LABEL: &str = "Acquire Cloud Sources";
    if app.tasks.labels().iter().filter(|label| label.as_str() == LABEL).count() >= 32 {
        return Err("too many commands waiting for cloud sources; cancel a download and retry".into());
    }
    let task = app.session.activity.start("download", LABEL, lightcraft_engine::activity::Cancel::Yes);
    let task_id = task.id();
    let cancel = task.cancel_flag().ok_or("could not create source-acquisition cancellation flag")?;
    task.set_unit(lightcraft_engine::activity::Unit::Bytes);
    let handle = task.handle();
    let Plan { resolver, assets, requirement, library, params } = plan;
    let captured_sources = assets.clone();
    crate::tasks::spawn(
        app,
        LABEL,
        None,
        move || {
            lightcraft_engine::assets::transfers::with_priority(lightcraft_engine::assets::transfers::Priority::ActivePhoto, || {
                lightcraft_engine::assets::transfers::with_progress(handle, || {
                    let mut leases = Vec::with_capacity(assets.len());
                    for (_, _, asset) in &assets {
                        let (lease, _) = resolver.acquire_asset(asset, requirement, &cancel).map_err(|e| e.to_string())?;
                        leases.push(lease);
                    }
                    drop(task);
                    Ok(leases)
                })
            })
        },
        move |app, ctx, result: Result<Vec<AssetLease>, String>| {
            let result = if still_current(app, &library, &captured_sources) {
                result
            } else {
                Err("the library or cloud sources changed while downloading; retry the command".into())
            };
            finish(app, ctx, params, result);
        },
    )?;
    Ok(task_id)
}

/// GUI commands return a background task; their normal side effects happen after acquisition.
pub fn defer_command(app: &mut LightcraftApp, command: &str, params: &Value) -> Option<Result<Value, String>> {
    let plan = plan(app, command, params)?;
    let command = command.to_string();
    Some(
        start(app, plan, move |app, ctx, params, leases| match leases {
            Ok(_leases) => {
                if let Err(error) = app.run(&command, params) {
                    app.toast_error(ctx, error);
                }
            }
            Err(error) => app.toast_error(ctx, error),
        })
        .map(|task| json!({"background": true, "task": task})),
    )
}

/// Control clients keep their ordinary response contract while the UI keeps answering.
pub fn defer_control(app: &mut LightcraftApp, request: &control::ControlRequest) -> Option<Result<(), String>> {
    let (command, params) = match request.method.as_str() {
        "ui.render" => ("ui.render", request.params.clone()),
        "engine.execute" | "ui.menu.invoke" | "command" => {
            let command = request.params.get("command").or_else(|| request.params.get("id"))?.as_str()?;
            (command, request.params.get("params").cloned().filter(|v| !v.is_null()).unwrap_or(json!({})))
        }
        _ => return None,
    };
    let plan = plan(app, command, &params)?;
    let mut request = request.clone();
    Some(
        start(app, plan, move |app, ctx, params, leases| {
            let response = match leases {
                Ok(_leases) => {
                    if request.method == "ui.render" {
                        request.params = params;
                    } else if let Some(p) = request.params.as_object_mut() {
                        p.insert("params".into(), params);
                    }
                    match control::handle(app, ctx, &request) {
                        control::Outcome::Done(value) => value,
                        _ => json!({"ok": false, "error": "source acquisition did not settle; retry the command"}),
                    }
                }
                Err(error) => json!({"ok": false, "error": error}),
            };
            let _ = request.reply.send(response);
        })
        .map(|_| ()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use lightcraft_catalog::{
        Catalog, Op, Photo,
        assets::{AssetId, Blob, RemoteLocation},
    };
    use lightcraft_engine::{
        Selection, Session,
        assets::{AssetError, BlobStore},
    };
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    static NEXT: AtomicUsize = AtomicUsize::new(0);
    struct SlowStore {
        bytes: Vec<u8>,
        released: Arc<AtomicBool>,
    }
    impl BlobStore for SlowStore {
        fn download(&self, _: &Blob, out: &mut dyn std::io::Write, cancel: &AtomicBool, _: &mut dyn FnMut(u64)) -> Result<(), AssetError> {
            while !self.released.load(Ordering::Relaxed) {
                if cancel.load(Ordering::Relaxed) {
                    return Err(AssetError::Cancelled);
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            out.write_all(&self.bytes).map_err(|e| AssetError::Disk(e.to_string()))
        }
    }
    struct Fixture {
        root: std::path::PathBuf,
        app: LightcraftApp,
        released: Arc<AtomicBool>,
    }
    impl Fixture {
        fn new() -> Self {
            let root = std::env::temp_dir().join(format!("lightcraft-cloud-ui-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
            let image = lightcraft_raster::Rgba8::from_fn(4, 3, |_, _| [40, 80, 120, 255]);
            let bytes = lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&image), &Default::default()).unwrap();
            let resolver = Arc::new(AssetResolver::new(root.clone()));
            let released = Arc::new(AtomicBool::new(false));
            resolver.connect("test".into(), Arc::new(SlowStore { bytes: bytes.clone(), released: released.clone() })).unwrap();
            let mut session = Session::new().with_fs();
            session.media.asset_resolver = Some(resolver);
            let asset_id = AssetId("first".into());
            let asset = Asset {
                original: Blob {
                    id: lightcraft_engine::assets::sha256(&bytes),
                    size: bytes.len() as u64,
                    remote: RemoteLocation { connection: "test".into(), key: "library/original.png".into(), version: None },
                },
                smart_preview: None,
                thumbnail: None,
            };
            session.catalog.apply(Op::SetAsset { id: asset_id.clone(), asset: Some(Box::new(asset)) }).unwrap();
            let photo = Photo::new(PhotoId(1), Source::Managed { asset_id }, "first.png", "PNG", 4, 3, "");
            session.catalog.apply(Op::AddPhoto { photo: Box::new(photo) }).unwrap();
            session.selection = Selection::single(PhotoId(1));
            Self { root, app: LightcraftApp::new(session, crate::Services::default()), released }
        }
        fn request(&mut self, command: &str) -> std::sync::mpsc::Receiver<Value> {
            let (request, rx) = control::ControlRequest::new("engine.execute", json!({"command": command, "params": {}}));
            assert!(matches!(control::handle(&mut self.app, &egui::Context::default(), &request), control::Outcome::Deferred));
            rx
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            self.released.store(true, Ordering::Relaxed);
            crate::tasks::wait(&mut self.app, &egui::Context::default(), Duration::from_secs(5));
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn foreground_export_acquisition_expands_proxy_presets_and_respects_overrides() {
        let mut fixture = Fixture::new();
        fixture.app.session.export_presets.push(lightcraft_engine::export::ExportPreset {
            name: "Preview export".into(),
            params: json!({"format": "png", "allowSmartPreview": true}),
        });
        let proxy = plan(&fixture.app, "app.export", &json!({"preset": "Preview export"})).unwrap();
        assert_eq!(proxy.requirement, SourceRequirement::PreviewAllowed);
        assert_eq!(proxy.params["allowSmartPreview"], true);
        assert!(proxy.params.get("preset").is_none());
        let original = plan(&fixture.app, "app.export", &json!({"preset": "Preview export", "allowSmartPreview": false})).unwrap();
        assert_eq!(original.requirement, SourceRequirement::OriginalRequired);
        fixture.app.session.last_export = Some(json!({"allowSmartPreview": true}));
        assert_eq!(plan(&fixture.app, "app.export", &json!({})).unwrap().requirement, SourceRequirement::OriginalRequired);
        assert!(plan(&fixture.app, "app.export", &json!({"preset": "missing"})).is_none());
    }

    #[test]
    fn metadata_acquisition_keeps_control_responsive_and_captures_the_target() {
        let mut fixture = Fixture::new();
        let reply = fixture.request("dialog.allMetadata");
        let (inspect, _) = control::ControlRequest::new("ui.inspect", json!({}));
        let before = Instant::now();
        assert!(matches!(control::handle(&mut fixture.app, &egui::Context::default(), &inspect), control::Outcome::Done(_)));
        assert!(before.elapsed() < Duration::from_secs(1));
        assert!(matches!(reply.try_recv(), Err(std::sync::mpsc::TryRecvError::Empty)));
        fixture.app.session.selection = Selection::default();
        fixture.released.store(true, Ordering::Relaxed);
        assert!(crate::tasks::wait(&mut fixture.app, &egui::Context::default(), Duration::from_secs(5)));
        let reply = reply.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_eq!(reply["ok"], true, "{reply}");
        let Some(crate::state::Dialog::AllMetadata { title, .. }) = &fixture.app.ui.dialog else {
            panic!("metadata dialog must open after acquisition")
        };
        assert_eq!(title, "first.png");
    }

    #[test]
    fn switching_libraries_during_acquisition_never_replays_the_old_command() {
        let mut fixture = Fixture::new();
        let reply = fixture.request("dialog.allMetadata");
        fixture
            .app
            .session
            .open_library_in(
                lightcraft_engine::library::LibraryStores {
                    dir: "other-library".into(),
                    catalog: Box::new(lightcraft_catalog::MemStore::new()),
                    files: Box::new(lightcraft_catalog::MemStore::new()),
                    on_disk: false,
                },
                false,
            )
            .unwrap();
        assert_eq!(fixture.app.session.catalog.len(), Catalog::new().len());
        fixture.released.store(true, Ordering::Relaxed);
        assert!(crate::tasks::wait(&mut fixture.app, &egui::Context::default(), Duration::from_secs(5)));
        let reply = reply.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_eq!(reply["ok"], false);
        assert!(reply["error"].as_str().unwrap().contains("library or cloud sources changed"));
        assert!(fixture.app.ui.dialog.is_none());
    }

    #[test]
    fn cancelling_acquisition_does_not_run_the_waiting_command() {
        let mut fixture = Fixture::new();
        let reply = fixture.request("dialog.allMetadata");
        fixture.app.session.execute("activity.cancel", &json!({"all": true})).unwrap();
        assert!(crate::tasks::wait(&mut fixture.app, &egui::Context::default(), Duration::from_secs(5)));
        assert_eq!(reply.recv_timeout(Duration::from_secs(1)).unwrap()["ok"], false);
        assert!(fixture.app.ui.dialog.is_none());
    }
}
