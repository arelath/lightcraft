use super::*;
use std::{fs, sync::atomic::AtomicU64};

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!("lightcraft-metadata-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn connection() -> S3Connection {
    serde_json::from_value(
        serde_json::json!({"id":"cloud","bucket":"photos","region":"us-east-1","prefix":"library","endpoint":"http://localhost:9000"}),
    )
    .unwrap()
}
fn files(cache: &Cache) -> Vec<PathBuf> {
    fs::read_dir(&cache.root).unwrap().map(|e| e.unwrap().path()).filter(|p| p.extension().is_some_and(|s| s == "body")).collect()
}
#[test]
fn identity_is_full_destination_and_exact_content_key_but_not_credentials_or_cache_root() {
    let c = connection();
    let key = "library/sync-v2/lib/ops/author/00000000000000000001-abc.json";
    let first = identity(&c, key).unwrap();
    let rotated = c.clone().with_keys(crate::assets::s3::AccessKeys::new("another-key", "another-secret").unwrap());
    assert_eq!(identity(&rotated, key).unwrap(), first);
    let mut profile = c.clone();
    profile.profile = Some("other-profile".into());
    assert_eq!(identity(&profile, key).unwrap(), first);
    let mut canonical = c.clone();
    canonical.prefix.push('/');
    canonical.endpoint.as_mut().unwrap().push('/');
    assert_eq!(identity(&canonical, key).unwrap(), first);
    for field in ["id", "bucket", "region", "endpoint"] {
        let mut v = serde_json::to_value(&c).unwrap();
        v[field] = serde_json::json!(match field {
            "id" => "other",
            "bucket" => "other-bucket",
            "region" => "us-west-1",
            _ => "http://localhost:9001",
        });
        let changed = serde_json::from_value(v).unwrap();
        assert_ne!(identity(&changed, key).unwrap(), first);
    }
    let mut moved = c.clone();
    moved.prefix = "other".into();
    assert!(identity(&moved, key).is_err());
    assert_ne!(identity(&moved, &key.replacen("library/", "other/", 1)).unwrap(), first);
    assert_ne!(identity(&c, &key.replace("abc.json", "def.json")).unwrap(), first);
    assert!(identity(&c, "library/catalog/head.json").is_err());
    assert!(identity(&c, "library/sync-v2/lib/control/anything.json").is_err());
    assert!(identity(&c, "library/sync-v2/lib/ops/../bad.json").is_err());
    let scratch = Scratch::new();
    let bound = c.clone().with_metadata_cache(Some(Arc::new(Cache::new(scratch.0.join("cache")))));
    assert!(c.same_destination(&bound));
    assert_eq!(serde_json::to_value(&c).unwrap(), serde_json::to_value(&bound).unwrap(), "runtime cache binding is never persisted");
}
#[test]
fn restart_rechecks_binding_length_and_hash_and_never_reuses_partial_files() {
    let scratch = Scratch::new();
    let root = scratch.0.join("cache");
    let cancel = AtomicBool::new(false);
    let id = [17; 32];
    let other = [18; 32];
    let body = vec![9; 70_000];
    let cache = Cache::new(root.clone());
    cache.save(&id, &body, &cancel).unwrap();
    assert_eq!(files(&cache).len(), 1);
    let entry = files(&cache).pop().unwrap();
    let committed = fs::read(&entry).unwrap();
    drop(cache);
    let cache = Cache::new(root.clone());
    assert_eq!(cache.load(&id, body.len(), &cancel).unwrap(), Some(body.clone()));
    assert_eq!(cache.load(&other, body.len(), &cancel).unwrap(), None);
    let foreign = root.join(other.iter().map(|b| format!("{b:02x}")).collect::<String>() + ".body");
    fs::write(&foreign, &committed).unwrap();
    let cache = Cache::new(root.clone());
    assert_eq!(cache.load(&other, body.len(), &cancel).unwrap(), None, "copied body has the wrong destination/key binding");
    assert!(!foreign.exists());
    for damaged in [
        committed[..40].to_vec(),
        vec![0; committed.len()],
        {
            let mut b = committed.clone();
            *b.last_mut().unwrap() ^= 1;
            b
        },
        {
            let mut b = committed.clone();
            b.push(0);
            b
        },
    ] {
        fs::write(&entry, damaged).unwrap();
        let cache = Cache::new(root.clone());
        assert_eq!(cache.load(&id, body.len(), &cancel).unwrap(), None);
        assert!(!entry.exists());
    }
    fs::write(root.join(".lightcraft-crashed.tmp"), b"uncommitted").unwrap();
    let cache = Cache::new(root);
    assert_eq!(cache.load(&id, body.len(), &cancel).unwrap(), None);
    assert!(cache.root.join(".lightcraft-crashed.tmp").exists());
}
#[test]
fn bounded_admission_evicts_old_bodies_and_failed_atomic_refresh_preserves_the_previous_entry() {
    let scratch = Scratch::new();
    let cancel = AtomicBool::new(false);
    let cache = Cache { max_entries: 2, max_bytes: 600, ..Cache::new(scratch.0.join("cache")) };
    let body = vec![5; 100];
    cache.save(&[1; 32], &body, &cancel).unwrap();
    cache.save(&[2; 32], &body, &cancel).unwrap();
    assert!(cache.load(&[1; 32], 100, &cancel).unwrap().is_some());
    cache.save(&[3; 32], &body, &cancel).unwrap();
    assert!(cache.load(&[2; 32], 100, &cancel).unwrap().is_none());
    assert!(cache.load(&[1; 32], 100, &cancel).unwrap().is_some());
    assert_eq!(files(&cache).len(), 2);
    {
        let _fault = lightcraft_catalog::safe_file::fail_writes_after(48);
        assert!(cache.save(&[1; 32], b"replacement", &cancel).is_err());
    }
    assert_eq!(cache.load(&[1; 32], 100, &cancel).unwrap(), Some(body));
    assert_eq!(fs::read_dir(&cache.root).unwrap().count(), 3, "two bodies and the cache lock; failed temporary file removed");
    cache.save(&[4; 32], &vec![0; 601], &cancel).unwrap();
    assert_eq!(files(&cache).len(), 2, "single over-budget body is not admitted");
    let occupied = fs::read_dir(&cache.root).unwrap().map(|f| f.unwrap().metadata().unwrap().len()).sum::<u64>();
    assert!(occupied <= 600);
}
#[test]
fn cancellation_and_unusable_or_overfull_namespaces_do_not_publish_cache_entries() {
    let scratch = Scratch::new();
    let cancel = AtomicBool::new(true);
    let cache = Cache::new(scratch.0.join("cache"));
    assert!(matches!(cache.load(&[1; 32], 100, &cancel), Err(AssetError::Cancelled)));
    assert!(matches!(cache.save(&[1; 32], b"a", &cancel), Err(AssetError::Cancelled)));
    assert!(!cache.root.exists());
    cancel.store(false, Ordering::Relaxed);
    fs::write(&cache.root, b"keep").unwrap();
    assert!(cache.save(&[1; 32], b"a", &cancel).is_err());
    assert_eq!(fs::read(&cache.root).unwrap(), b"keep");
    let cache = Cache { max_entries: 1, max_bytes: 100, ..Cache::new(scratch.0.join("full")) };
    fs::create_dir_all(&cache.root).unwrap();
    fs::write(cache.root.join("uncommitted.tmp"), vec![1; 100]).unwrap();
    assert!(cache.save(&[1; 32], b"a", &cancel).is_err());
    assert!(files(&cache).is_empty());
    assert!(cache.root.join("uncommitted.tmp").exists());
}
#[test]
fn cancellation_interrupts_waiting_for_another_cache_user() {
    let scratch = Scratch::new();
    let cache = Arc::new(Cache::new(scratch.0.join("cache")));
    let cancel = Arc::new(AtomicBool::new(false));
    let held = cache.state.lock().unwrap();
    std::thread::scope(|scope| {
        let (send, receive) = std::sync::mpsc::channel();
        let cache = cache.clone();
        let worker_cancel = cancel.clone();
        scope.spawn(move || {
            send.send(cache.load(&[1; 32], 100, &worker_cancel).err()).unwrap();
        });
        cancel.store(true, Ordering::Relaxed);
        assert!(matches!(receive.recv_timeout(std::time::Duration::from_secs(2)).unwrap(), Some(AssetError::Cancelled)));
    });
    drop(held);
}

#[test]
fn independent_cache_instances_rescan_after_each_others_writes_and_share_the_disk_budget() {
    let scratch = Scratch::new();
    let root = scratch.0.join("cache");
    let cancel = AtomicBool::new(false);
    let first = Cache { max_entries: 2, max_bytes: 600, ..Cache::new(root.clone()) };
    let second = Cache { max_entries: 2, max_bytes: 600, ..Cache::new(root) };
    first.save(&[1; 32], &[7; 100], &cancel).unwrap();
    assert!(second.load(&[1; 32], 100, &cancel).unwrap().is_some());
    first.save(&[2; 32], &[8; 100], &cancel).unwrap();
    second.save(&[3; 32], &[9; 100], &cancel).unwrap();
    assert_eq!(files(&first).len(), 2, "stale second index must account for the first writer's new body");
    assert!(first.load(&[3; 32], 100, &cancel).unwrap().is_some(), "the first index discovers the second writer's entry");
    assert!(files(&first).iter().map(|p| p.metadata().unwrap().len()).sum::<u64>() <= 600);
    // A writer can crash after invalidating the generation but before publishing.
    // Its uncommitted temp consumes capacity after the next reader rescans.
    let mut gate = std::fs::OpenOptions::new().read(true).write(true).open(first.root.join(".cache-lock")).unwrap();
    use std::io::Write;
    let generation = u64::MAX - 1;
    gate.write_all(&generation.to_le_bytes()).unwrap();
    std::fs::write(first.root.join(".lightcraft-interrupted.tmp"), [0; 300]).unwrap();
    assert!(first.load(&[3; 32], 100, &cancel).unwrap().is_some());
    assert!(first.save(&[4; 32], &[1; 100], &cancel).is_err(), "generation exhaustion safely disables further publication");
    assert!(files(&first).len() <= 1);
    assert!(first.root.join(".lightcraft-interrupted.tmp").exists());
}

#[test]
fn cancellation_interrupts_a_file_lock_held_by_an_independent_instance() {
    let scratch = Scratch::new();
    let root = scratch.0.join("cache");
    fs::create_dir_all(&root).unwrap();
    let gate = std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(false).open(root.join(".cache-lock")).unwrap();
    gate.try_lock().unwrap();
    let cache = Cache::new(root);
    let cancel = AtomicBool::new(false);
    std::thread::scope(|scope| {
        let (send, receive) = std::sync::mpsc::channel();
        let cache = &cache;
        let cancel = &cancel;
        scope.spawn(move || {
            send.send(cache.load(&[1; 32], 100, cancel).err()).unwrap();
        });
        cancel.store(true, Ordering::Relaxed);
        assert!(matches!(receive.recv_timeout(std::time::Duration::from_secs(2)).unwrap(), Some(AssetError::Cancelled)));
    });
    drop(gate);
}

#[test]
fn restarted_history_reader_reuses_verified_bytes_but_a_fresh_read_detects_corruption_and_missing_objects() {
    use lightcraft_catalog::replication::{Change, SyncState};
    use std::io::{Read, Write};
    let scratch = Scratch::new();
    let cancel = AtomicBool::new(false);
    let mut state = SyncState::new("lib".into(), "author".into(), "cloud".into()).unwrap();
    let mutation = state
        .enqueue(vec![Change { entity: "library:lib".into(), field: "label.Red".into(), base: None, value: serde_json::json!("red") }])
        .unwrap()
        .unwrap();
    let bytes = crate::cloud_append::encode(&mutation).unwrap();
    let key = crate::cloud_append::mutation_key("library", &mutation).unwrap();
    let body = Arc::new(Mutex::new(Some(bytes.clone())));
    let calls = Arc::new(AtomicU64::new(0));
    let stop = Arc::new(AtomicBool::new(false));
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let worker_body = body.clone();
    let worker_calls = calls.clone();
    let worker_stop = stop.clone();
    let worker = std::thread::spawn(move || {
        while !worker_stop.load(Ordering::Relaxed) {
            let (mut stream, _) = match listener.accept() {
                Ok(v) => v,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(std::time::Duration::from_millis(2));
                    continue;
                }
                Err(e) => panic!("{e}"),
            };
            stream.set_read_timeout(Some(std::time::Duration::from_secs(3))).unwrap();
            let mut header = Vec::new();
            while !header.ends_with(b"\r\n\r\n") {
                assert!(header.len() < 32 << 10);
                let mut b = [0];
                stream.read_exact(&mut b).unwrap();
                header.push(b[0]);
            }
            assert!(header.starts_with(b"GET "));
            worker_calls.fetch_add(1, Ordering::Relaxed);
            let current = worker_body.lock().unwrap().clone();
            let code = if current.is_some() { 200 } else { 404 };
            let current = current.unwrap_or_default();
            write!(stream, "HTTP/1.1 {code} Fixture\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", current.len()).unwrap();
            stream.write_all(&current).unwrap();
        }
    });
    let mut c = connection();
    c.endpoint = Some(endpoint);
    let c = c.with_keys(crate::assets::s3::AccessKeys::new("fixture-access", "fixture-secret").unwrap());
    let decode = |bytes: &[u8]| crate::cloud_append::decode_mutation("library", "lib", &key, bytes);
    let root = scratch.0.join("cache");
    let first = c.clone().with_metadata_cache(Some(Arc::new(Cache::new(root.clone()))));
    assert_eq!(first.history_record(&key, 1 << 20, &cancel, decode).unwrap().value, mutation);
    assert_eq!(calls.load(Ordering::Relaxed), 1);
    drop(first);
    let reopened = c.clone().with_metadata_cache(Some(Arc::new(Cache::new(root.clone()))));
    assert_eq!(reopened.history_record(&key, 1 << 20, &cancel, decode).unwrap().value, mutation);
    assert_eq!(calls.load(Ordering::Relaxed), 1, "restart has no network body read");
    let entry = files(&Cache::new(root.clone())).pop().unwrap();
    fs::write(&entry, b"partial cache publication").unwrap();
    assert_eq!(reopened.history_record(&key, 1 << 20, &cancel, decode).unwrap().value, mutation);
    assert_eq!(calls.load(Ordering::Relaxed), 2, "damaged cache is refetched, validated and repaired");
    *body.lock().unwrap() = Some(b"remote corrupt bytes".to_vec());
    assert!(reopened.fresh_metadata().history_record(&key, 1 << 20, &cancel, decode).is_err());
    assert_eq!(calls.load(Ordering::Relaxed), 3);
    assert_eq!(reopened.history_record(&key, 1 << 20, &cancel, decode).unwrap().value, mutation, "failed verification never poisons cached proof");
    *body.lock().unwrap() = None;
    assert!(matches!(reopened.fresh_metadata().history_record(&key, 1 << 20, &cancel, decode), Err(AssetError::Missing)));
    assert_eq!(calls.load(Ordering::Relaxed), 4);
    *body.lock().unwrap() = Some(bytes);
    let refused = c.clone().with_metadata_cache(Some(Arc::new(Cache::new(scratch.0.join("rejected")))));
    assert!(refused.history_record::<()>(&key, 1 << 20, &cancel, |_| Err(AssetError::Integrity)).is_err());
    assert!(files(&Cache::new(scratch.0.join("rejected"))).is_empty(), "caller schema validation precedes cache admission");
    let blocked = scratch.0.join("cache-is-a-file");
    fs::write(&blocked, b"preserve").unwrap();
    let uncached = c.with_metadata_cache(Some(Arc::new(Cache::new(blocked.clone()))));
    assert_eq!(
        uncached.history_record(&key, 1 << 20, &cancel, decode).unwrap().value,
        mutation,
        "unwritable cache does not block verified network reads"
    );
    assert_eq!(fs::read(blocked).unwrap(), b"preserve");
    stop.store(true, Ordering::Relaxed);
    worker.join().unwrap();
}
