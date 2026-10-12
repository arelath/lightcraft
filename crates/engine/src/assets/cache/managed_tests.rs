use super::*;
use crate::Session;
use crate::assets::AssetResolver;
use lightcraft_catalog::assets::Asset;
use lightcraft_catalog::uploads::UploadState;
use lightcraft_catalog::{Op, Source};
use serde_json::{Value, json};
use std::sync::atomic::{AtomicBool, AtomicUsize};

static NEXT: AtomicUsize = AtomicUsize::new(0);
struct Fixture {
    root: PathBuf,
    session: Session,
    resolver: Arc<AssetResolver>,
    id: String,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("lightcraft-staging-retention-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        std::fs::create_dir_all(&root).unwrap();
        let pixels = lightcraft_raster::Rgba8::from_fn(32, 24, |x, y| [x as u8 * 5, y as u8 * 7, 100, 255]);
        let png = lightcraft_codecs::encode_png(&lightcraft_codecs::EncodeImage::rgba8(&pixels), &Default::default()).unwrap();
        std::fs::write(root.join("source.png"), png).unwrap();
        let mut session = Session::new().with_fs();
        session.open_library(root.join("library"), false).unwrap();
        session.execute("cloud.configure",&json!({"id":"personal","bucket":"photos","region":"us-east-1","prefix":"library","endpoint":"http://127.0.0.1:9","profile":"disabled"})).unwrap();
        session.execute("cloud.import", &json!({"connection":"personal","paths":[root.join("source.png")]})).unwrap();
        let id = session.catalog.upload_intents().keys().next().unwrap().clone();
        let resolver = session.media.asset_resolver.clone().unwrap();
        Self { root, session, resolver, id }
    }
    fn managed(&self) -> PathBuf {
        self.root.join("library/Managed")
    }
    fn asset(&self, id: &str) -> Asset {
        self.session.catalog.asset(&self.session.catalog.upload_intents()[id].asset_id).unwrap().clone()
    }
    fn mark(&mut self, id: &str, persist: bool) {
        let mut intent = self.session.catalog.upload_intents()[id].clone();
        let asset = self.asset(id);
        let keys: Vec<_> = crate::assets::managed::blobs(&asset).map(|blob| blob.remote.key.clone()).collect();
        let connection = self
            .session
            .library
            .as_mut()
            .unwrap()
            .cloud_connections()
            .unwrap()
            .into_iter()
            .find(|connection| connection.id == asset.original.remote.connection)
            .unwrap();
        let photo = self.session.catalog.photo(intent.photo_id).unwrap().as_ref().clone();
        let receipt = json!({"version":2,"id":id,"imported":{"mutationId":id,"photoId":id,"assetId":intent.asset_id,"photo":photo,"asset":asset},"storage":connection.storage_snapshot(),"state":"synchronized","verified":keys,"multipart":{},"generation":1,"error":null});
        std::fs::create_dir_all(self.managed().join("uploads")).unwrap();
        lightcraft_catalog::safe_file::write_atomic(
            &self.managed().join("uploads").join(format!("{id}.json")),
            &serde_json::to_vec(&receipt).unwrap(),
        )
        .unwrap();
        intent.state = UploadState::Synchronized;
        intent.verified = keys.into_iter().collect();
        self.session.apply_system(Op::SetUploadIntent { id: id.into(), intent: Some(Box::new(intent)) }).unwrap();
        if persist {
            self.session.persist().unwrap();
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = self.session.close_library();
        let _maintenance = self.resolver.cache.maintenance.lock().unwrap_or_else(PoisonError::into_inner);
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn only_durable_acknowledgement_makes_verified_staging_disposable() {
    let mut fixture = Fixture::new();
    let resolver = fixture.resolver.clone();
    let _maintenance = resolver.cache.maintenance.lock().unwrap();
    let id = fixture.id.clone();
    let asset = fixture.asset(&id);
    let path = fixture.managed().join("blobs").join(&asset.original.id.0);
    let bytes = std::fs::read(&path).unwrap();
    fixture.mark(&id, false);
    resolver.cache.promote_staging().unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    fixture.session.persist().unwrap();
    resolver.cache.promote_staging().unwrap();
    assert!(!path.exists());
    assert_eq!(std::fs::read(resolver.root.join(&asset.original.id.0)).unwrap(), bytes);
    assert!(matches!(fixture.session.catalog.photo(lightcraft_catalog::PhotoId(1)).unwrap().source, Source::Managed { .. }));
}

#[test]
fn active_leases_defer_promotion_and_pins_survive_the_verified_copy() {
    let mut fixture = Fixture::new();
    let resolver = fixture.resolver.clone();
    let _maintenance = resolver.cache.maintenance.lock().unwrap();
    let id = fixture.id.clone();
    let asset = fixture.asset(&id);
    let lease = resolver.acquire_cached(&asset.original, &AtomicBool::new(false)).unwrap().unwrap();
    let original = lease.path().to_owned();
    let bytes = lease.read_bytes().unwrap();
    resolver
        .set_offline_pins(&[("photo:1".into(), Some(OfflinePin { quality: OfflineQuality::Original, blobs: vec![asset.original.id.clone()] }))])
        .unwrap();
    fixture.mark(&id, true);
    resolver.cache.promote_staging().unwrap();
    assert_eq!(std::fs::read(&original).unwrap(), bytes);
    drop(lease);
    resolver.cache.promote_staging().unwrap();
    assert!(!original.exists());
    assert_eq!(resolver.acquire_cached(&asset.original, &AtomicBool::new(false)).unwrap().unwrap().read_bytes().unwrap(), bytes);
    assert!(resolver.evict(&asset.original).is_err(), "offline pin protects the promoted bytes");
}

#[test]
fn another_pending_import_to_a_different_bucket_protects_shared_blobs() {
    let mut fixture = Fixture::new();
    let resolver = fixture.resolver.clone();
    let _maintenance = resolver.cache.maintenance.lock().unwrap();
    let id = fixture.id.clone();
    let asset = fixture.asset(&id);
    fixture.mark(&id, true);
    fixture.session.execute("cloud.configure",&json!({"id":"archive","bucket":"archive-photos","region":"us-east-1","prefix":"library","endpoint":"http://127.0.0.1:9","profile":"disabled"})).unwrap();
    fixture.session.execute("cloud.import", &json!({"connection":"archive","paths":[fixture.root.join("source.png")]})).unwrap();
    let other = fixture.session.catalog.upload_intents().keys().find(|value| *value != &id).unwrap().clone();
    assert_eq!(fixture.asset(&other).original.id, asset.original.id);
    resolver.cache.promote_staging().unwrap();
    assert!(fixture.managed().join("blobs").join(&asset.original.id.0).is_file());
    fixture.mark(&other, true);
    resolver.cache.promote_staging().unwrap();
    assert!(!fixture.managed().join("blobs").join(&asset.original.id.0).exists());
}

#[test]
fn failed_copy_corruption_and_missing_destination_proof_keep_the_original() {
    let mut fixture = Fixture::new();
    let resolver = fixture.resolver.clone();
    let _maintenance = resolver.cache.maintenance.lock().unwrap();
    let id = fixture.id.clone();
    let asset = fixture.asset(&id);
    let path = fixture.managed().join("blobs").join(&asset.original.id.0);
    let bytes = std::fs::read(&path).unwrap();
    fixture.mark(&id, true);
    let fault = lightcraft_catalog::safe_file::fail_writes_after(0);
    assert!(resolver.cache.promote_staging().is_err());
    drop(fault);
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    let receipt_path = fixture.managed().join("uploads").join(format!("{id}.json"));
    let original = std::fs::read(&receipt_path).unwrap();
    let mut legacy: Value = serde_json::from_slice(&original).unwrap();
    legacy["version"] = json!(1);
    legacy.as_object_mut().unwrap().remove("storage");
    std::fs::write(&receipt_path, serde_json::to_vec(&legacy).unwrap()).unwrap();
    resolver.cache.promote_staging().unwrap();
    assert!(path.is_file());
    std::fs::write(&receipt_path, &original).unwrap();
    std::fs::write(&path, b"corrupted staging").unwrap();
    assert!(resolver.cache.promote_staging().is_err());
    assert_eq!(std::fs::read(&path).unwrap(), b"corrupted staging");
    std::fs::write(&path, &bytes).unwrap();
    std::fs::write(&receipt_path, b"future or corrupt ownership record").unwrap();
    assert!(resolver.cache.promote_staging().is_err());
    assert_eq!(std::fs::read(path).unwrap(), bytes);
}

#[test]
fn local_removal_cancels_while_waiting_for_maintenance_without_touching_data() {
    let mut fixture = Fixture::new();
    let resolver = fixture.resolver.clone();
    let _maintenance = resolver.cache.maintenance.lock().unwrap();
    let asset = fixture.asset(&fixture.id);
    let path = fixture.managed().join("blobs").join(&asset.original.id.0);
    let bytes = std::fs::read(&path).unwrap();
    let id = fixture.id.clone();
    fixture.mark(&id, true);
    let mut calls = 0;
    let result = crate::cloud_upload::observed_removal(&mut fixture.session, &json!({"ids":[1]}), &mut |_, _, _| {
        calls += 1;
        false
    });
    assert!(result.is_err());
    assert!(calls > 0);
    assert_eq!(std::fs::read(path).unwrap(), bytes);
    assert!(fixture.session.catalog.photo(lightcraft_catalog::PhotoId(1)).is_some());
}

#[test]
fn zero_budget_evicts_only_uploaded_blobs_and_restart_keeps_pending_working_data() {
    let mut fixture = Fixture::new();
    let resolver = fixture.resolver.clone();
    let maintenance = resolver.cache.maintenance.lock().unwrap();
    let id = fixture.id.clone();
    let asset = fixture.asset(&id);
    let working = fixture.managed().join("working/pending.tif");
    std::fs::create_dir_all(working.parent().unwrap()).unwrap();
    std::fs::write(&working, b"pending editor revision").unwrap();
    fixture.mark(&id, true);
    resolver.cache.promote_staging().unwrap();
    resolver.cache.set_budget(0).unwrap();
    drop(maintenance);
    resolver.cache.trim().unwrap();
    assert!(!fixture.managed().join("blobs").join(&asset.original.id.0).exists());
    assert!(!resolver.root.join(&asset.original.id.0).exists());
    fixture.session.close_library().unwrap();
    let old = std::mem::replace(&mut fixture.session, Session::new().with_fs());
    drop(old);
    fixture.session.open_library(fixture.root.join("library"), false).unwrap();
    assert_eq!(fixture.session.catalog.upload_intents()[&id].state, UploadState::Synchronized);
    assert_eq!(fixture.session.media.asset_resolver.as_ref().unwrap().cache_status().budget_bytes, 0);
    assert_eq!(std::fs::read(working).unwrap(), b"pending editor revision");
    assert!(fixture.managed().join("imports").join(format!("{id}.json")).is_file());
}
