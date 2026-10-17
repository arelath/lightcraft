//! Activity-aware cadence for ordinary remote scans, shared by native and web.
use std::time::Duration;
use web_time::Instant;

pub struct PollCadence {
    last_activity: Instant,
    last_scan: Instant,
}
impl Default for PollCadence {
    fn default() -> Self {
        let now = Instant::now();
        Self { last_activity: now, last_scan: now }
    }
}
impl PollCadence {
    pub fn activity(&mut self, now: Instant) {
        self.last_activity = now;
    }
    pub fn scanned(&mut self, now: Instant) {
        self.last_scan = now;
    }
    pub fn deadline(&self, now: Instant) -> Instant {
        let seconds = if now.saturating_duration_since(self.last_activity) > Duration::from_secs(60) { 900 } else { 60 };
        self.last_scan + Duration::from_secs(seconds)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ordinary_scans_slow_after_inactivity_and_resume_on_input() {
        let now = Instant::now();
        let mut cadence = PollCadence { last_activity: now, last_scan: now };
        assert_eq!(cadence.deadline(now), now + Duration::from_secs(60));
        assert_eq!(cadence.deadline(now + Duration::from_secs(60)), now + Duration::from_secs(60));
        let idle = now + Duration::from_secs(61);
        assert_eq!(cadence.deadline(idle), now + Duration::from_secs(900));
        cadence.activity(idle);
        assert_eq!(cadence.deadline(idle), now + Duration::from_secs(60));
        cadence.scanned(idle);
        assert_eq!(cadence.deadline(idle), idle + Duration::from_secs(60));
        assert_eq!(cadence.deadline(idle + Duration::from_secs(901)), idle + Duration::from_secs(900));
    }
}
