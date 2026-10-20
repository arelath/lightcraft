use super::*;
use serde_json::json;

fn connection() -> S3Connection {
    serde_json::from_value(json!({"id":"cloud","bucket":"photos","region":"us-east-1","prefix":"library"})).unwrap()
}
fn marker(n: u64) -> (RecordRef, Vec<u8>) {
    let parent =
        RecordRef { key: format!("library/catalog/changes/{n:064x}.json"), hash: lightcraft_catalog::assets::BlobId(format!("{n:064x}")), size: 100 };
    let bytes = serde_json::to_vec(&json!({"version":3,"parent":parent,"imports":[],"checkpointMarker":true})).unwrap();
    let hash = sha256(&bytes);
    (RecordRef { key: format!("library/catalog/changes/{}.json", hash.0), hash, size: bytes.len() as u64 }, bytes)
}
fn owned_paths(index: &Index) -> [PathBuf; 2] {
    [index.refs.path.clone(), index.bodies.path.clone()]
}
fn cleaned(paths: [PathBuf; 2]) {
    assert!(paths.iter().all(|path| !path.exists()), "{paths:?}");
}

#[test]
fn index_keeps_the_oldest_recovery_window_with_constant_disk_and_cleans_up() {
    let mut index = Index::new().unwrap();
    let paths = owned_paths(&index);
    let cancel = AtomicBool::new(false);
    for n in 0..100_000 {
        let (reference, bytes) = marker(n);
        index.push(&reference, &bytes, &cancel).unwrap();
    }
    assert_eq!(index.count, 100_000);
    assert_eq!(index.refs.file().unwrap().metadata().unwrap().len(), MAX_INDEX);
    assert!(index.bodies.file().unwrap().metadata().unwrap().len() <= MAX_BODIES);
    assert_eq!(index.retained(), RECOVERY_SEGMENT_BATCHES);
    for n in (100_000 - RECOVERY_SEGMENT_BATCHES..100_000).rev() {
        let (reference, batch) = index.pop(&connection(), &cancel).unwrap().unwrap();
        assert_eq!(reference, marker(n).0);
        assert_eq!(batch.parent.unwrap().hash.0, format!("{n:064x}"));
    }
    assert!(index.pop(&connection(), &cancel).unwrap().is_none());
    drop(index);
    cleaned(paths);
}

#[test]
fn byte_ring_wraps_and_retains_only_complete_oldest_records() {
    let mut index = Index::with_capacity(2000).unwrap();
    let paths = owned_paths(&index);
    let cancel = AtomicBool::new(false);
    for n in 0..100 {
        let (reference, bytes) = marker(n);
        index.push(&reference, &bytes, &cancel).unwrap();
    }
    assert_eq!(index.bodies.file().unwrap().metadata().unwrap().len(), 2000);
    let retained = index.retained();
    assert!(retained > 1 && retained < 100);
    for n in (100 - retained..100).rev() {
        assert_eq!(index.pop(&connection(), &cancel).unwrap().unwrap().0, marker(n).0);
    }
    assert!(index.pop(&connection(), &cancel).unwrap().is_none());
    drop(index);
    cleaned(paths);
}

#[test]
fn cycle_detector_is_bounded_and_rejects_self_and_longer_cycles() {
    for period in [1, 2, 17, 4097] {
        let mut detector = Cycle::default();
        let mut rejected = false;
        for n in 0..period * 4 {
            if detector.visit(&lightcraft_catalog::assets::BlobId(format!("{:064x}", n % period))).is_err() {
                rejected = true;
                break;
            }
        }
        assert!(rejected, "cycle length {period}");
    }
    let mut detector = Cycle::default();
    for n in 0..100_000 {
        detector.visit(&lightcraft_catalog::assets::BlobId(format!("{n:064x}"))).unwrap();
    }
}

#[test]
fn invalid_scratch_frames_return_actionable_errors_and_clean_up() {
    let cancel = AtomicBool::new(false);
    for corrupt_length in [false, true] {
        let mut index = Index::new().unwrap();
        let paths = owned_paths(&index);
        let (reference, bytes) = marker(1);
        index.push(&reference, &bytes, &cancel).unwrap();
        let (offset, bytes) =
            if corrupt_length { (8 + MAX_REFERENCE as u64, u32::MAX.to_le_bytes().to_vec()) } else { (0, u64::MAX.to_le_bytes().to_vec()) };
        index.refs.write(offset, &bytes, &cancel).unwrap();
        let error = index.pop(&connection(), &cancel).err().unwrap().to_string();
        assert!(error.contains("scratch") && error.contains("retained"), "{error}");
        drop(index);
        cleaned(paths);
    }
}

#[test]
fn stored_bytes_are_rehashed_and_scope_and_schema_are_rechecked() {
    let cancel = AtomicBool::new(false);
    for mode in 0..4 {
        let mut index = Index::new().unwrap();
        let paths = owned_paths(&index);
        let (mut reference, mut bytes) = marker(1);
        if mode == 1 {
            bytes = br#"{"version":99,"imports":[],"checkpointMarker":true}"#.to_vec();
            reference.hash = sha256(&bytes);
            reference.key = format!("library/catalog/changes/{}.json", reference.hash.0);
            reference.size = bytes.len() as u64;
        }
        index.push(&reference, &bytes, &cancel).unwrap();
        let mut connection = connection();
        match mode {
            0 => index.bodies.write(0, b"!", &cancel).unwrap(),
            2 => {
                index.bodies.file().unwrap().set_len(1).unwrap();
            }
            3 => connection.prefix = "another-library".into(),
            _ => {}
        }
        assert!(index.pop(&connection, &cancel).is_err());
        drop(index);
        cleaned(paths);
    }
}

#[test]
fn cancellation_and_disk_write_failure_never_admit_partial_records_and_clean_up() {
    let (reference, bytes) = marker(1);
    let cancel = AtomicBool::new(true);
    let mut index = Index::new().unwrap();
    let paths = owned_paths(&index);
    assert!(matches!(index.push(&reference, &bytes, &cancel), Err(AssetError::Cancelled)));
    assert_eq!(index.count, 0);
    cancel.store(false, Ordering::Relaxed);
    index.push(&reference, &bytes, &cancel).unwrap();
    cancel.store(true, Ordering::Relaxed);
    assert!(matches!(index.pop(&connection(), &cancel), Err(AssetError::Cancelled)));
    drop(index);
    cleaned(paths);
    let mut index = Index::new().unwrap();
    let paths = owned_paths(&index);
    index.bodies.file.take();
    assert!(matches!(index.push(&reference, &bytes, &AtomicBool::new(false)), Err(AssetError::Disk(_))));
    assert_eq!(index.count, 0);
    drop(index);
    cleaned(paths);
}

#[test]
fn abrupt_exit_fixture() {
    let Some(report) = std::env::var_os("LIGHTCRAFT_REPLAY_CRASH_TEST") else { return };
    let mut index = Index::new().unwrap();
    let (reference, bytes) = marker(1);
    index.push(&reference, &bytes, &AtomicBool::new(false)).unwrap();
    std::fs::write(report, serde_json::to_vec(&owned_paths(&index)).unwrap()).unwrap();
    // Only this explicitly spawned fixture exits; no destructor gets to clean up.
    std::process::exit(91);
}

#[test]
#[cfg(any(unix, windows))]
fn abrupt_process_exit_reclaims_both_scratch_files() {
    let mut nonce = [0; 32];
    getrandom::fill(&mut nonce).unwrap();
    let report = std::env::temp_dir().join(format!("lightcraft-replay-exit-{}.json", sha256(&nonce).0));
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command.args(["--exact", "cloud_replication::replay::tests::abrupt_exit_fixture", "--nocapture"]);
    command.env("LIGHTCRAFT_REPLAY_CRASH_TEST", &report);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    let output = command.output().unwrap();
    assert_eq!(output.status.code(), Some(91), "{} {}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
    let paths = serde_json::from_slice(&std::fs::read(&report).unwrap()).unwrap();
    std::fs::remove_file(report).unwrap();
    cleaned(paths);
}
