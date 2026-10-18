//! Account API cooldowns contain only host/port and a monotonic deadline. A
//! throttled CAS is not retried inside a publication or maintenance loop.
use super::*;
use std::sync::atomic::Ordering;
const MAX_DELAY_MS: u64 = 15 * 60 * 1000;
type Cooldowns = BTreeMap<(String, u16), (Instant, u16)>;
static COOLDOWNS: OnceLock<Mutex<Cooldowns>> = OnceLock::new();
fn remaining(until: Instant) -> u64 {
    u64::try_from(until.saturating_duration_since(Instant::now()).as_millis()).unwrap_or(MAX_DELAY_MS).max(1)
}
pub(super) fn check(url: &Url, cancel: &AtomicBool) -> Result<()> {
    if cancel.load(Ordering::Relaxed) {
        return Err(AssetError::Cancelled);
    }
    let mut cooldowns = COOLDOWNS.get_or_init(|| Mutex::new(BTreeMap::new())).lock().unwrap_or_else(|e| e.into_inner());
    cooldowns.retain(|_, (until, _)| *until > Instant::now());
    let cooldown = cooldowns.get(&(url.host.clone(), url.port)).or_else(|| {
        // Never evict an active cooldown to admit a new host. This also bounds
        // hostile or broken endpoint configurations without permitting a burst.
        (cooldowns.len() >= 32).then(|| cooldowns.values().max_by_key(|(until, _)| *until)).flatten()
    });
    if let Some((until, status)) = cooldown {
        return Err(AssetError::Throttled { status: *status, retry_after_ms: remaining(*until) });
    }
    Ok(())
}
pub(super) fn defer(url: &Url, status: u16, header: Option<&str>) -> AssetError {
    let delay_ms = header.and_then(|s| s.trim().parse::<u64>().ok()).unwrap_or(2).saturating_mul(1000).clamp(1000, MAX_DELAY_MS);
    let mut cooldowns = COOLDOWNS.get_or_init(|| Mutex::new(BTreeMap::new())).lock().unwrap_or_else(|e| e.into_inner());
    cooldowns.retain(|_, (until, _)| *until > Instant::now());
    let key = (url.host.clone(), url.port);
    if cooldowns.len() < 32 || cooldowns.contains_key(&key) {
        let until = Instant::now() + Duration::from_millis(delay_ms);
        cooldowns
            .entry(key)
            .and_modify(|(old, code)| {
                *old = (*old).max(until);
                *code = status;
            })
            .or_insert((until, status));
    }
    AssetError::Throttled { status, retry_after_ms: delay_ms }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn provider_delay_is_bounded_preserved_and_not_an_uncertain_write() {
        for (port, header, expected) in
            [(19061, Some("60"), 60_000), (19062, Some("invalid"), 2000), (19063, Some("18446744073709551615"), MAX_DELAY_MS)]
        {
            let url = Url::parse(&format!("http://127.0.0.1:{port}/b2api/v4/test")).unwrap();
            let error = defer(&url, 429, header);
            assert!(!error.uncertain_write());
            let failure = crate::cloud_status::CloudFailure::from(error);
            assert_eq!(failure.http_status, Some(429));
            assert_eq!(failure.retry_after_ms, Some(expected));
            assert!(matches!(check(&url, &AtomicBool::new(false)), Err(AssetError::Throttled { .. })));
            assert!(matches!(check(&url, &AtomicBool::new(true)), Err(AssetError::Cancelled)));
            let mut retry = crate::cloud_status::RetryBudget::default();
            assert_eq!(retry.failed(&failure, Some(1000)), Some(expected / 1000));
            assert_eq!(retry.view(Some(1000))["secondsUntilRetry"], expected / 1000);
        }
    }
}
