//! Wire contract for the personal signer's narrow B2 authority endpoint.
//! Native account authorization and bucket metadata never cross this boundary.
use super::Head;
use crate::cloud_status::CloudFailure;
use serde::{Deserialize, Serialize};

pub const REQUEST_LIMIT: usize = 32 << 10;
pub const RESPONSE_LIMIT: usize = 32 << 10;
pub const HEAD_LIMIT: usize = 16 << 10;

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Request {
    pub connection: String,
    pub prefix: String,
    /// Canonical public bucket URL. Binds an otherwise reused connection ID to
    /// the same destination before the signer can change any remote authority.
    pub bucket_root: String,
    pub action: Action,
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "camelCase", deny_unknown_fields)]
pub enum Action {
    Read {},
    Initialize {
        head: Box<Head>,
    },
    Write {
        expected: String,
        head: Box<Head>,
    },
    /// Retire only the legacy source bound to the exact committed B2 authority.
    Fence {
        expected: String,
        library: String,
    },
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Snapshot {
    pub head: Box<Head>,
    pub token: String,
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "camelCase", deny_unknown_fields)]
pub enum Reply {
    Read {
        current: Option<Snapshot>,
    },
    /// The caller must reread; an accepted response is not a replay/ACK proof.
    Updated {},
    Failed {
        failure: CloudFailure,
    },
}
