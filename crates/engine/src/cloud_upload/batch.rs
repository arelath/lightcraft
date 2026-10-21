//! A bounded media batch feeds the single per-library metadata publisher.
//! Media readiness is durable before it leaves a worker. No publication lock is
//! held while transferring media, writing receipts, or waiting for phase ACKs.
use super::*;
use std::sync::atomic::AtomicUsize;
pub(super) const WORKERS: usize = 4;
pub(super) const IMPORTS: usize = 32;

pub(super) struct Prepared<'a> {
    pub ready: Vec<(&'a Item, stages::VerifiedUpload)>,
    pub failures: Vec<String>,
}
pub(super) fn prepare<'a>(
    items: &'a [Item],
    events: &mpsc::SyncSender<Event>,
    cancel: &AtomicBool,
    progress: &UploadProgress,
    revision: u64,
    fresh: bool,
) -> Prepared<'a> {
    let next = AtomicUsize::new(0);
    let stopped = AtomicBool::new(false);
    let attribution = crate::cloud_traffic::Attribution::capture();
    let mut prepared = Prepared { ready: Vec::new(), failures: Vec::new() };
    let mut visited = BTreeSet::new();
    std::thread::scope(|scope| {
        let (sender, receiver) = mpsc::sync_channel(WORKERS);
        let mut workers = Vec::new();
        for slot in 0..WORKERS.min(items.len()) {
            let progress = progress.for_worker(slot);
            let sender = sender.clone();
            let next = &next;
            let stopped = &stopped;
            let attribution = &attribution;
            let spawned = std::thread::Builder::new().name("cloud-media".into()).spawn_scoped(scope, move || {
                let _traffic = attribution.enter();
                let Some(first) = items.first() else { return };
                let mut media = crate::cloud_replication::VerifiedMedia::coordinated(
                    first.coordinator.media.clone(),
                    revision,
                    Some(progress.bytes.clone()),
                    fresh,
                );
                loop {
                    if cancel.load(Ordering::Relaxed) || stopped.load(Ordering::Acquire) {
                        break;
                    }
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    let Some(item) = items.get(index) else { break };
                    let _traffic = crate::cloud_traffic::Scope::operation(
                        crate::cloud_traffic::Purpose::Upload,
                        item.sync.as_deref().map(|state| state.library.as_str()),
                        Some(&item.id),
                        Some(item.attempt),
                    );
                    if let Err(reason) = item.connection.check_admission(cancel) {
                        item.item_result(events, &Ok(Value::Null));
                        if matches!(reason, AssetError::ProviderLimit { .. } | AssetError::Throttled { .. }) {
                            stopped.store(true, Ordering::Release);
                            let _ = events.send(Event::Suspended(crate::cloud_status::CloudFailure::from(reason.clone())));
                        }
                        let _ = sender.send((index, Err(error(reason.to_string()))));
                        break;
                    }
                    let result = crate::guard::catch("cloud media worker", || item.ensure_media(events, cancel, &progress, &mut media))
                        .unwrap_or_else(|message| Err(error(message)));
                    match &result {
                        Ok(_) => {
                            media.ready(&item.connection, blobs(&item.imported.asset).cloned());
                            let _ = events.send(Event::MediaReady { attempt: item.attempt, id: item.id.clone() });
                        }
                        Err(reason) => {
                            item.item_result(events, &Err(error(reason.to_string())));
                            if let Err(reason @ (AssetError::ProviderLimit { .. } | AssetError::Throttled { .. })) =
                                item.connection.check_admission(cancel)
                            {
                                stopped.store(true, Ordering::Release);
                                let _ = events.send(Event::Suspended(crate::cloud_status::CloudFailure::from(reason)));
                            }
                        }
                    }
                    if sender.send((index, result)).is_err() {
                        break;
                    }
                }
            });
            match spawned {
                Ok(worker) => workers.push(worker),
                Err(reason) => {
                    stopped.store(true, Ordering::Release);
                    prepared.failures.push(format!("could not start media worker: {reason}; queued originals retained"));
                    break;
                }
            }
        }
        drop(sender);
        for (index, result) in receiver {
            visited.insert(index);
            if let Some(item) = items.get(index) {
                match result {
                    Ok(verified) => prepared.ready.push((item, verified)),
                    Err(reason) => prepared.failures.push(reason.to_string()),
                }
            }
        }
        for worker in workers {
            if worker.join().is_err() {
                prepared.failures.push("media worker stopped unexpectedly; staging retained".into());
            }
        }
    });
    for (index, item) in items.iter().enumerate() {
        if !visited.contains(&index) {
            item.item_result(events, &Ok(Value::Null));
        }
    }
    // Stable reporting order is independent of transfer completion and causal publication order.
    prepared.ready.sort_by(|(left, _), (right, _)| left.id.cmp(&right.id));
    prepared
}

pub(super) fn abort(items: &[Item], events: &mpsc::SyncSender<Event>, cancel: &AtomicBool) -> Result<(Vec<Value>, Vec<String>)> {
    let mut results = Vec::new();
    let mut failures = Vec::new();
    for (index, item) in items.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            return Err(error("upload cleanup cancelled; receipts retained"));
        }
        if let Err(reason) = item.connection.check_admission(cancel) {
            if matches!(reason, AssetError::ProviderLimit { .. } | AssetError::Throttled { .. }) {
                let _ = events.send(Event::Suspended(crate::cloud_status::CloudFailure::from(reason.clone())));
            }
            for untouched in items.iter().skip(index) {
                untouched.item_result(events, &Ok(Value::Null));
            }
            failures.push(reason.to_string());
            break;
        }
        let result = item.abort(events, cancel);
        item.item_result(events, &result);
        match result {
            Ok(value) => results.push(value),
            Err(reason) => failures.push(reason.to_string()),
        }
    }
    Ok((results, failures))
}
