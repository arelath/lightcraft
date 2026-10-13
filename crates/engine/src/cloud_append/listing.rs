//! Bounded ListObjectsV2 codec shared by native S3 and browser Fetch.
use super::{Result, invalid};
use quick_xml::events::Event;
use quick_xml::name::{Namespace, ResolveResult};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ListRequest {
    pub prefix: String,
    #[serde(default)]
    pub delimiter: bool,
    #[serde(default)]
    pub continuation: Option<String>,
    #[serde(default)]
    pub start_after: Option<String>,
}
impl ListRequest {
    pub fn validate(&self, scope: &str) -> Result<()> {
        let scope = scope.trim_end_matches('/');
        if self.prefix.len() > 2048
            || (!scope.is_empty() && !self.prefix.starts_with(&format!("{scope}/")))
            || self.prefix.trim_end_matches('/').split('/').any(|p| p.is_empty() || matches!(p, "." | ".."))
            || self.prefix.contains('\\')
            || self.prefix.chars().any(char::is_control)
            || self.continuation.as_ref().is_some_and(|v| v.is_empty() || v.len() > 8192 || v.chars().any(char::is_control))
            || self.start_after.as_ref().is_some_and(|v| !v.starts_with(&self.prefix) || v.len() > 2048 || v.chars().any(char::is_control))
            || (self.continuation.is_some() && self.start_after.is_some())
        {
            return Err(invalid("invalid or out-of-scope object listing"));
        }
        Ok(())
    }
}
#[derive(Debug)]
pub struct Page {
    pub keys: Vec<String>,
    pub prefixes: Vec<String>,
    pub next: Option<String>,
}
pub fn parse(bytes: &[u8], request: &ListRequest) -> Result<Page> {
    let bad = || invalid("invalid, oversized or out-of-scope ListObjectsV2 response");
    if bytes.len() > 4 << 20 {
        return Err(bad());
    }
    let mut reader = quick_xml::NsReader::from_reader(bytes);
    reader.config_mut().check_end_names = true;
    let mut path = Vec::<String>::new();
    let mut text = String::new();
    let mut fields = BTreeMap::new();
    let mut keys = Vec::new();
    let mut prefixes = Vec::new();
    let mut root = false;
    for _ in 0..100_000 {
        let (ns, event) = reader.read_resolved_event().map_err(|_| bad())?;
        match event {
            Event::Start(ref s) | Event::Empty(ref s) => {
                if !matches!(ns, ResolveResult::Unbound | ResolveResult::Bound(Namespace(b"http://s3.amazonaws.com/doc/2006-03-01/"))) {
                    return Err(bad());
                }
                let name = std::str::from_utf8(s.local_name().as_ref()).map_err(|_| bad())?.to_owned();
                if path.len() >= 16 || name.len() > 80 {
                    return Err(bad());
                }
                if path.is_empty() {
                    if root || name != "ListBucketResult" {
                        return Err(bad());
                    }
                    root = true;
                }
                for a in s.attributes() {
                    a.map_err(|_| bad())?;
                }
                path.push(name);
                text.clear();
                if matches!(event, Event::Empty(_)) {
                    path.pop();
                }
            }
            Event::Text(t) => {
                let value = t.xml_content().map_err(|_| bad())?;
                if text.len().saturating_add(value.len()) > 8192 {
                    return Err(bad());
                }
                text.push_str(&value);
            }
            Event::GeneralRef(r) => {
                let v = format!("&{};", r.decode().map_err(|_| bad())?);
                text.push_str(&quick_xml::escape::unescape(&v).map_err(|_| bad())?);
                if text.len() > 8192 {
                    return Err(bad());
                }
            }
            Event::End(e) => {
                let name = path.pop().ok_or_else(bad)?;
                if e.local_name().as_ref() != name.as_bytes() {
                    return Err(bad());
                }
                if path.len() == 1 && matches!(name.as_str(), "Prefix" | "IsTruncated" | "NextContinuationToken") {
                    if fields.insert(name, text.clone()).is_some() {
                        return Err(bad());
                    }
                } else if path.len() == 2 && name == "Key" && path.last().is_some_and(|p| p == "Contents") {
                    keys.push(text.clone());
                } else if path.len() == 2 && name == "Prefix" && path.last().is_some_and(|p| p == "CommonPrefixes") {
                    prefixes.push(text.clone());
                }
                if keys.len().saturating_add(prefixes.len()) > 1000 {
                    return Err(bad());
                }
                text.clear();
            }
            Event::DocType(_) | Event::CData(_) => return Err(bad()),
            Event::Eof => {
                if !root || !path.is_empty() || fields.get("Prefix") != Some(&request.prefix) {
                    return Err(bad());
                }
                let next = match fields.get("IsTruncated").map(String::as_str) {
                    Some("false") => None,
                    Some("true") => Some(
                        fields
                            .remove("NextContinuationToken")
                            .filter(|s| !s.is_empty() && s.len() <= 8192 && !s.chars().any(char::is_control))
                            .ok_or_else(bad)?,
                    ),
                    _ => return Err(bad()),
                };
                for k in keys.iter().chain(&prefixes) {
                    if !k.starts_with(&request.prefix)
                        || k.len() > 2048
                        || k.contains('\\')
                        || k.chars().any(char::is_control)
                        || k.trim_end_matches('/').split('/').any(|p| p.is_empty() || matches!(p, "." | ".."))
                    {
                        return Err(bad());
                    }
                }
                if !request.delimiter && !prefixes.is_empty() {
                    return Err(bad());
                }
                return Ok(Page { keys, prefixes, next });
            }
            _ => {}
        }
    }
    Err(bad())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn xml_pages_unescape_keys_and_reject_scope_tokens_dtd_and_false_truncation() {
        let r = ListRequest { prefix: "p/ops/".into(), delimiter: false, continuation: None, start_after: None };
        r.validate("p").unwrap();
        let page=parse(b"<ListBucketResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"><Prefix>p/ops/</Prefix><IsTruncated>true</IsTruncated><Contents><Key>p/ops/a&amp;b</Key><Size>4</Size></Contents><NextContinuationToken>a&amp;b</NextContinuationToken></ListBucketResult>",&r).unwrap();
        assert_eq!(page.keys, vec!["p/ops/a&b"]);
        assert_eq!(page.next.as_deref(), Some("a&b"));
        for bytes in [
            b"<!DOCTYPE root><ListBucketResult/>".as_slice(),
            b"<ListBucketResult><Prefix>p/ops/</Prefix><IsTruncated>true</IsTruncated></ListBucketResult>",
            b"<ListBucketResult><Prefix>p/ops/</Prefix><IsTruncated>false</IsTruncated><Contents><Key>other/key</Key></Contents></ListBucketResult>",
        ] {
            assert!(parse(bytes, &r).is_err());
        }
        let mut hostile = r;
        hostile.continuation = Some("x".repeat(8193));
        assert!(hostile.validate("p").is_err());
    }
}
