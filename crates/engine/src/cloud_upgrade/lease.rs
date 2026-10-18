//! Keep cancellation responsive even while a renewal request itself is blocked.
use crate::cloud_status::{CloudFailure, FailureKind};
use std::sync::{
    Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;

type Result<T> = std::result::Result<T, CloudFailure>;

struct Stop<'a> {
    stop: &'a AtomicBool,
    io_cancel: &'a AtomicBool,
    watcher: std::thread::Thread,
    heartbeat: Option<std::thread::Thread>,
}
impl Drop for Stop<'_> {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.io_cancel.store(true, Ordering::Relaxed);
        self.watcher.unpark();
        if let Some(heartbeat) = &self.heartbeat {
            heartbeat.unpark();
        }
    }
}

pub(super) fn run<T>(
    user_cancel: &AtomicBool,
    tick: impl Fn(&AtomicBool) -> Result<()> + Sync,
    body: impl FnOnce(&AtomicBool) -> Result<T>,
) -> Result<T> {
    let stop = AtomicBool::new(false);
    let work_cancel = AtomicBool::new(user_cancel.load(Ordering::Relaxed));
    let io_cancel = AtomicBool::new(false);
    let failure = Mutex::new(None);
    std::thread::scope(|scope| {
        let watcher = std::thread::Builder::new()
            .name("cloud-upgrade-cancel".into())
            .spawn_scoped(scope, || {
                while !stop.load(Ordering::Acquire) {
                    if user_cancel.load(Ordering::Relaxed) {
                        work_cancel.store(true, Ordering::Relaxed);
                        io_cancel.store(true, Ordering::Relaxed);
                    }
                    std::thread::park_timeout(Duration::from_millis(50));
                }
                io_cancel.store(true, Ordering::Relaxed);
            })
            .map_err(|e| CloudFailure::local(format!("could not monitor upgrade cancellation: {e}")))?;
        let heartbeat = std::thread::Builder::new().name("cloud-upgrade-lease".into()).spawn_scoped(scope, || {
            while !stop.load(Ordering::Acquire) && !work_cancel.load(Ordering::Relaxed) {
                if let Err(error) = tick(&io_cancel) {
                    if !stop.load(Ordering::Acquire) {
                        *failure.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(error);
                        work_cancel.store(true, Ordering::Relaxed);
                    }
                    break;
                }
                std::thread::park_timeout(Duration::from_millis(100));
            }
        });
        // Also stop both monitors if the worker's outer panic boundary unwinds us.
        let shutdown = Stop {
            stop: &stop,
            io_cancel: &io_cancel,
            watcher: watcher.thread().clone(),
            heartbeat: heartbeat.as_ref().ok().map(|h| h.thread().clone()),
        };
        let result = match &heartbeat {
            Ok(_) if !work_cancel.load(Ordering::Relaxed) => body(&work_cancel),
            Ok(_) => Err(CloudFailure::new(FailureKind::Cancelled, "cloud upgrade cancelled; pending work retained")),
            Err(error) => Err(CloudFailure::local(format!("could not renew upgrade ownership: {error}"))),
        };
        drop(shutdown);
        if let Ok(heartbeat) = heartbeat {
            heartbeat.join().map_err(|_| CloudFailure::local("upgrade lease monitor stopped unexpectedly"))?;
        }
        watcher.join().map_err(|_| CloudFailure::local("upgrade cancellation monitor stopped unexpectedly"))?;
        let renewal_error = failure.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take();
        if let Some(error) = renewal_error {
            Err(error)
        } else if user_cancel.load(Ordering::Relaxed) {
            Err(CloudFailure::new(FailureKind::Cancelled, "cloud upgrade cancelled; pending work retained"))
        } else {
            result
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU64;
    #[test]
    fn worker_unwind_does_not_leave_scoped_monitors_waiting_forever() {
        let user = AtomicBool::new(false);
        let result = std::panic::catch_unwind(|| run::<()>(&user, |_| Ok(()), |_| panic!("synthetic worker panic")));
        assert!(result.is_err(), "the caller's outer panic boundary receives the worker failure");
    }
    #[test]
    fn expensive_work_keeps_renewing_with_an_injectable_clock_until_the_supervisor_stops() {
        use crate::cloud_control::{Boundary, Control, LEASE_MS, Plan, lease::Lease};
        let user = AtomicBool::new(false);
        let clock = AtomicU64::new(100);
        let current = Mutex::new(
            Control::initial("library")
                .begin("migration", Plan::Rebase, Boundary { tip: "ab".repeat(32), generation: 1 }, "owner", &"cd".repeat(32), 100)
                .unwrap(),
        );
        let lease = Mutex::new(Lease::new(current.lock().unwrap().clone()).unwrap());
        run(
            &user,
            |_| {
                let now = clock.load(Ordering::Relaxed);
                let mut lease = lease.lock().unwrap();
                if lease.due(now) {
                    let mut control = current.lock().unwrap();
                    *control = lease.renewal(&control, now)?.unwrap();
                    lease.confirmed(&control);
                }
                Ok(())
            },
            |_| {
                for now in [LEASE_MS / 2 + 100, LEASE_MS + 100, LEASE_MS * 3 / 2 + 100, LEASE_MS * 2 + 100] {
                    clock.store(now, Ordering::Relaxed);
                    let deadline = std::time::Instant::now() + Duration::from_secs(5);
                    loop {
                        if current.lock().unwrap().migration.as_ref().unwrap().lease_until_ms == now + LEASE_MS {
                            break;
                        }
                        assert!(std::time::Instant::now() < deadline, "heartbeat renews while the expensive body is paused");
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    assert!(current.lock().unwrap().takeover("other", &"ef".repeat(32), now + 1, false).is_err());
                }
                Ok(())
            },
        )
        .unwrap();
        let control = current.lock().unwrap();
        let expired = control.migration.as_ref().unwrap().lease_until_ms;
        let replacement = control.takeover("other", &"ef".repeat(32), expired, false).unwrap().begin_output(&"ef".repeat(32), expired).unwrap();
        assert!(replacement.activate(&"cd".repeat(32)).is_err());
    }
    #[test]
    fn ownership_loss_cancels_work_without_relabeling_it_as_user_cancellation() {
        let user = AtomicBool::new(false);
        let ticks = AtomicU64::new(0);
        let result: Result<()> = run(
            &user,
            |_| {
                ticks.fetch_add(1, Ordering::Relaxed);
                Err(crate::cloud_control::lease::lost())
            },
            |work| {
                let deadline = std::time::Instant::now() + Duration::from_secs(5);
                while !work.load(Ordering::Relaxed) && std::time::Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(5));
                }
                assert!(work.load(Ordering::Relaxed));
                Err(CloudFailure::new(FailureKind::Cancelled, "interrupted I/O"))
            },
        );
        assert_eq!(result.unwrap_err().kind, FailureKind::Contention);
        assert!(!user.load(Ordering::Relaxed));
        assert_eq!(ticks.load(Ordering::Relaxed), 1);
    }
    #[test]
    fn user_cancel_reaches_body_and_a_blocked_heartbeat() {
        let user = AtomicBool::new(false);
        let started = AtomicBool::new(false);
        let result: Result<()> = run(
            &user,
            |io| {
                started.store(true, Ordering::Release);
                while !io.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(CloudFailure::new(FailureKind::Cancelled, "interrupted renewal"))
            },
            |work| {
                while !started.load(Ordering::Acquire) {
                    std::thread::yield_now();
                }
                user.store(true, Ordering::Relaxed);
                let deadline = std::time::Instant::now() + Duration::from_secs(5);
                while !work.load(Ordering::Relaxed) && std::time::Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(5));
                }
                assert!(work.load(Ordering::Relaxed));
                Err(CloudFailure::new(FailureKind::Cancelled, "interrupted work"))
            },
        );
        assert_eq!(result.unwrap_err().kind, FailureKind::Cancelled);
    }
}
