//! Field-aware inverses preserve independent received metadata and memberships.
use crate::{EngineError, Result};
use lightcraft_catalog::{AlbumId, Catalog, Meta, Op, PhotoId};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, Default)]
pub(crate) struct Fields {
    meta: BTreeMap<PhotoId, BTreeSet<&'static str>>,
    members: BTreeMap<AlbumId, BTreeSet<PhotoId>>,
}

macro_rules! metadata_fields {
    ($($field:ident),+ $(,)?) => {
        fn changed_meta(current: &Meta, target: &Meta) -> BTreeSet<&'static str> {
            // Exhaustive construction makes a new metadata field a compile-time obligation.
            let Meta { $($field),+ } = target;
            let mut changed = BTreeSet::new();
            $(if current.$field != *$field { changed.insert(stringify!($field)); })+
            changed
        }
        fn merge_meta(current: &Meta, target: &Meta, fields: &BTreeSet<&'static str>) -> Meta {
            Meta { $($field: if fields.contains(stringify!($field)) {target.$field.to_owned()} else {current.$field.to_owned()}),+ }
        }
    }
}
metadata_fields!(
    camera,
    lens,
    focal_mm,
    aperture,
    shutter,
    iso,
    location,
    city,
    state,
    country,
    gps,
    title,
    caption,
    alt_text,
    extended_description,
    copyright,
    copyright_status,
    usage_terms,
    copyright_url,
    creator,
    keywords,
    regions
);

impl Fields {
    pub fn record(catalog: &Catalog, inverse: &Op) -> Self {
        let mut metas = BTreeMap::new();
        let mut members = BTreeMap::new();
        let mut pending = vec![inverse];
        while let Some(op) = pending.pop() {
            match op {
                Op::Batch { ops } => pending.extend(ops.iter().rev()),
                Op::SetMeta { id, meta } => {
                    metas.insert(*id, meta.as_ref());
                }
                Op::SetAlbumPhotos { id, photos } => {
                    members.insert(*id, photos);
                }
                _ => {}
            }
        }
        Self {
            meta: metas.into_iter().filter_map(|(id, meta)| catalog.photo(id).map(|photo| (id, changed_meta(&photo.meta, meta)))).collect(),
            members: members
                .into_iter()
                .filter_map(|(id, target)| {
                    catalog.album(id).map(|album| {
                        let current: BTreeSet<_> = album.photos.iter().copied().collect();
                        let target: BTreeSet<_> = target.iter().copied().collect();
                        (id, current.symmetric_difference(&target).copied().collect())
                    })
                })
                .collect(),
        }
    }
    pub fn extend(&mut self, other: Self) {
        for (id, fields) in other.meta {
            self.meta.entry(id).or_default().extend(fields);
        }
        for (id, members) in other.members {
            self.members.entry(id).or_default().extend(members);
        }
    }
    pub fn prepare(&self, catalog: &Catalog, inverse: &Op) -> Result<Op> {
        if self.meta.is_empty() && self.members.is_empty() {
            return Ok(inverse.clone());
        }
        self.prepare_step(&mut catalog.clone(), inverse, 0)
    }
    fn prepare_step(&self, current: &mut Catalog, inverse: &Op, depth: usize) -> Result<Op> {
        if depth > 128 {
            return Err(EngineError::Other("undo/redo operation nesting exceeds the supported limit".into()));
        }
        let op = match inverse {
            Op::Batch { ops } => {
                let ops = ops.iter().map(|op| self.prepare_step(current, op, depth + 1)).collect::<Result<Vec<_>>>()?;
                return Ok(Op::Batch { ops });
            }
            Op::SetMeta { id, meta } => {
                if let (Some(fields), Some(photo)) = (self.meta.get(id), current.photo(*id)) {
                    Op::SetMeta { id: *id, meta: Box::new(merge_meta(&photo.meta, meta, fields)) }
                } else {
                    inverse.clone()
                }
            }
            Op::SetAlbumPhotos { id, photos } => {
                if let (Some(fields), Some(album)) = (self.members.get(id), current.album(*id)) {
                    let present: BTreeSet<_> = album.photos.iter().copied().collect();
                    let mut kept = BTreeSet::new();
                    let mut target: Vec<_> =
                        photos.iter().copied().filter(|photo| (fields.contains(photo) || present.contains(photo)) && kept.insert(*photo)).collect();
                    target.extend(album.photos.iter().copied().filter(|photo| !fields.contains(photo) && kept.insert(*photo)));
                    Op::SetAlbumPhotos { id: *id, photos: target }
                } else {
                    inverse.clone()
                }
            }
            _ => inverse.clone(),
        };
        current.apply(op.clone())?;
        Ok(op)
    }
}
