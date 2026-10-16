//! Schema-defined reference slots; no recursive search through arbitrary JSON properties.
use super::super::{Result, error};
use super::{AlbumRef, PhotoRef, decode};
use lightcraft_catalog::{Catalog, Filter, Op, Rule, RuleSet};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub(in crate::cloud_sync) struct PortableFilter {
    text: String,
    rating: u8,
    rating_op: lightcraft_catalog::RatingOp,
    flag: Option<lightcraft_catalog::Flag>,
    label: Option<lightcraft_catalog::ColorLabel>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    labels: Vec<lightcraft_catalog::ColorLabel>,
    kind: Option<lightcraft_catalog::MediaKind>,
    edited: Option<bool>,
    deleted: bool,
    date: Option<String>,
    keyword: Option<String>,
    person: Option<String>,
    camera: Option<String>,
    lens: Option<String>,
    date_from: Option<String>,
    date_to: Option<String>,
    imported: Option<String>,
    imported_from: Option<String>,
    merged: Option<String>,
    subfolders: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    only: Vec<PhotoRef>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    only_restricted: bool,
    album: Option<AlbumRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    rule_set: Option<PortableRules>,
}
impl PortableFilter {
    pub(in crate::cloud_sync) fn from_local(filter: &Filter, catalog: &Catalog, ids: &Catalog, connection: &str) -> Result<Self> {
        if filter.folder.is_some() || filter.library_folder.is_some() || filter.sync_status.is_some() {
            return Err(error("a device-local smart filter cannot be published"));
        }
        let only = filter
            .only
            .iter()
            .map(|id| {
                if !super::super::shared_selection(catalog, connection, *id) {
                    return Err(error("a device-local smart selection cannot be published"));
                }
                PhotoRef::retained(ids, *id)
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(Self {
            text: filter.text.clone(),
            rating: filter.rating,
            rating_op: filter.rating_op,
            flag: filter.flag,
            label: filter.label,
            labels: filter.labels.clone(),
            kind: filter.kind,
            edited: filter.edited,
            deleted: filter.deleted,
            date: filter.date.clone(),
            keyword: filter.keyword.clone(),
            person: filter.person.clone(),
            camera: filter.camera.clone(),
            lens: filter.lens.clone(),
            date_from: filter.date_from.clone(),
            date_to: filter.date_to.clone(),
            imported: filter.imported.clone(),
            imported_from: filter.imported_from.clone(),
            merged: filter.merged.clone(),
            subfolders: filter.subfolders,
            only,
            only_restricted: filter.only_restricted || !filter.only.is_empty(),
            album: filter.album.map(|id| AlbumRef::from_local(ids, id)).transpose()?,
            rule_set: filter.rule_set.as_ref().map(|rules| PortableRules::from_local(rules, ids, 0)).transpose()?,
        })
    }
    pub(in crate::cloud_sync) fn to_local(&self, catalog: &mut Catalog, ops: &mut Vec<Op>) -> Result<Filter> {
        let only: Vec<_> = self.only.iter().filter_map(|reference| reference.resolve(catalog)).collect();
        let only_restricted = if self.only.is_empty() { self.only_restricted } else { only.is_empty() };
        Ok(Filter {
            text: self.text.clone(),
            rating: self.rating,
            rating_op: self.rating_op,
            flag: self.flag,
            label: self.label,
            labels: self.labels.clone(),
            kind: self.kind,
            edited: self.edited,
            deleted: self.deleted,
            date: self.date.clone(),
            keyword: self.keyword.clone(),
            person: self.person.clone(),
            camera: self.camera.clone(),
            lens: self.lens.clone(),
            date_from: self.date_from.clone(),
            date_to: self.date_to.clone(),
            imported: self.imported.clone(),
            imported_from: self.imported_from.clone(),
            merged: self.merged.clone(),
            subfolders: self.subfolders,
            only,
            only_restricted,
            album: self.album.as_ref().map(|reference| reference.resolve(catalog, ops)).transpose()?,
            rule_set: self.rule_set.as_ref().map(|rules| rules.to_local(catalog, ops, 0)).transpose()?,
            folder: None,
            library_folder: None,
            sync_status: None,
        })
    }
}
#[derive(Clone, Debug, Serialize)]
struct PortableRules {
    #[serde(rename = "match")]
    mode: lightcraft_catalog::Match,
    rules: Vec<PortableRule>,
}
#[derive(Clone, Debug)]
enum PortableRule {
    Group(PortableRules),
    Album { op: String, target: AlbumRef },
    Field { field: String, op: String, value: Value },
}
impl Serialize for PortableRule {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut map = serializer.serialize_map(None)?;
        match self {
            Self::Group(group) => map.serialize_entry("group", group)?,
            Self::Album { op, target } => {
                map.serialize_entry("field", "album")?;
                map.serialize_entry("op", op)?;
                map.serialize_entry("value", target)?;
            }
            Self::Field { field, op, value } => {
                map.serialize_entry("field", field)?;
                map.serialize_entry("op", op)?;
                map.serialize_entry("value", value)?;
            }
        }
        map.end()
    }
}
impl<'de> Deserialize<'de> for PortableRules {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        Self::parse(Value::deserialize(deserializer)?, 0).map_err(serde::de::Error::custom)
    }
}
fn depth_limit(depth: usize) -> Result<()> {
    if depth > 64 { Err(error("cloud smart filter exceeds 64 nesting levels")) } else { Ok(()) }
}
impl PortableRules {
    fn from_local(rules: &RuleSet, ids: &Catalog, depth: usize) -> Result<Self> {
        depth_limit(depth)?;
        let mode = rules.mode;
        let rules = rules
            .rules
            .iter()
            .map(|rule| {
                Ok(match rule {
                    Rule::Group { group } => PortableRule::Group(Self::from_local(group, ids, depth + 1)?),
                    Rule::Field { field, op, value } if field == "album" => PortableRule::Album {
                        op: op.clone(),
                        target: AlbumRef::from_local(
                            ids,
                            lightcraft_catalog::rules::album_rule_id(value).ok_or_else(|| error("invalid local album rule reference"))?,
                        )?,
                    },
                    Rule::Field { field, .. } if field == "filePath" => return Err(error("a device-local path rule cannot be published")),
                    Rule::Field { field, op, value } => PortableRule::Field { field: field.clone(), op: op.clone(), value: value.clone() },
                })
            })
            .collect::<Result<_>>()?;
        Ok(Self { mode, rules })
    }
    fn parse(value: Value, depth: usize) -> Result<Self> {
        depth_limit(depth)?;
        let map = value.as_object().ok_or_else(|| error("invalid cloud rule group"))?;
        if map.keys().any(|key| !matches!(key.as_str(), "match" | "rules")) {
            return Err(error("unsupported cloud rule group field"));
        }
        let mode = map.get("match").cloned().map(decode).transpose()?.unwrap_or_default();
        let mut rules = Vec::new();
        if let Some(values) = map.get("rules") {
            for value in values.as_array().ok_or_else(|| error("invalid cloud rules list"))? {
                let map = value.as_object().ok_or_else(|| error("invalid cloud rule"))?;
                if let Some(group) = map.get("group") {
                    if map.len() != 1 {
                        return Err(error("unsupported cloud group rule field"));
                    }
                    rules.push(PortableRule::Group(Self::parse(group.clone(), depth + 1)?));
                    continue;
                }
                if map.keys().any(|key| !matches!(key.as_str(), "field" | "op" | "value")) {
                    return Err(error("unsupported cloud rule field"));
                }
                let field: String = decode(map.get("field").cloned().ok_or_else(|| error("cloud rule has no field"))?)?;
                let op: String = decode(map.get("op").cloned().ok_or_else(|| error("cloud rule has no operator"))?)?;
                let value = map.get("value").cloned().unwrap_or(Value::Null);
                rules.push(match field.as_str() {
                    "album" => PortableRule::Album { op, target: decode(value)? },
                    "filePath" => return Err(error("cloud rule contains a device-local path")),
                    _ => PortableRule::Field { field, op, value },
                });
            }
        }
        Ok(Self { mode, rules })
    }
    fn to_local(&self, catalog: &mut Catalog, ops: &mut Vec<Op>, depth: usize) -> Result<RuleSet> {
        depth_limit(depth)?;
        let rules = self
            .rules
            .iter()
            .map(|rule| {
                Ok(match rule {
                    PortableRule::Group(group) => Rule::Group { group: group.to_local(catalog, ops, depth + 1)? },
                    PortableRule::Album { op, target } => {
                        Rule::Field { field: "album".into(), op: op.clone(), value: serde_json::json!(target.resolve(catalog, ops)?) }
                    }
                    PortableRule::Field { field, op, value } => Rule::Field { field: field.clone(), op: op.clone(), value: value.clone() },
                })
            })
            .collect::<Result<_>>()?;
        Ok(RuleSet { mode: self.mode, rules })
    }
}

/// A path rule cannot acquire a portable meaning; preserve the whole smart album locally.
pub(in crate::cloud_sync) fn has_local_rules(rules: Option<&RuleSet>) -> bool {
    let mut pending: Vec<_> = rules.into_iter().map(|rules| (rules, 0usize)).collect();
    while let Some((rules, depth)) = pending.pop() {
        if depth > 64 {
            return true;
        }
        for rule in &rules.rules {
            match rule {
                Rule::Group { group } => pending.push((group, depth + 1)),
                Rule::Field { field, .. } if field == "filePath" => return true,
                _ => {}
            }
        }
    }
    false
}
