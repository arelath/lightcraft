//! Pure upload decisions. Callers own network/file I/O, journal rollback and worker replies.
use super::*;
use lightcraft_catalog::{Catalog, assets::AssetId, replication::SyncState};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Ownership {
    Current,
    LibraryChanged,
    ConnectionChanged,
}
impl Ownership {
    pub(super) fn new(library_current: bool, connection_current: bool) -> Self {
        if !library_current {
            Self::LibraryChanged
        } else if !connection_current {
            Self::ConnectionChanged
        } else {
            Self::Current
        }
    }
    fn phase(self) -> Result<()> {
        match self {
            Self::Current => Ok(()),
            Self::LibraryChanged => Err(error("library changed; staged data preserved")),
            Self::ConnectionChanged => Err(error("storage connection changed; retry using its current settings; staged data preserved")),
        }
    }
    pub(super) fn publication(self) -> Result<()> {
        if self == Self::Current { Ok(()) } else { Err(error("library or connection changed; append publication retained")) }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ContentReadiness {
    Live,
    Retained,
    Unavailable,
}
impl ContentReadiness {
    pub(super) fn of(catalog: &Catalog, intent: &UploadIntent) -> Self {
        if catalog.photo(intent.photo_id).is_some_and(|photo| !photo.deleted && matches!(photo.source, Source::Managed { .. })) {
            Self::Live
        } else if retained_content(catalog, intent) {
            Self::Retained
        } else {
            Self::Unavailable
        }
    }
    pub(super) fn eligible(self) -> bool {
        self != Self::Unavailable
    }
}

pub(super) enum IntentDecision {
    Unchanged,
    Persist(Box<UploadIntent>),
}
pub(super) struct PhaseContext<'a> {
    pub(super) ownership: Ownership,
    pub(super) attempt: u64,
    pub(super) worker: u64,
    pub(super) asset: Option<&'a AssetId>,
    pub(super) paused: bool,
}
pub(super) struct PhaseUpdate {
    pub(super) state: UploadState,
    pub(super) verified: BTreeSet<String>,
    pub(super) error: Option<String>,
    pub(super) remote_invalid: bool,
}
pub(super) fn phase(context: PhaseContext<'_>, catalog: &Catalog, current: Option<&UploadIntent>, update: PhaseUpdate) -> Result<IntentDecision> {
    if context.attempt != context.worker {
        return Err(error("obsolete upload attempt; current acknowledgement retained"));
    }
    context.ownership.phase()?;
    let current = current.ok_or_else(|| error("import was removed; staged original preserved"))?;
    if context.asset != Some(&current.asset_id) {
        return Err(error("import's asset changed; staged data preserved"));
    }
    if !ContentReadiness::of(catalog, current).eligible() {
        return Err(error("photo was deleted or changed; upload stopped and original preserved"));
    }
    let deliberate_failure = update.state == UploadState::Failed && update.remote_invalid;
    if current.state == UploadState::Synchronized && !deliberate_failure {
        return Ok(IntentDecision::Unchanged);
    }
    let paused = context.paused && update.state == UploadState::Failed;
    let mut intent = current.clone();
    intent.state = if paused { UploadState::SavedLocally } else { update.state };
    intent.verified = update.verified;
    intent.error = if paused { None } else { update.error };
    if deliberate_failure {
        intent.finalization = None;
    }
    Ok(if &intent == current { IntentDecision::Unchanged } else { IntentDecision::Persist(Box::new(intent)) })
}

pub(super) fn finalized(
    ownership: Ownership,
    current: Option<&UploadIntent>,
    expected_asset: Option<&AssetId>,
    written: &UploadFinalization,
) -> Result<IntentDecision> {
    if ownership != Ownership::Current {
        return Err(error("library or connection changed; receipt proof remains queued"));
    }
    let current = current.ok_or_else(|| error("upload intent disappeared; receipt proof retained"))?;
    if expected_asset != Some(&current.asset_id) || current.state != UploadState::Synchronized {
        return Err(error("upload acknowledgement changed; receipt proof retained"));
    }
    let Some(queued) = &current.finalization else { return Ok(IntentDecision::Unchanged) };
    written.validate().map_err(error)?;
    if queued.version != written.version
        || queued.protocol != written.protocol
        || queued.library != written.library
        || queued.generation.is_some_and(|generation| written.generation.is_none_or(|written| written < generation))
        || queued.proofs.iter().any(|(id, digest)| written.proofs.get(id) != Some(digest))
    {
        return Err(error("upload receipt proof changed; newer finalization remains queued"));
    }
    let mut intent = current.clone();
    intent.finalization = None;
    intent.error = None;
    Ok(IntentDecision::Persist(Box::new(intent)))
}

pub(super) struct PublicationBinding<'a> {
    pub(super) connection: &'a S3Connection,
    pub(super) state: Option<&'a SyncState>,
}
pub(super) fn publication_binding<'a>(mut bindings: impl Iterator<Item = PublicationBinding<'a>>) -> Result<&'a SyncState> {
    let first = bindings.next().ok_or_else(|| error("no verified media ready for publication"))?;
    let local = first.state.ok_or_else(|| error("enable cloud synchronization before publishing; originals retained"))?;
    if bindings.any(|item| {
        !item.connection.same_destination(first.connection)
            || item.state.is_none_or(|state| state.library != local.library || state.protocol != local.protocol || state.clock != local.clock)
    }) {
        return Err(error("verified upload batch spans different publication bindings; originals retained"));
    }
    Ok(local)
}

pub(super) enum PublicationReadiness {
    Waiting,
    Covered(UploadFinalization),
}

pub(super) fn media_ready(state: UploadState, asset: &Asset, verified: &BTreeSet<String>) -> bool {
    state == UploadState::ObjectsVerified && asset.blobs().all(|blob| verified.contains(&blob.remote.key))
}
pub(super) fn publication_readiness(
    local: &SyncState,
    generation: Option<u64>,
    expected: BTreeMap<String, String>,
    every_proof_verified: bool,
) -> Result<PublicationReadiness> {
    if expected.is_empty() || !every_proof_verified {
        return Ok(PublicationReadiness::Waiting);
    }
    let proof = UploadFinalization {
        version: 1,
        protocol: local.protocol,
        library: local.library.clone(),
        generation: if local.protocol.is_head() { generation } else { None },
        proofs: expected,
    };
    proof.validate().map_err(error)?;
    Ok(PublicationReadiness::Covered(proof))
}

#[cfg(test)]
mod tests;
