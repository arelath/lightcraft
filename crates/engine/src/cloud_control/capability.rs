//! Executable upgrade capabilities, separate from readable maintenance envelopes and
//! ownership. A valid envelope may describe work only a newer build can perform.
use super::{Blocked, Outcome, Plan};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Converter {
    Identity,
}

/// Resolved by this build's converter registry. Fields are private so callers cannot
/// construct a capability merely by copying a remote migration's claimed target.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MigrationSpec {
    plan: Plan,
    source_schema: u32,
    target_schema: u32,
    converter: Converter,
    minimum_reader: u32,
    minimum_writer: u32,
}

// Schema 1 has only identity rebase/protocol conversion. Add real translators here
// with their actual compatibility requirements when a schema change needs one.
const SUPPORTED: [MigrationSpec; 2] = [
    MigrationSpec { plan: Plan::Rebase, source_schema: 1, target_schema: 1, converter: Converter::Identity, minimum_reader: 1, minimum_writer: 1 },
    MigrationSpec { plan: Plan::AppendLog, source_schema: 1, target_schema: 1, converter: Converter::Identity, minimum_reader: 1, minimum_writer: 1 },
];

impl MigrationSpec {
    pub fn resolve(plan: Plan, source_schema: u32, target_schema: u32) -> Outcome<Self> {
        SUPPORTED.iter().find(|s| s.plan == plan && s.source_schema == source_schema && s.target_schema == target_schema).copied().ok_or_else(|| {
            Blocked::UpdateToUpgrade(format!(
                "{} from schema {source_schema} to {target_schema} has no supported converter in this build",
                plan.name()
            ))
        })
    }
    pub fn converter(self) -> Converter {
        self.converter
    }
    pub fn source_schema(self) -> u32 {
        self.source_schema
    }
    pub fn target_schema(self) -> u32 {
        self.target_schema
    }
    pub fn minimum_reader(self) -> u32 {
        self.minimum_reader
    }
    pub fn minimum_writer(self) -> u32 {
        self.minimum_writer
    }
}
