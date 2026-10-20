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
    Read {
        /// Only an explicit upgrade may treat an uninitialized authority as an
        /// initialization candidate. Ordinary reads remain fail-closed.
        #[serde(rename = "forUpgrade", default, skip_serializing_if = "std::ops::Not::not")]
        for_upgrade: bool,
    },
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
impl Reply {
    /// Inspect compatibility before strict Head deserialization: an updated
    /// signer may return a head this browser cannot yet understand.
    pub fn decode(bytes: &[u8]) -> Result<Self, CloudFailure> {
        use crate::cloud_status::{FailureKind, Recovery};
        let incompatible = || {
            CloudFailure::new(FailureKind::Unsupported, "personal cloud control response is not understood").with_remedy(Recovery::CheckCompatibility)
        };
        if bytes.len() > RESPONSE_LIMIT {
            return Err(CloudFailure::new(FailureKind::Capacity, "personal cloud control response exceeds its bounded size"));
        }
        let value: serde_json::Value = serde_json::from_slice(bytes).map_err(|_| incompatible())?;
        if value.get("result").and_then(serde_json::Value::as_str) == Some("read")
            && let Some(head) = value.get("current").and_then(|s| s.get("head"))
        {
            let bytes = serde_json::to_vec(head).map_err(CloudFailure::from)?;
            Head::decode(&bytes)?;
            if bytes.len() > HEAD_LIMIT {
                return Err(CloudFailure::new(FailureKind::Capacity, "personal cloud authority head exceeds its bounded size"));
            }
        }
        serde_json::from_value(value).map_err(|_| incompatible())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cloud_status::{FailureKind, Recovery};
    use serde_json::json;
    #[test]
    fn newer_head_and_control_replies_require_a_browser_update_before_strict_decoding() {
        for head in [
            json!({"version":super::super::HEAD_VERSION+1,"futureHead":true}),
            json!({"version":super::super::HEAD_VERSION,"control":{"controlVersion":crate::cloud_control::CONTROL_VERSION+1,"futureControl":true}}),
        ] {
            let bytes = serde_json::to_vec(&json!({"result":"read","current":{"head":head,"token":"b2:1:unused"}})).unwrap();
            let error = Reply::decode(&bytes).err().unwrap();
            assert_eq!(error.remedy, Some(Recovery::Update));
            assert_eq!(error.kind, FailureKind::Unsupported);
        }
    }
    #[test]
    fn reply_bounds_and_typed_failure_provenance_are_preserved() {
        let failure = CloudFailure { retry_after_ms: Some(7000), ..CloudFailure::http("B2 control", 503).with_remedy(Recovery::FinishUpgrade) };
        let bytes = serde_json::to_vec(&Reply::Failed { failure: failure.clone() }).unwrap();
        let Reply::Failed { failure: decoded } = Reply::decode(&bytes).unwrap() else { panic!("failure was not preserved") };
        assert_eq!(serde_json::to_value(decoded).unwrap(), serde_json::to_value(failure).unwrap());
        assert!(matches!(Reply::decode(br#"{"result":"read","current":null}"#).unwrap(), Reply::Read { current: None }));
        assert!(matches!(Reply::decode(br#"{"result":"updated"}"#).unwrap(), Reply::Updated {}));
        assert_eq!(Reply::decode(br#"{"result":"future"}"#).err().unwrap().remedy, Some(Recovery::CheckCompatibility));
        assert_eq!(Reply::decode(&vec![b' '; RESPONSE_LIMIT + 1]).err().unwrap().kind, FailureKind::Capacity);
    }
}
