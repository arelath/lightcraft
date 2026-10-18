//! Transport-independent renewal policy. The lease authorizes only the exact acquired
//! migration; a newer revision is not evidence that a different owner is ours.
use super::{Control, State};
use crate::cloud_status::{CloudFailure, FailureKind};

#[derive(Clone, Debug)]
pub struct Lease {
    acquired: Control,
    until: u64,
    terminal: Option<Control>,
    revision: u64,
    finished: bool,
}
impl Lease {
    pub fn new(acquired: Control) -> Result<Self, CloudFailure> {
        acquired.migration_spec()?;
        let until = acquired.migration.as_ref().ok_or_else(lost)?.lease_until_ms;
        let revision = acquired.revision;
        Ok(Self { acquired, until, terminal: None, revision, finished: false })
    }
    /// Check the local deadline often, but contact storage only halfway through the
    /// lease. This also permits deterministic tests with a clock advanced during I/O.
    pub fn due(&self, now: u64) -> bool {
        !self.finished && now >= self.until.saturating_sub(super::LEASE_MS / 2)
    }
    /// Register the exact terminal control before attempting its CAS. Concurrent
    /// renewal may then observe our successful activation/retirement without treating
    /// it as a lost lease. Revisions still advance through the authority's CAS.
    pub fn expect_completion(&mut self, terminal: Control) -> Result<(), CloudFailure> {
        terminal.may_read()?;
        let migration = self.acquired.migration.as_ref().ok_or_else(lost)?;
        let prepared = if migration.exposure == super::Exposure::Publishing {
            self.acquired.clone()
        } else {
            self.acquired.begin_output(&migration.token, 0)?
        };
        let expected = match migration.plan {
            super::Plan::Rebase => prepared.activate(&migration.token)?,
            super::Plan::AppendLog => prepared.retire(&migration.token, terminal.successor.clone().ok_or_else(lost)?)?,
        };
        let mut normalized = terminal.clone();
        normalized.revision = expected.revision;
        if terminal.revision < expected.revision || normalized != expected {
            return Err(CloudFailure::local("invalid upgrade completion fence"));
        }
        self.terminal = Some(terminal);
        Ok(())
    }
    /// `None` means the registered terminal transition is already committed. Every
    /// renewal is derived from the current control, not an earlier revision.
    pub fn renewal(&self, current: &Control, now: u64) -> Result<Option<Control>, CloudFailure> {
        current.may_read()?;
        if self.terminal.as_ref().is_some_and(|wanted| {
            let mut observed = current.clone();
            observed.revision = wanted.revision;
            current.revision >= wanted.revision.max(self.revision) && &observed == wanted
        }) {
            return Ok(None);
        }
        let ours = self.acquired.migration.as_ref().ok_or_else(lost)?;
        let actual = current.migration.as_ref().ok_or_else(lost)?;
        if current.state != State::Upgrading
            || current.library != self.acquired.library
            || current.epoch != self.acquired.epoch
            || current.active_metadata_root != self.acquired.active_metadata_root
            || current.schema != self.acquired.schema
            || current.control_version != self.acquired.control_version
            || current.minimum_reader != self.acquired.minimum_reader
            || current.minimum_writer != self.acquired.minimum_writer
            || current.revision < self.revision
            || actual.id != ours.id
            || actual.token != ours.token
            || actual.owner != ours.owner
            || actual.source != ours.source
            || actual.plan != ours.plan
            || actual.target_epoch != ours.target_epoch
            || actual.target_schema != ours.target_schema
        {
            return Err(lost());
        }
        Ok(Some(current.renew(&ours.token, now)?))
    }
    pub fn confirmed(&mut self, control: &Control) {
        self.revision = self.revision.max(control.revision);
        if let Some(migration) = &control.migration {
            self.until = migration.lease_until_ms;
        } else {
            self.finished = true;
        }
    }
}
pub fn lost() -> CloudFailure {
    CloudFailure::new(FailureKind::Contention, "upgrade ownership was taken over by another device; this result was not activated")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cloud_control::{Boundary, LEASE_MS, Plan};

    fn acquired() -> Control {
        Control::initial("library")
            .begin("migration", Plan::Rebase, Boundary { tip: "ab".repeat(32), generation: 1 }, "owner", &"cd".repeat(32), 100)
            .unwrap()
    }
    #[test]
    fn renewal_keeps_a_paused_worker_owned_then_stopped_renewal_allows_takeover() {
        let mut current = acquired();
        let mut lease = Lease::new(current.clone()).unwrap();
        for now in [LEASE_MS / 2 + 100, LEASE_MS + 100, LEASE_MS * 3 / 2 + 100, LEASE_MS * 2 + 100] {
            assert!(lease.due(now));
            current = lease.renewal(&current, now).unwrap().unwrap();
            lease.confirmed(&current);
            assert!(!lease.due(now));
            assert!(current.takeover("other", &"ef".repeat(32), now + 1, false).is_err());
        }
        let expired = current.migration.as_ref().unwrap().lease_until_ms;
        let replaced = current.takeover("other", &"ef".repeat(32), expired, false).unwrap().begin_output(&"ef".repeat(32), expired).unwrap();
        assert_eq!(lease.renewal(&replaced, expired).unwrap_err().kind, FailureKind::Contention);
        assert!(replaced.activate(&"cd".repeat(32)).is_err(), "stale activation is fenced even after output preparation");
    }
    #[test]
    fn renewal_accepts_only_the_registered_completion_and_same_frozen_source() {
        let current = acquired().begin_output(&"cd".repeat(32), 200).unwrap();
        let mut lease = Lease::new(current.clone()).unwrap();
        let terminal = current.activate(&"cd".repeat(32)).unwrap();
        assert!(lease.renewal(&terminal, 300).is_err());
        lease.expect_completion(terminal.clone()).unwrap();
        assert!(lease.renewal(&terminal, 300).unwrap().is_none());
        let mut altered = current.clone();
        altered.migration.as_mut().unwrap().source.generation += 1;
        assert_eq!(lease.renewal(&altered, 300).unwrap_err().kind, FailureKind::Contention);
    }
}
