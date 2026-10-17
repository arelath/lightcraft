//! Read-only recovery pages. Cursors bind the request to one library/revision;
//! fetching a page never resolves, restores or publishes anything.
use crate::{Result, Session};
use lightcraft_catalog::replication::SyncState;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const COMMAND: &str = "cloud.syncStatus";
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
enum Section {
    Conflicts,
    Choices,
    Deleted,
    Structural,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Request {
    section: Section,
    #[serde(default)]
    search: String,
    cursor: Option<String>,
    limit: Option<u64>,
    entity: Option<String>,
    field: Option<String>,
}
impl Request {
    fn validate(&mut self) -> Result<usize> {
        if self.search.len() > 1024 || self.search.chars().count() > 256 || self.search.chars().any(char::is_control) {
            return Err(super::bad(COMMAND, "search must contain at most 256 characters without control characters"));
        }
        self.search = self.search.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase();
        if self.cursor.as_ref().is_some_and(|cursor| cursor.len() > 256 || !cursor.is_ascii()) {
            return Err(super::bad(COMMAND, "invalid recovery cursor; refresh this list"));
        }
        if self.section == Section::Choices {
            for value in [&self.entity, &self.field] {
                if value.as_ref().is_none_or(|value| value.is_empty() || value.len() > 256 || value.chars().any(char::is_control)) {
                    return Err(super::bad(COMMAND, "choices requires a bounded entity and field"));
                }
            }
        } else if self.entity.is_some() || self.field.is_some() {
            return Err(super::bad(COMMAND, "entity and field are only used for choices pages"));
        }
        let (default, maximum) = match self.section {
            Section::Choices => (8, 32),
            Section::Deleted => (50, 200),
            _ => (16, 64),
        };
        let limit = self.limit.unwrap_or(default);
        if limit == 0 || limit > maximum {
            return Err(super::bad(COMMAND, format!("page limit must be between 1 and {maximum}")));
        }
        usize::try_from(limit).map_err(|_| super::bad(COMMAND, "page limit is too large"))
    }
    fn matches(&self, text: &str) -> bool {
        let text = text.to_lowercase();
        self.search.split_whitespace().all(|term| text.contains(term))
    }
    fn scope(&self, state: &SyncState, generation: u64, limit: usize) -> Result<String> {
        let encoded = serde_json::to_vec(&json!([state.library, generation, self.section, self.search, self.entity, self.field, limit]))
            .map_err(|error| super::bad(COMMAND, error.to_string()))?;
        Ok(Sha256::digest(encoded).iter().map(|byte| format!("{byte:02x}")).collect())
    }
    fn offset(&self, revision: u64, scope: &str) -> Result<usize> {
        let Some(cursor) = &self.cursor else { return Ok(0) };
        let mut parts = cursor.split('.');
        let invalid = || super::bad(COMMAND, "recovery cursor is stale or belongs to another list; refresh this list");
        let (Some("cp1"), Some(version), Some(offset), Some(bound)) = (parts.next(), parts.next(), parts.next(), parts.next()) else {
            return Err(invalid());
        };
        if parts.next().is_some() || bound != scope || u64::from_str_radix(version, 16).ok() != Some(revision) {
            return Err(invalid());
        }
        let offset = u64::from_str_radix(offset, 16).map_err(|_| invalid())?;
        usize::try_from(offset).map_err(|_| invalid())
    }
}
struct Page {
    offset: usize,
    limit: usize,
    total: usize,
    items: Vec<Value>,
}
impl Page {
    fn push(&mut self, build: impl FnOnce() -> Value) {
        if self.total >= self.offset && self.items.len() < self.limit {
            self.items.push(build());
        }
        self.total = self.total.saturating_add(1);
    }
    fn view(self, request: &Request, revision: u64, scope: &str) -> Result<Value> {
        if self.offset > self.total {
            return Err(super::bad(COMMAND, "recovery cursor is past this list; refresh this list"));
        }
        let cursor = |offset: usize| format!("cp1.{revision:x}.{offset:x}.{scope}");
        let next = self.offset.saturating_add(self.items.len());
        Ok(json!({"section":request.section,"search":request.search,"revision":revision,"items":self.items,"total":self.total,
            "offset":self.offset,"limit":self.limit,"nextCursor":(next<self.total).then(||cursor(next)),
            "previousCursor":(self.offset>0).then(||cursor(self.offset.saturating_sub(self.limit)))}))
    }
}

pub(super) fn page(session: &Session, params: &Value) -> Result<Value> {
    let mut request: Request = serde_json::from_value(params.clone()).map_err(|error| super::bad(COMMAND, error.to_string()))?;
    let limit = request.validate()?;
    let state = session.catalog.sync_state().ok_or_else(|| super::bad(COMMAND, "open a cloud library to browse its recovery records"))?;
    let revision = session.catalog.revision;
    let scope = request.scope(state, session.library_generation(), limit)?;
    let offset = request.offset(revision, &scope)?;
    let mut page = Page { offset, limit, total: 0, items: Vec::with_capacity(limit) };
    match request.section {
        Section::Conflicts => {
            for (entity, fields) in &state.documents {
                for (field, register) in fields {
                    if !register.0.values().next().is_some_and(|first| register.0.values().any(|value| value != first)) {
                        continue;
                    }
                    if request.search.is_empty()
                        || request.matches(&format!("{entity} {field} {}", super::entity_name(state, entity).unwrap_or_default()))
                    {
                        page.push(|| super::conflict_view(state, entity, field, register));
                    }
                }
            }
        }
        Section::Choices => {
            if let (Some(entity), Some(field)) = (&request.entity, &request.field)
                && let Some(register) = state.documents.get(entity).and_then(|fields| fields.get(field))
            {
                let winner = state.winning(entity, field).map(|(stamp, _)| stamp.id());
                let mut choices: Vec<_> = register.0.iter().collect();
                let sequence = |revision: &str| revision.rsplit_once('-').and_then(|(_, n)| n.parse::<u64>().ok()).unwrap_or(0);
                choices.sort_by_key(|(revision, _)| (winner.as_deref() != Some(revision.as_str()), std::cmp::Reverse(sequence(revision))));
                for (revision, value) in choices {
                    if !request.search.is_empty() && !request.matches(&format!("{revision} {value}")) {
                        continue;
                    }
                    page.push(|| {
                        let (kind, preview) = super::preview(value);
                        json!({"revision":revision,"current":winner.as_deref()==Some(revision.as_str()),
                            "thisDevice":revision.rsplit_once('-').is_some_and(|(device,_)|device==state.device),"kind":kind,"preview":preview})
                    });
                }
            }
        }
        Section::Deleted => {
            for entity in state.documents.keys().filter(|entity| entity.starts_with("photo:")) {
                if state.winning(entity, "deleted").is_none_or(|(_, value)| value.as_bool() != Some(true)) {
                    continue;
                }
                let name = super::entity_name(state, entity);
                if request.search.is_empty() || request.matches(&format!("{entity} {}", name.as_deref().unwrap_or_default())) {
                    page.push(|| json!({"entity":entity,"name":name}));
                }
            }
        }
        Section::Structural => {
            let mut conflicts = crate::cloud_sync::structural_conflicts(&session.catalog);
            conflicts.sort_by(|a, b| (a["entity"].as_str(), a["field"].as_str()).cmp(&(b["entity"].as_str(), b["field"].as_str())));
            for conflict in conflicts {
                if request.search.is_empty() || request.matches(&conflict.to_string()) {
                    page.push(|| conflict);
                }
            }
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    let enabled = !session.cloud_transition.local_only();
    #[cfg(target_arch = "wasm32")]
    let enabled = true;
    Ok(json!({"enabled":enabled,"library":state.library,"page":page.view(&request,revision,&scope)?}))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cursor_scope_includes_library_generation_query_and_limits() {
        let state = SyncState::new("first".into(), "device".into(), "connection".into()).unwrap();
        let other = SyncState::new("second".into(), "device".into(), "connection".into()).unwrap();
        let mut request: Request = serde_json::from_value(json!({"section":"deleted"})).unwrap();
        let limit = request.validate().unwrap();
        let scope = request.scope(&state, 1, limit).unwrap();
        request.cursor = Some(format!("cp1.1.32.{scope}"));
        assert_eq!(request.offset(1, &scope).unwrap(), 50);
        assert!(request.offset(2, &scope).is_err());
        assert!(request.offset(1, &request.scope(&other, 1, limit).unwrap()).is_err());
        assert!(request.offset(1, &request.scope(&state, 2, limit).unwrap()).is_err());
        assert!(request.offset(1, &request.scope(&state, 1, limit + 1).unwrap()).is_err());
        request.search = "another query".into();
        assert!(request.offset(1, &request.scope(&state, 1, limit).unwrap()).is_err());
    }
}
