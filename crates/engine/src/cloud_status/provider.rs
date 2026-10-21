//! Recognize only known provider refusals. Never display an arbitrary error response.
use quick_xml::events::Event;

pub const MAX_ERROR_BODY: usize = 16 << 10;

/// B2's S3 facade collapses its download-cap code into AccessDenied. Match its
/// documented response exactly, rather than treating every HTTP 403 as a cap.
pub fn b2_download_cap(status: u16, bytes: &[u8]) -> bool {
    if status != 403 || bytes.len() > MAX_ERROR_BODY {
        return false;
    }
    fields(bytes).is_some_and(|(code, message)| {
        code == "download_cap_exceeded"
            || (code == "AccessDenied"
                && message == "Cannot download file, download bandwidth or transaction (Class B) cap exceeded. See the Caps & Alerts page to increase your cap.")
    })
}

fn fields(bytes: &[u8]) -> Option<(String, String)> {
    let mut reader = quick_xml::NsReader::from_reader(bytes);
    let mut path = Vec::<String>::new();
    let (mut code, mut message) = (None::<String>, None::<String>);
    let mut root = false;
    for _ in 0..512 {
        let (namespace, event) = reader.read_resolved_event().ok()?;
        match event {
            Event::Start(ref start) | Event::Empty(ref start) => {
                if !matches!(
                    namespace,
                    quick_xml::name::ResolveResult::Unbound
                        | quick_xml::name::ResolveResult::Bound(quick_xml::name::Namespace(b"http://s3.amazonaws.com/doc/2006-03-01/"))
                ) {
                    return None;
                }
                for attribute in start.attributes() {
                    attribute.ok()?;
                }
                let name = std::str::from_utf8(start.local_name().as_ref()).ok()?.to_owned();
                if path.len() >= 4 || name.len() > 80 || (path.is_empty() && (root || name != "Error")) {
                    return None;
                }
                if path.is_empty() {
                    root = true;
                } else if path.len() == 1 {
                    let field = match name.as_str() {
                        "Code" => Some(&mut code),
                        "Message" => Some(&mut message),
                        _ => None,
                    };
                    if let Some(field) = field {
                        if field.is_some() {
                            return None;
                        }
                        *field = Some(String::new());
                    }
                } else if path.last().is_some_and(|name| matches!(name.as_str(), "Code" | "Message")) {
                    return None;
                }
                path.push(name);
                if matches!(event, Event::Empty(_)) {
                    path.pop();
                }
            }
            Event::End(end) => {
                let expected = path.pop()?;
                if expected.as_bytes() != end.local_name().as_ref() {
                    return None;
                }
            }
            Event::Text(text) => {
                let text = text.xml_content().ok()?;
                if path.is_empty() && !text.trim().is_empty() {
                    return None;
                }
                if path.len() == 2 {
                    match path.last()?.as_str() {
                        "Code" => code.as_mut()?.push_str(&text),
                        "Message" => message.as_mut()?.push_str(&text),
                        _ => {}
                    }
                }
            }
            Event::GeneralRef(reference) => {
                let encoded = format!("&{};", reference.decode().ok()?);
                let text = quick_xml::escape::unescape(&encoded).ok()?;
                if path.len() == 2 {
                    match path.last()?.as_str() {
                        "Code" => code.as_mut()?.push_str(&text),
                        "Message" => message.as_mut()?.push_str(&text),
                        _ => {}
                    }
                }
            }
            Event::DocType(_) | Event::CData(_) => return None,
            Event::Eof => return (root && path.is_empty()).then_some((code?, message.unwrap_or_default())),
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    const MESSAGE: &str =
        "Cannot download file, download bandwidth or transaction (Class B) cap exceeded. See the Caps &amp; Alerts page to increase your cap.";

    #[test]
    fn only_a_bounded_structured_b2_cap_is_recognized() {
        let xml = format!("<Error><Code>AccessDenied</Code><Message>{MESSAGE}</Message><RequestId>not displayed</RequestId></Error>");
        assert!(b2_download_cap(403, xml.as_bytes()));
        assert!(b2_download_cap(403, xml.replace("<Error>", "<Error xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">").as_bytes()));
        assert!(!b2_download_cap(401, xml.as_bytes()));
        for other in [
            xml.replace("AccessDenied", "SignatureDoesNotMatch"),
            xml.replace(MESSAGE, "Access denied"),
            xml.replace("</Code>", "</Code><Code>AccessDenied</Code>"),
            xml.replace("</Message>", "</Message><Message/>"),
            xml.replace("<Error>", "<Other>"),
            xml.replace("<Error>", "<Error xmlns=\"https://wrong.invalid/\">"),
            format!("{xml}{xml}"),
            format!("<!DOCTYPE Error>{xml}"),
            xml.replace("</Error>", ""),
            format!("{xml}{}", " ".repeat(MAX_ERROR_BODY)),
            "HTTP 403: download cap exceeded".into(),
        ] {
            assert!(!b2_download_cap(403, other.as_bytes()), "{other}");
        }
        assert!(b2_download_cap(403, b"<Error><Code>download_cap_exceeded</Code></Error>"));
    }

    #[test]
    fn provider_limit_keeps_http_provenance_and_stops_automatic_retry() {
        use crate::cloud_status::{CloudFailure, Diagnostic, FailureKind, PollHealth, Recovery};
        let error = CloudFailure::from(crate::assets::AssetError::ProviderLimit { status: 403 });
        assert_eq!(error.kind, FailureKind::Capacity);
        assert_eq!(error.http_status, Some(403));
        assert_eq!(error.remedy, Some(Recovery::ReviewProviderLimits));
        assert!(error.action().contains("Backblaze Caps & Alerts"));
        assert!(!error.retryable());
        let mut health = PollHealth::default();
        assert_eq!(health.fail_typed(&error, Some(0), None), None);
        let value = serde_json::json!({"syncEnabled":true,"health":{"sync":{"lastError":crate::cloud_status::Failure::classified(&error,None)}}});
        let diagnostic = Diagnostic::from_overview(&value, false).unwrap();
        assert_eq!(diagnostic.recovery(), Some(Recovery::ReviewProviderLimits));
        assert_eq!(diagnostic.short_cause(), "Storage provider limit reached.");
        assert!(diagnostic.advice().contains("transaction caps"));
    }
}
