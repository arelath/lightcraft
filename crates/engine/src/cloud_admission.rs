//! Process-wide admission shared by S3 metadata/media and native B2 control.
//! Only a salted-in-process credential digest is retained; no credentials or tokens
//! are logged or persisted. Existing in-flight requests may finish after refusal.
use crate::assets::{AssetError, Result};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

const MAX_SCOPES: usize = 128;
const MAX_DELAY_MS: u64 = 15 * 60 * 1000;
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Scope([u8; 32]);
#[derive(Default)]
struct State {
    cap: Option<u16>,
    cooldown: Option<(Instant, u16)>,
}
#[derive(Default)]
struct Registry {
    scopes: BTreeMap<Scope, State>,
    overflow: State,
}
static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();
fn registry() -> &'static Mutex<Registry> {
    REGISTRY.get_or_init(Default::default)
}
fn prune(registry: &mut Registry) {
    let now = Instant::now();
    registry.scopes.retain(|_, state| {
        state.cooldown = state.cooldown.filter(|(until, _)| *until > now);
        state.cap.is_some() || state.cooldown.is_some()
    });
    registry.overflow.cooldown = registry.overflow.cooldown.filter(|(until, _)| *until > now);
}
impl Scope {
    pub(crate) fn new(service: &str, access: &str, secret: &str) -> Self {
        let mut hash = Sha256::new();
        // Salt is never serialized. Length framing avoids ambiguous tuples.
        static SALT: OnceLock<[u8; 32]> = OnceLock::new();
        hash.update(SALT.get_or_init(|| {
            let mut salt = [0; 32];
            let _ = getrandom::fill(&mut salt);
            salt
        }));
        for part in [service, access, secret] {
            hash.update((part.len() as u64).to_le_bytes());
            hash.update(part.as_bytes());
        }
        Self(hash.finalize().into())
    }
    pub(crate) fn check(self, cancel: &AtomicBool) -> Result<()> {
        if cancel.load(Ordering::Relaxed) {
            return Err(AssetError::Cancelled);
        }
        let mut registry = registry().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        prune(&mut registry);
        for state in [Some(&registry.overflow), registry.scopes.get(&self)].into_iter().flatten() {
            if let Some(status) = state.cap {
                return Err(AssetError::ProviderLimit { status });
            }
            if let Some((until, status)) = state.cooldown {
                let retry_after_ms = u64::try_from(until.saturating_duration_since(Instant::now()).as_millis()).unwrap_or(MAX_DELAY_MS).max(1);
                return Err(AssetError::Throttled { status, retry_after_ms });
            }
        }
        Ok(())
    }
    fn change(self, apply: impl FnOnce(&mut State)) {
        let mut registry = registry().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        prune(&mut registry);
        if registry.scopes.contains_key(&self) || registry.scopes.len() < MAX_SCOPES {
            apply(registry.scopes.entry(self).or_default());
        } else {
            apply(&mut registry.overflow);
        } // Fail closed; never evict an active cap/cooldown.
    }
    pub(crate) fn cap(self, status: u16) {
        self.change(|state| state.cap = Some(status));
    }
    pub(crate) fn throttle(self, status: u16, header: Option<&str>) -> AssetError {
        let delay_ms = header.and_then(|value| value.trim().parse::<u64>().ok()).unwrap_or(2).saturating_mul(1000).clamp(1000, MAX_DELAY_MS);
        self.change(|state| {
            let until = Instant::now() + Duration::from_millis(delay_ms);
            state.cooldown = Some((state.cooldown.map_or(until, |(old, _)| old.max(until)), status));
        });
        AssetError::Throttled { status, retry_after_ms: delay_ms }
    }
    /// Explicit user retry clears the cap only, never a provider Retry-After.
    pub(crate) fn retry(self) {
        let mut registry = registry().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(state) = registry.scopes.get_mut(&self) {
            state.cap = None;
        }
        // Overflow deliberately cannot be reset for an unrelated scope. A restart
        // is the conservative recovery after 128 independently suspended keys.
        prune(&mut registry);
    }
}

#[cfg(test)]
mod tests;
