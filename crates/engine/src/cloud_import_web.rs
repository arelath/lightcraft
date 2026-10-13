//! Native durable revision commands require the browser staging adapter.
use crate::{EngineError, Result, Session};
use lightcraft_catalog::PhotoId;
use serde_json::Value;

fn unsupported() -> Result<Value> {
    Err(EngineError::Other("managed revision staging requires the browser storage adapter".into()))
}
pub(crate) fn convert_dng(_: &mut Session, _: &[PhotoId], _: &Value) -> Result<Value> {
    unsupported()
}
pub(crate) fn save_xmp(_: &mut Session, _: &[PhotoId], _: &Value) -> Result<Value> {
    unsupported()
}
pub(crate) fn reload_working(_: &mut Session, _: &[PhotoId], _: &Value) -> Result<Value> {
    unsupported()
}
pub(crate) fn edit_external(_: &mut Session, _: &[PhotoId], _: &crate::export::ExportOptions, _: &Value) -> Result<Value> {
    unsupported()
}
