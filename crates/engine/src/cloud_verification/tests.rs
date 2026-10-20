use super::*;
#[test]
fn reservations_and_cursor_survive_restart_and_clock_changes_without_refunding_failed_io() {
    let now = DAY_MS * 10;
    let mut schedule = Schedule::new("destination".into(), now);
    assert!(!schedule.reserve(now, false).unwrap());
    assert!(schedule.reserve(now, true).unwrap());
    schedule.cursor.after = Some(format!("ops/author/{:020}-{}.json", 1, "a".repeat(64)));
    let mut loaded = Schedule::decode(&schedule.encode().unwrap(), "destination").unwrap();
    assert_eq!(loaded.cursor.after, schedule.cursor.after);
    assert!(!loaded.reserve(now + 1, false).unwrap());
    assert!(!loaded.reserve(now - DAY_MS, true).unwrap());
    for _ in 1..SLICES_PER_DAY {
        assert!(loaded.reserve(now + 1, true).unwrap());
    }
    assert!(!loaded.reserve(now + 2, true).unwrap());
    assert!(loaded.reserve(now + DAY_MS, false).unwrap());
    let mut done = loaded.cursor.clone();
    done.complete = true;
    loaded.failed = true;
    loaded.completed_slice(done, now + DAY_MS);
    assert!(!loaded.failed);
    assert!(!loaded.reserve(now + DAY_MS + 1, false).unwrap());
    assert!(loaded.reserve(now + DAY_MS + WEEK_MS, false).unwrap());
    assert!(Schedule::decode(&schedule.encode().unwrap(), "another").is_err());
    assert!(Schedule::decode(&vec![0; MAX_RECORD_BYTES + 1], "destination").is_err());
}
#[test]
fn one_slice_counts_nested_reads_and_reserves_worst_case_bytes_before_io() {
    let budget = Budget::default();
    budget.admit(64 << 20).unwrap();
    budget.admit(64 << 20).unwrap();
    assert_eq!(budget.admit(1), Err(AssetError::HistoryBudget));
    budget.read(64 << 20, 100);
    for _ in 2..REQUESTS {
        budget.admit(1).unwrap();
    }
    assert_eq!(budget.admit(1), Err(AssetError::HistoryBudget));
}
#[test]
fn twenty_four_idle_hours_do_not_turn_polling_into_fresh_verification() {
    let start = DAY_MS * 10;
    let mut state = Schedule::new("destination".into(), start);
    assert!(state.discover(start, false));
    let mut restarted = Schedule::decode(&state.encode().unwrap(), "destination").unwrap();
    assert!(!restarted.discover(start + 60_000, false));
    assert!(restarted.discover(start + DISCOVERY_MS, false));
    assert!(restarted.discover(start + DISCOVERY_MS + 1, true), "manual checks can discover old identities immediately");
    for tick in 0..96 {
        let now = start + tick * 15 * 60 * 1000;
        state = Schedule::decode(&state.encode().unwrap(), "destination").unwrap();
        assert!(!state.reserve(now, false).unwrap(), "idle tick {tick} reset a fresh verification deadline");
    }
    assert!(state.reserve(start + WEEK_MS, false).unwrap());
    let mut cursor = state.cursor.clone();
    cursor.complete = true;
    state.completed_slice(cursor, start + WEEK_MS);
    for tick in 1..96 {
        assert!(!state.reserve(start + WEEK_MS + tick * 15 * 60 * 1000, false).unwrap());
    }
}
#[test]
fn missing_required_keys_cannot_hide_in_listing_gaps_or_after_the_last_page() {
    let expected = std::collections::BTreeSet::from(["ops/a/first".into(), "ops/a/second".into(), "receipts/root.json".into()]);
    assert_eq!(listing_gap(&expected, "ops", None, Some("ops/a/second")), Err(AssetError::Missing));
    assert!(listing_gap(&expected, "ops", Some("ops/a/first"), Some("ops/a/second")).is_ok());
    assert_eq!(listing_gap(&expected, "ops", Some("ops/a/first"), None), Err(AssetError::Missing));
    assert!(listing_gap(&expected, "ops", Some("ops/a/second"), None).is_ok());
    assert_eq!(listing_gap(&expected, "receipts", Some("ops/a/second"), None), Err(AssetError::Missing));
}
#[cfg(not(target_arch = "wasm32"))]
#[test]
fn disk_reservation_precedes_io_and_cancelled_slices_stay_spent_across_reopen() {
    use std::{
        fs,
        sync::atomic::{AtomicBool, AtomicU64, Ordering},
    };
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!("lightcraft-verify-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
    fs::create_dir_all(&dir).unwrap();
    let c = serde_json::from_value(
        serde_json::json!({"id":"cloud","bucket":"photos","region":"us-east-1","prefix":"library","endpoint":"http://localhost:9000"}),
    )
    .unwrap();
    let mut local = lightcraft_catalog::replication::SyncState::new("library".into(), "device".into(), "cloud".into()).unwrap();
    local.protocol = lightcraft_catalog::replication::SyncProtocol::AppendLog;
    let now = DAY_MS * 10;
    {
        let run = native::Run::open(&dir, &c, &local, now, false, true).unwrap();
        assert!(!run.selected());
    }
    let path = fs::read_dir(dir.join("remote-verification-v1"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.extension().is_some_and(|e| e == "json"))
        .unwrap();
    let before = fs::read(&path).unwrap();
    {
        let _fault = lightcraft_catalog::safe_file::fail_writes_after(1);
        assert!(native::Run::open(&dir, &c, &local, now, true, false).is_err());
    }
    assert_eq!(fs::read(&path).unwrap(), before);
    for _ in 0..SLICES_PER_DAY {
        let mut run = native::Run::open(&dir, &c, &local, now, true, false).unwrap();
        assert_eq!(run.verify(&c, &local, &AtomicBool::new(true)), Err(AssetError::Cancelled));
        assert!(native::Run::open(&dir, &c, &local, now, true, false).is_err(), "independent writers share an OS lock");
    }
    {
        let mut run = native::Run::open(&dir, &c, &local, now, true, false).unwrap();
        assert!(!run.selected());
        assert_eq!(run.verify(&c, &local, &AtomicBool::new(false)).unwrap()["deferred"], true);
    }
    fs::remove_dir_all(dir).unwrap();
}
