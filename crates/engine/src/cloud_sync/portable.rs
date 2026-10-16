//! Explicit shared schemas. Catalog-only fields cannot enter the wire representation by default.
use super::{Result, encode, error};
use lightcraft_catalog::assets::{Asset, AssetId};
use lightcraft_catalog::{Album, Catalog, Meta, Op, Photo, PhotoId, Source};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::BTreeMap, sync::Arc};

mod filter;
pub(super) use filter::{PortableFilter, has_local_rules};

pub(super) fn decode<T: serde::de::DeserializeOwned>(value: Value) -> Result<T> {
    serde_json::from_value(value).map_err(|e| error(format!("unsupported or malformed cloud metadata: {e}")))
}
fn present<'de, T: Deserialize<'de>, D: serde::Deserializer<'de>>(deserializer: D) -> std::result::Result<Option<T>, D::Error> {
    T::deserialize(deserializer).map(Some)
}
pub(super) fn fields(value: &impl Serialize) -> Result<BTreeMap<String, Value>> {
    let Value::Object(map) = encode(value)? else { return Err(error("portable document is not an object")) };
    Ok(map.into_iter().collect())
}
macro_rules! reference {
    ($name:ident) => {
        #[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
        #[serde(try_from = "String", into = "String")]
        pub(super) struct $name(String);
        impl TryFrom<String> for $name {
            type Error = String;
            fn try_from(id: String) -> std::result::Result<Self, String> {
                if lightcraft_catalog::assets::identifier(&id) { Ok(Self(id)) } else { Err("invalid global reference identity".into()) }
            }
        }
        impl From<$name> for String {
            fn from(id: $name) -> Self {
                id.0
            }
        }
    };
}
reference!(PhotoRef);
reference!(AlbumRef);
impl PhotoRef {
    pub(super) fn from_local(catalog: &Catalog, ids: &Catalog, connection: &str, id: PhotoId) -> Option<Self> {
        super::mapped_photo(catalog, ids, connection, id.0).map(Self)
    }
    pub(super) fn retained(ids: &Catalog, id: PhotoId) -> Result<Self> {
        Ok(Self(super::global(ids, "photo", id.0)?))
    }
    pub(super) fn resolve(&self, catalog: &Catalog) -> Option<PhotoId> {
        catalog.cloud_local_identity(&self.0)?.strip_prefix("photo:")?.parse().ok().map(PhotoId)
    }
    pub(super) fn id(&self) -> &str {
        &self.0
    }
}
impl AlbumRef {
    pub(super) fn id(&self) -> &str {
        &self.0
    }
    pub(super) fn from_local(ids: &Catalog, id: lightcraft_catalog::AlbumId) -> Result<Self> {
        Ok(Self(super::global(ids, "album", id.0)?))
    }
    pub(super) fn resolve(&self, catalog: &mut Catalog, ops: &mut Vec<Op>) -> Result<lightcraft_catalog::AlbumId> {
        super::materialize::album_reference(catalog, ops, &self.0)
    }
}

/// Optional legacy content fields keep the previous seed fallback; new writers emit them explicitly.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Content {
    #[serde(rename = "assetId")]
    pub(super) asset_id: AssetId,
    pub(super) asset: Asset,
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "present")]
    format: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "present")]
    kind: Option<lightcraft_catalog::MediaKind>,
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "present")]
    width: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "present")]
    height: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "present")]
    file_size: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    content_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    preview_only: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    embedded_lens: Option<lightcraft_develop::EmbeddedLens>,
    #[serde(default)]
    as_shot_wb: Option<(f64, f64)>,
    #[serde(default)]
    duration: Option<f64>,
}
impl Content {
    pub(super) fn from_local(photo: &Photo, asset_id: &AssetId, asset: &Asset) -> Result<Self> {
        Ok(Self {
            asset_id: asset_id.clone(),
            asset: asset.rebound("library").map_err(error)?,
            format: Some(photo.format.clone()),
            kind: Some(photo.kind),
            width: Some(photo.width),
            height: Some(photo.height),
            file_size: Some(photo.file_size),
            content_hash: photo.content_hash.clone(),
            preview_only: photo.preview_only.clone(),
            embedded_lens: photo.embedded_lens,
            as_shot_wb: photo.as_shot_wb,
            duration: photo.duration,
        })
    }
    pub(super) fn apply(&self, photo: &mut Photo) {
        photo.source = Source::Managed { asset_id: self.asset_id.clone() };
        photo.local = false;
        photo.local_baseline = None;
        if let Some(value) = &self.format {
            photo.format = value.clone();
        }
        if let Some(value) = &self.kind {
            photo.kind = *value;
        }
        if let Some(value) = &self.width {
            photo.width = *value;
        }
        if let Some(value) = &self.height {
            photo.height = *value;
        }
        if let Some(value) = &self.file_size {
            photo.file_size = *value;
        }
        photo.content_hash = self.content_hash.clone();
        photo.preview_only = self.preview_only.clone();
        photo.embedded_lens = self.embedded_lens;
        photo.as_shot_wb = self.as_shot_wb;
        photo.duration = self.duration;
    }
}

#[derive(Clone, Debug, Serialize)]
struct PortableMeta {
    camera: String,
    lens: String,
    focal_mm: Option<f32>,
    aperture: Option<f32>,
    shutter: String,
    iso: Option<u32>,
    location: String,
    city: String,
    state: String,
    country: String,
    gps: Option<(f64, f64)>,
    title: String,
    caption: String,
    alt_text: String,
    extended_description: String,
    copyright: String,
    copyright_status: lightcraft_catalog::CopyrightStatus,
    usage_terms: String,
    copyright_url: String,
    creator: String,
    keywords: Vec<String>,
    regions: Vec<lightcraft_meta::Region>,
}
impl From<&Meta> for PortableMeta {
    fn from(meta: &Meta) -> Self {
        Self {
            camera: meta.camera.clone(),
            lens: meta.lens.clone(),
            focal_mm: meta.focal_mm,
            aperture: meta.aperture,
            shutter: meta.shutter.clone(),
            iso: meta.iso,
            location: meta.location.clone(),
            city: meta.city.clone(),
            state: meta.state.clone(),
            country: meta.country.clone(),
            gps: meta.gps,
            title: meta.title.clone(),
            caption: meta.caption.clone(),
            alt_text: meta.alt_text.clone(),
            extended_description: meta.extended_description.clone(),
            copyright: meta.copyright.clone(),
            copyright_status: meta.copyright_status,
            usage_terms: meta.usage_terms.clone(),
            copyright_url: meta.copyright_url.clone(),
            creator: meta.creator.clone(),
            keywords: meta.keywords.clone(),
            regions: meta.regions.clone(),
        }
    }
}
fn apply_meta(meta: &mut Meta, field: &str, value: Value) -> Result<()> {
    match field {
        "camera" => meta.camera = decode(value)?,
        "lens" => meta.lens = decode(value)?,
        "focal_mm" => meta.focal_mm = decode(value)?,
        "aperture" => meta.aperture = decode(value)?,
        "shutter" => meta.shutter = decode(value)?,
        "iso" => meta.iso = decode(value)?,
        "location" => meta.location = decode(value)?,
        "city" => meta.city = decode(value)?,
        "state" => meta.state = decode(value)?,
        "country" => meta.country = decode(value)?,
        "gps" => meta.gps = decode(value)?,
        "title" => meta.title = decode(value)?,
        "caption" => meta.caption = decode(value)?,
        "alt_text" => meta.alt_text = decode(value)?,
        "extended_description" => meta.extended_description = decode(value)?,
        "copyright" => meta.copyright = decode(value)?,
        "copyright_status" => meta.copyright_status = decode(value)?,
        "usage_terms" => meta.usage_terms = decode(value)?,
        "copyright_url" => meta.copyright_url = decode(value)?,
        "creator" => meta.creator = decode(value)?,
        "keywords" => meta.keywords = decode(value)?,
        "regions" => meta.regions = decode(value)?,
        _ => return Err(error("unsupported cloud metadata field")),
    }
    Ok(())
}

#[derive(Clone, Debug, Serialize)]
pub(super) struct PortablePhoto {
    content: Content,
    file_name: String,
    captured: Option<String>,
    imported: String,
    rating: u8,
    flag: lightcraft_catalog::Flag,
    label: Option<lightcraft_catalog::ColorLabel>,
    develop: Arc<lightcraft_develop::DevelopSettings>,
    edited: Option<String>,
    versions: Vec<lightcraft_catalog::Version>,
    history: Vec<lightcraft_catalog::HistoryStep>,
    deleted: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    copy_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    import_look: Option<Arc<lightcraft_develop::DevelopSettings>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    analysis: Option<lightcraft_catalog::Analysis>,
    #[serde(skip_serializing_if = "Option::is_none")]
    copy_of: Option<Option<PhotoRef>>,
    #[serde(skip)]
    meta: PortableMeta,
}
impl PortablePhoto {
    pub(super) fn from_local(photo: &Photo, asset_id: &AssetId, asset: &Asset, catalog: &Catalog, ids: &Catalog, connection: &str) -> Result<Self> {
        Ok(Self {
            content: Content::from_local(photo, asset_id, asset)?,
            file_name: photo.file_name.clone(),
            captured: photo.captured.clone(),
            imported: photo.imported.clone(),
            rating: photo.rating,
            flag: photo.flag,
            label: photo.label,
            develop: photo.develop.clone(),
            edited: photo.edited.clone(),
            versions: photo.versions.clone(),
            history: photo.history.clone(),
            deleted: photo.deleted,
            copy_name: photo.copy_name.clone(),
            import_look: photo.import_look.clone(),
            analysis: photo.analysis,
            copy_of: photo.copy_of.map(|id| PhotoRef::from_local(catalog, ids, connection, id)),
            meta: PortableMeta::from(&photo.meta),
        })
    }
    pub(super) fn fields(&self) -> Result<BTreeMap<String, Value>> {
        let mut result = fields(self)?;
        for (key, value) in fields(&self.meta)? {
            result.insert(format!("meta.{key}"), value);
        }
        Ok(result)
    }
}
/// Apply only named logical shared fields to the local seed. Never serialize a catalog Photo.
pub(super) fn apply_photo(photo: &mut Photo, values: &BTreeMap<String, Value>, catalog: &Catalog) -> Result<()> {
    for (field, value) in values {
        if let Some(field) = field.strip_prefix("meta.") {
            apply_meta(&mut photo.meta, field, value.clone())?;
            continue;
        }
        match field.as_str() {
            "content" => {}
            "copy_of" => photo.copy_of = decode::<Option<PhotoRef>>(value.clone())?.and_then(|reference| reference.resolve(catalog)),
            "file_name" => photo.file_name = decode(value.clone())?,
            "captured" => photo.captured = decode(value.clone())?,
            "imported" => photo.imported = decode(value.clone())?,
            "rating" => photo.rating = decode(value.clone())?,
            "flag" => photo.flag = decode(value.clone())?,
            "label" => photo.label = decode(value.clone())?,
            "develop" => photo.develop = decode(value.clone())?,
            "edited" => photo.edited = decode(value.clone())?,
            "versions" => photo.versions = decode(value.clone())?,
            "history" => photo.history = decode(value.clone())?,
            "deleted" => photo.deleted = decode(value.clone())?,
            "copy_name" => photo.copy_name = decode(value.clone())?,
            "import_look" => photo.import_look = decode(value.clone())?,
            "analysis" => photo.analysis = decode(value.clone())?,
            _ => return Err(error("unsupported cloud photo field")),
        }
    }
    Ok(())
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PortableAlbum {
    name: String,
    #[serde(default)]
    parent: Option<AlbumRef>,
    #[serde(default)]
    folder: bool,
    #[serde(default)]
    cover: Option<PhotoRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    smart: Option<PortableFilter>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    quick: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    order: Option<u32>,
    #[serde(default)]
    deleted: bool,
}
impl PortableAlbum {
    pub(super) fn from_local(album: &Album, catalog: &Catalog, ids: &Catalog, connection: &str) -> Result<Self> {
        Ok(Self {
            name: album.name.clone(),
            parent: album.parent.map(|id| AlbumRef::from_local(ids, id)).transpose()?,
            folder: album.folder,
            cover: album.cover.and_then(|id| PhotoRef::from_local(catalog, ids, connection, id)),
            smart: album.smart.as_ref().map(|filter| PortableFilter::from_local(filter, catalog, ids, connection)).transpose()?,
            quick: album.quick,
            order: album.order,
            deleted: false,
        })
    }
}

#[derive(Clone, Debug, Serialize)]
pub(super) struct PortableStack {
    pub(super) photos: Vec<PhotoRef>,
    pub(super) collapsed: bool,
    pub(super) deleted: bool,
}

#[cfg(test)]
mod tests;
