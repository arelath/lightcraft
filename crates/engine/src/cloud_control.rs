//! Library control envelope: the small, stable record that says whether this build may
//! read or publish a cloud library, and the maintenance state machine for format
//! upgrades. Pure and wasm-safe; transports apply each transition through the same
//! authority that accepts ordinary metadata commits (the conditional head).
//!
//! Safety never depends on wall clocks: leases only decide when another upgrader may
//! *attempt* a takeover, and every transition is accepted only by a compare-and-swap
//! on the control revision/token it was derived from.
use serde::{Deserialize, Serialize};
use serde_json::Value;

mod capability;
pub use capability::{Converter, MigrationSpec};

/// Envelope format this build understands. A newer envelope is refused before any
/// other field of the library is interpreted.
pub const CONTROL_VERSION: u32 = 1;
/// Highest library schema this build can read.
pub const READER: u32 = 1;
/// Highest library schema this build can publish without losing fields.
pub const WRITER: u32 = 1;
/// Schema of the current wire set: head v4, batch v3, checkpoint wrapper v1,
/// mutation <= v2, portable state <= v3, append manifest v1.
pub const SCHEMA: u32 = 1;
/// Root of the first epoch: the existing conditional-head layout.
pub const LEGACY_ROOT: &str = "catalog";
/// Default upgrade lease. Expiry permits a takeover attempt, never ordinary writes.
pub const LEASE_MS: u64 = 10 * 60 * 1000;
const MAX_TEXT: usize = 128;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum State {
    Active,
    Upgrading,
    /// The library moved to another publication protocol; this authority accepts
    /// no further commits and points at its successor.
    Retired,
}

/// The exact frozen source of an upgrade: the head position at which maintenance began.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Boundary {
    pub tip: String,
    pub generation: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Plan {
    /// Rewrite the shared state into a new conditional-head epoch namespace.
    Rebase,
    /// Seed the append-log protocol and retire the conditional head.
    AppendLog,
}
impl Plan {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "rebase" => Some(Self::Rebase),
            "appendLog" => Some(Self::AppendLog),
            _ => None,
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Self::Rebase => "rebase",
            Self::AppendLog => "appendLog",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Migration {
    /// Stable for the whole operation, including takeovers: retries reuse its outputs.
    pub id: String,
    pub plan: Plan,
    pub target_epoch: u64,
    pub target_schema: u32,
    pub source: Boundary,
    pub owner: String,
    /// Fencing token: changes on every acquisition and takeover, and activation must
    /// present it. Readable by anyone who can read the head, so it stops a stale owner,
    /// not a malicious client with write credentials.
    pub token: String,
    pub lease_until_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Successor {
    pub protocol: String,
    pub epoch: u64,
    pub manifest: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Control {
    pub control_version: u32,
    pub library: String,
    /// Active generation of the library's metadata (not an edit sequence).
    pub epoch: u64,
    pub schema: u32,
    pub minimum_reader: u32,
    pub minimum_writer: u32,
    pub state: State,
    /// Metadata namespace of the active epoch, relative to the storage prefix.
    pub active_metadata_root: String,
    /// Increments on every control transition.
    pub revision: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub migration: Option<Migration>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub successor: Option<Successor>,
}

/// Why this build may not read or publish. Every variant retains local work.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Blocked {
    /// The envelope or schema is newer than this build can read.
    UpdateToRead(String),
    /// Readable, but publishing could lose newer fields.
    UpdateToWrite(String),
    /// A readable maintenance envelope describes a converter this build lacks.
    UpdateToUpgrade(String),
    /// An upgrade holds the publication authority; receiving continues.
    Maintenance {
        migration: String,
        plan: Plan,
        owner: String,
    },
    /// The library moved to another protocol.
    Retired(Successor),
    Invalid(String),
    /// Append mode has no CAS: two different control records at the same revision
    /// leave the intended state unknown, so publication pauses (receiving continues).
    Conflict(String),
}
impl std::fmt::Display for Blocked {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UpdateToRead(why) => write!(f, "update LightCraft to read this cloud library ({why}); local data retained"),
            Self::UpdateToWrite(why) => {
                write!(f, "update LightCraft to publish to this cloud library ({why}); browsing and local edits continue, pending work retained")
            }
            Self::UpdateToUpgrade(why) => {
                write!(f, "update LightCraft to complete this cloud library upgrade ({why}); remote data preserved, pending work retained")
            }
            Self::Maintenance { migration, plan, .. } => write!(
                f,
                "cloud library maintenance in progress ({} upgrade {migration}); publication paused, browsing and local edits continue, pending work retained",
                plan.name()
            ),
            Self::Retired(successor) => write!(
                f,
                "cloud library moved to {} (epoch {}); use Migrate Cloud Library… (cloud.upgradeLibrary) on this device to rejoin; pending work retained",
                successor.protocol, successor.epoch
            ),
            Self::Invalid(why) => write!(f, "invalid cloud library control record ({why}); remote data preserved, local work retained"),
            Self::Conflict(why) => write!(
                f,
                "conflicting cloud library control records ({why}); publication paused, browsing and local edits continue, pending work retained"
            ),
        }
    }
}
pub type Outcome<T> = std::result::Result<T, Blocked>;

fn valid_text(value: &str) -> bool {
    !value.is_empty() && value.len() <= MAX_TEXT && value.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'/'))
}
fn hex(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|b| b.is_ascii_digit() || matches!(b, b'a'..=b'f'))
}
/// Append-log epoch namespace, relative to the storage prefix.
pub fn append_root(library: &str, epoch: u64) -> String {
    if epoch <= 1 { format!("sync-v2/{library}") } else { format!("sync-v2/{library}/epochs/{epoch}") }
}
/// Epoch 1 keeps today's head layout; every later epoch gets its own namespace.
pub fn epoch_root(epoch: u64) -> String {
    if epoch <= 1 { LEGACY_ROOT.into() } else { format!("{LEGACY_ROOT}/epochs/{epoch}") }
}

impl Control {
    /// The implicit control of a library created before the envelope existed.
    pub fn initial(library: &str) -> Self {
        Self {
            control_version: CONTROL_VERSION,
            library: library.into(),
            epoch: 1,
            schema: SCHEMA,
            minimum_reader: 1,
            minimum_writer: 1,
            state: State::Active,
            active_metadata_root: LEGACY_ROOT.into(),
            revision: 0,
            migration: None,
            successor: None,
        }
    }
    /// The implicit control of an append-log library with no control record.
    pub fn initial_append(library: &str) -> Self {
        Self { active_metadata_root: append_root(library, 1), ..Self::initial(library) }
    }
    /// Decide readability from the stable envelope fields before decoding the rest,
    /// so a newer control format yields "update" instead of "malformed".
    pub fn decode(value: &Value) -> Outcome<Self> {
        let number = |name: &str| value.get(name).and_then(Value::as_u64).ok_or_else(|| Blocked::Invalid(format!("missing {name}")));
        let version = number("controlVersion")?;
        if version > u64::from(CONTROL_VERSION) {
            return Err(Blocked::UpdateToRead(format!("control format {version}")));
        }
        let reader = number("minimumReader")?;
        if reader > u64::from(READER) {
            return Err(Blocked::UpdateToRead(format!("schema reader {reader}")));
        }
        let control: Self = serde_json::from_value(value.clone()).map_err(|e| Blocked::Invalid(e.to_string()))?;
        control.validate()?;
        Ok(control)
    }
    pub fn validate(&self) -> Outcome<()> {
        let bad = |why: &str| Err(Blocked::Invalid(why.into()));
        if self.control_version == 0 || self.control_version > CONTROL_VERSION {
            return bad("control version");
        }
        if !valid_text(&self.library) || self.epoch == 0 || self.schema == 0 {
            return bad("library, epoch or schema");
        }
        if self.minimum_reader == 0 || self.minimum_reader > self.minimum_writer || self.minimum_writer > self.schema {
            return bad("reader/writer compatibility");
        }
        if self.active_metadata_root != epoch_root(self.epoch) && self.active_metadata_root != append_root(&self.library, self.epoch) {
            return bad("active metadata root does not belong to its epoch");
        }
        match (self.state, &self.migration, &self.successor) {
            (State::Active, None, None) => {}
            (State::Upgrading, Some(m), None) => {
                if !valid_text(&m.id) || !valid_text(&m.owner) || !hex(&m.token) || !hex(&m.source.tip) || m.source.generation == 0 {
                    return bad("migration identity");
                }
                if m.target_epoch != self.epoch.saturating_add(1) || m.target_schema < self.schema {
                    return bad("migration target");
                }
            }
            (State::Retired, None, Some(s)) => {
                if !valid_text(&s.protocol) || s.epoch == 0 || !hex(&s.manifest) {
                    return bad("successor");
                }
            }
            _ => return bad("state does not match its migration/successor fields"),
        }
        Ok(())
    }
    /// Reading: any valid, readable control (including maintenance and retirement).
    pub fn may_read(&self) -> Outcome<()> {
        self.validate()?;
        if self.minimum_reader > READER {
            return Err(Blocked::UpdateToRead(format!("schema reader {}", self.minimum_reader)));
        }
        Ok(())
    }
    /// Ordinary publication. Checked on every reread, immediately before the CAS that
    /// carries this exact control forward.
    pub fn may_publish(&self) -> Outcome<()> {
        self.may_read()?;
        if self.minimum_writer > WRITER {
            return Err(Blocked::UpdateToWrite(format!("schema writer {}", self.minimum_writer)));
        }
        match (&self.state, &self.migration, &self.successor) {
            (State::Active, _, _) => Ok(()),
            (State::Upgrading, Some(m), _) => Err(Blocked::Maintenance { migration: m.id.clone(), plan: m.plan, owner: m.owner.clone() }),
            (State::Retired, _, Some(s)) => Err(Blocked::Retired(s.clone())),
            _ => Err(Blocked::Invalid("state".into())),
        }
    }
    fn next(&self) -> Outcome<Self> {
        let mut next = self.clone();
        next.revision = self.revision.checked_add(1).ok_or_else(|| Blocked::Invalid("control revision exhausted".into()))?;
        Ok(next)
    }
    /// Active → Upgrading, freezing `source` (the head this transition replaces).
    pub fn begin(&self, id: &str, plan: Plan, source: Boundary, owner: &str, token: &str, now_ms: u64) -> Outcome<Self> {
        self.may_publish()?;
        let spec = MigrationSpec::resolve(plan, self.schema, SCHEMA.max(self.schema))?;
        let mut next = self.next()?;
        next.state = State::Upgrading;
        next.migration = Some(Migration {
            id: id.into(),
            plan,
            target_epoch: self.epoch.checked_add(1).ok_or_else(|| Blocked::Invalid("epoch exhausted".into()))?,
            target_schema: spec.target_schema(),
            source,
            owner: owner.into(),
            token: token.into(),
            lease_until_ms: now_ms.saturating_add(LEASE_MS),
        });
        next.validate()?;
        Ok(next)
    }
    fn owned(&self, token: &str) -> Outcome<&Migration> {
        self.validate()?;
        match &self.migration {
            Some(m) if m.token == token => Ok(m),
            Some(m) => Err(Blocked::Maintenance { migration: m.id.clone(), plan: m.plan, owner: m.owner.clone() }),
            None => Err(Blocked::Invalid("no migration in progress".into())),
        }
    }
    /// The enclosing control retains the frozen source schema until activation.
    /// Resolve it with the actual target before taking ownership or producing data.
    pub fn migration_spec(&self) -> Outcome<MigrationSpec> {
        self.may_read()?;
        let m = self.migration.as_ref().ok_or_else(|| Blocked::Invalid("no migration in progress".into()))?;
        MigrationSpec::resolve(m.plan, self.schema, m.target_schema)
    }
    pub fn renew(&self, token: &str, now_ms: u64) -> Outcome<Self> {
        self.owned(token)?;
        let mut next = self.next()?;
        if let Some(m) = &mut next.migration {
            m.lease_until_ms = now_ms.saturating_add(LEASE_MS);
        }
        Ok(next)
    }
    /// A new upgrader resumes the *same* migration (same id, plan and frozen source)
    /// with a new token, so a stale owner can no longer activate its result.
    pub fn takeover(&self, owner: &str, token: &str, now_ms: u64, force: bool) -> Outcome<Self> {
        self.validate()?;
        self.migration_spec()?;
        let Some(current) = &self.migration else { return Err(Blocked::Invalid("no migration in progress".into())) };
        if !force && now_ms < current.lease_until_ms {
            return Err(Blocked::Maintenance { migration: current.id.clone(), plan: current.plan, owner: current.owner.clone() });
        }
        if current.token == token {
            return Err(Blocked::Invalid("takeover requires a fresh token".into()));
        }
        let mut next = self.next()?;
        if let Some(m) = &mut next.migration {
            m.owner = owner.into();
            m.token = token.into();
            m.lease_until_ms = now_ms.saturating_add(LEASE_MS);
        }
        next.validate()?;
        Ok(next)
    }
    /// Before activation, the owner can return the library to its unchanged epoch.
    pub fn cancel(&self, token: &str) -> Outcome<Self> {
        self.owned(token)?;
        self.migration_spec()?;
        let mut next = self.next()?;
        next.state = State::Active;
        next.migration = None;
        Ok(next)
    }
    /// Upgrading → Active at the target epoch; only the current token may do this.
    pub fn activate(&self, token: &str) -> Outcome<Self> {
        let m = self.owned(token)?.clone();
        let spec = self.migration_spec()?;
        if m.plan != Plan::Rebase {
            return Err(Blocked::Invalid("this plan retires the head instead of activating an epoch".into()));
        }
        let mut next = self.next()?;
        next.state = State::Active;
        next.epoch = m.target_epoch;
        next.schema = m.target_schema;
        next.minimum_reader = spec.minimum_reader();
        next.minimum_writer = spec.minimum_writer();
        next.active_metadata_root = epoch_root(m.target_epoch);
        next.migration = None;
        next.validate()?;
        Ok(next)
    }
    /// Upgrading → Retired: the head becomes a permanent pointer to its successor.
    pub fn retire(&self, token: &str, successor: Successor) -> Outcome<Self> {
        let m = self.owned(token)?;
        self.migration_spec()?;
        if m.plan != Plan::AppendLog {
            return Err(Blocked::Invalid("only a protocol migration retires the head".into()));
        }
        let mut next = self.next()?;
        next.state = State::Retired;
        next.migration = None;
        next.successor = Some(successor);
        next.validate()?;
        Ok(next)
    }
}

/// A fresh unpredictable fencing token for one upgrade acquisition or takeover (the browser derives
/// its token from Web Crypto the same way).
#[cfg(not(target_arch = "wasm32"))]
pub fn token() -> Result<String, String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|_| "secure token generation failed".to_string())?;
    Ok(crate::assets::sha256(&bytes).0)
}

/// Mutation translation across schemas. No current schema needs it, so it is refused:
/// a future translator must keep the original mutation (identity and digest) and record
/// a durable original→translated provenance mapping before publishing the new one.
pub fn translate_mutation(from_schema: u32, to_schema: u32) -> Result<(), String> {
    if from_schema == SCHEMA && to_schema == SCHEMA {
        return Ok(());
    }
    Err(format!("translating pending mutations from schema {from_schema} to {to_schema} is unsupported in this build; pending work retained"))
}

#[cfg(test)]
mod tests;
