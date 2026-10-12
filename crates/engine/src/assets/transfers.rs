//! A bounded, cancellation-aware admission queue shared by all native acquisitions.
//! Downloading holds a permit; decoding and cached reads do not.

use super::{AssetError, Result};
use lightcraft_catalog::assets::BlobId;
use serde::Serialize;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Priority {
    Background = 0,
    VisibleThumbnail = 1,
    NearbyPhoto = 2,
    ActivePhoto = 3,
}

thread_local! {
    static PRIORITY: std::cell::Cell<Priority> = const { std::cell::Cell::new(Priority::Background) };
    static PROGRESS: std::cell::RefCell<Option<crate::activity::TaskHandle>> = const { std::cell::RefCell::new(None) };
}

pub fn with_progress<T>(task: crate::activity::TaskHandle, run: impl FnOnce() -> T) -> T {
    struct Restore(Option<crate::activity::TaskHandle>);
    impl Drop for Restore {
        fn drop(&mut self) {
            PROGRESS.with(|p| {
                p.replace(self.0.take());
            });
        }
    }
    let _restore = Restore(PROGRESS.with(|p| p.replace(Some(task))));
    run()
}

pub(super) fn progress(blob: &super::Blob, done: u64) {
    PROGRESS.with(|p| {
        if let Some(task) = p.borrow().as_ref() {
            task.progress(done, blob.size);
            task.detail(&blob.remote.key);
        }
    });
}

pub(super) fn cancel_flag() -> Option<Arc<AtomicBool>> {
    PROGRESS.with(|p| p.borrow().as_ref().and_then(crate::activity::TaskHandle::cancel_flag))
}

/// Applies to acquisitions in this call, and restores the caller's priority during unwinding.
pub fn with_priority<T>(priority: Priority, run: impl FnOnce() -> T) -> T {
    struct Restore(Priority);
    impl Drop for Restore {
        fn drop(&mut self) {
            PRIORITY.with(|p| p.set(self.0));
        }
    }
    let _restore = Restore(PRIORITY.with(|p| p.replace(priority)));
    run()
}

pub(super) fn current_priority() -> u32 {
    PRIORITY.with(|p| p.get() as u32)
}

struct Waiting {
    blob: BlobId,
    priority: Arc<AtomicU32>,
}

#[derive(Default)]
struct State {
    next: u64,
    waiting: BTreeMap<u64, Waiting>,
    active: BTreeMap<u64, BlobId>,
}

#[derive(Serialize)]
pub struct QueueStatus {
    pub limit: usize,
    pub active: Vec<BlobId>,
    pub queued: Vec<QueuedTransfer>,
}

#[derive(Serialize)]
pub struct QueuedTransfer {
    pub blob: BlobId,
    pub priority: Priority,
}

pub(super) struct Queue {
    limit: usize,
    state: Mutex<State>,
    changed: Condvar,
}

impl Queue {
    pub(super) fn new(limit: usize) -> Self {
        Self { limit: limit.clamp(1, 16), state: Mutex::new(State::default()), changed: Condvar::new() }
    }

    pub(super) fn status(&self) -> QueueStatus {
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        QueueStatus {
            limit: self.limit,
            active: state.active.values().cloned().collect(),
            queued: state
                .waiting
                .values()
                .map(|w| QueuedTransfer {
                    blob: w.blob.clone(),
                    priority: match w.priority.load(Ordering::Relaxed) {
                        3.. => Priority::ActivePhoto,
                        2 => Priority::NearbyPhoto,
                        1 => Priority::VisibleThumbnail,
                        _ => Priority::Background,
                    },
                })
                .collect(),
        }
    }

    pub(super) fn enter(&self, blob: &BlobId, priority: Arc<AtomicU32>, cancel: &AtomicBool) -> Result<Permit<'_>> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.waiting.len() >= 4096 {
            return Err(AssetError::Invalid("too many queued cloud downloads; cancel background work and retry".into()));
        }
        let id = state.next.checked_add(1).ok_or_else(|| AssetError::Invalid("transfer queue exhausted; reopen the library".into()))?;
        state.next = id;
        state.waiting.insert(id, Waiting { blob: blob.clone(), priority });
        loop {
            if cancel.load(Ordering::Relaxed) {
                state.waiting.remove(&id);
                self.changed.notify_all();
                return Err(AssetError::Cancelled);
            }
            // Higher priority first, FIFO within a priority. A duplicate foreground request
            // promotes the same entry's atomic priority, without creating another download.
            let next = state.waiting.iter().max_by_key(|(id, w)| (w.priority.load(Ordering::Relaxed), std::cmp::Reverse(**id))).map(|(id, _)| *id);
            if state.active.len() < self.limit && next == Some(id) {
                state.waiting.remove(&id);
                state.active.insert(id, blob.clone());
                return Ok(Permit { queue: self, id });
            }
            state = self.changed.wait_timeout(state, Duration::from_millis(20)).unwrap_or_else(PoisonError::into_inner).0;
        }
    }
}

pub(super) struct Permit<'a> {
    queue: &'a Queue,
    id: u64,
}

impl Drop for Permit<'_> {
    fn drop(&mut self) {
        self.queue.state.lock().unwrap_or_else(PoisonError::into_inner).active.remove(&self.id);
        self.queue.changed.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Instant;

    fn queued(queue: &Queue, count: usize) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while queue.status().queued.len() != count {
            assert!(Instant::now() < deadline, "queue did not settle");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    fn bounded_queue_prioritizes_active_nearby_visible_then_background() {
        let queue = Arc::new(Queue::new(1));
        let active = queue.enter(&BlobId("occupied".into()), Arc::new(AtomicU32::new(0)), &AtomicBool::new(false)).unwrap();
        let (tx, rx) = mpsc::channel();
        let mut threads = Vec::new();
        for priority in 0..4 {
            let (queue, tx) = (queue.clone(), tx.clone());
            threads.push(std::thread::spawn(move || {
                let _permit = queue.enter(&BlobId(priority.to_string()), Arc::new(AtomicU32::new(priority)), &AtomicBool::new(false)).unwrap();
                assert_eq!(queue.status().active.len(), 1);
                tx.send(priority).unwrap();
                std::thread::sleep(Duration::from_millis(3));
            }));
        }
        queued(&queue, 4);
        drop(active);
        let order: Vec<_> = (0..4).map(|_| rx.recv_timeout(Duration::from_secs(5)).unwrap()).collect();
        for thread in threads {
            thread.join().unwrap();
        }
        assert_eq!(order, [3, 2, 1, 0]);
        assert!(queue.status().active.is_empty() && queue.status().queued.is_empty());
    }

    #[test]
    fn queued_duplicate_priority_promotion_and_cancellation_do_not_consume_a_slot() {
        let queue = Arc::new(Queue::new(1));
        let held = queue.enter(&BlobId("occupied".into()), Arc::new(AtomicU32::new(0)), &AtomicBool::new(false)).unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let promoted = Arc::new(AtomicU32::new(0));
        let (tx, rx) = mpsc::channel();
        let mut threads = Vec::new();
        for (id, priority, flag) in [
            ("cancel", Arc::new(AtomicU32::new(3)), cancel.clone()),
            ("visible", Arc::new(AtomicU32::new(1)), Arc::new(AtomicBool::new(false))),
            ("promoted", promoted.clone(), Arc::new(AtomicBool::new(false))),
        ] {
            let (queue, tx) = (queue.clone(), tx.clone());
            threads.push(std::thread::spawn(move || match queue.enter(&BlobId(id.into()), priority, &flag) {
                Ok(_permit) => {
                    tx.send(id).unwrap();
                    std::thread::sleep(Duration::from_millis(3));
                }
                Err(error) => {
                    assert_eq!(error, AssetError::Cancelled);
                    tx.send("cancelled").unwrap();
                }
            }));
        }
        queued(&queue, 3);
        promoted.store(3, Ordering::Relaxed);
        cancel.store(true, Ordering::Relaxed);
        assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), "cancelled");
        assert_eq!(queue.status().active.len(), 1);
        drop(held);
        assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), "promoted");
        assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), "visible");
        for thread in threads {
            thread.join().unwrap();
        }
    }
}
