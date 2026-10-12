# Cloud storage milestones and tasks

Based on [docs/cloud-storage.md](docs/cloud-storage.md). Checkboxes reflect verified work; milestone acceptance remains separate. Evidence: [docs/cloud-storage-implementation.md](docs/cloud-storage-implementation.md). Complete the four core milestones in order. Browser support is an additional adapter, and a managed metadata backend is optional follow-up work.

The source document describes a particular source snapshot. Verify its implementation assumptions before changing code. Task IDs use the `CS` prefix to distinguish this backlog from the existing project plan.

Verified on 2026-10-12: Milestone 1, including managed originals and proxies, catalog migration, integrity and failure recovery, real CLI/MCP acceptance, and native desktop control-channel acceptance with a generated RAW. Deferred metadata/render requests kept the UI responsive during a deliberately slow download, and attempts to overwrite managed cache data were rejected. The local S3-compatible test server verified request signing; live AWS interoperability remains unverified. `cargo +1.95.0 xtask ci` passed all eight stages. Long-lived keys cannot be entered into LightCraft; authentication uses externally managed AWS profiles or environment credentials.

## Milestone 1 — Read cloud-backed originals

**Outcome:** A RAW file stored only in S3 can be opened, edited, and exported through normal LightCraft commands, with verified local caching.

**Dependencies:** None.

### Asset model and storage boundary

- [x] **CS1.1** Trace current source models, loaders, availability checks, export paths, and local catalog persistence; confirm the integration points described in the source document.
- [x] **CS1.2** Define separate asset-resolver, transfer-manager, and catalog-synchronization interfaces behind the engine boundary; keep network and storage-provider details out of the UI and image-processing pipeline.
- [x] **CS1.3** Add managed asset references alongside existing file and demo sources, with distinct photo, asset, and immutable blob identities; preserve virtual copies that share an original.
- [x] **CS1.4** Model originals and renditions with remote connection, object key, and optional version references; keep local paths and device retention state separate from synchronized library data.
- [x] **CS1.5** Add backward-compatible catalog serialization and migration for managed sources; verify existing local libraries continue to open and save.
- [x] **CS1.6** Add SHA-256 identity and integrity verification for cloud blobs while preserving existing duplicate-detection and cache-key hashing semantics; treat ETags as concurrency tokens.
- [x] **CS1.7** Evaluate a pure-Rust native provider such as `object_store` for dependency, licensing, platform, and conditional-operation compatibility; implement S3 access behind LightCraft-owned interfaces with a local or in-memory test provider.
- [x] **CS1.8** Add storage connection configuration and authentication using AWS profiles or temporary credentials; store supported user-supplied long-lived credentials in the OS credential store and restrict access to the configured bucket or library prefix.

### Resolver and verified cache

- [x] **CS1.9** Implement source-quality requirements equivalent to `PreviewAllowed` and `OriginalRequired`, returning an asset lease that pins verified local content for the caller's lifetime.
- [x] **CS1.10** Implement download-to-temporary-file, SHA-256 verification, and atomic cache publication; reject incomplete or corrupt downloads as usable sources.
- [x] **CS1.11** Schedule downloads outside the UI thread and resume pending operations when their required assets become available; avoid network waits inside synchronous loader callbacks.
- [x] **CS1.12** Route file loading, preview loading, denoise pair loading, and original-byte access such as `Media::file_bytes` through the resolver.
- [x] **CS1.13** Distinguish uncached remote originals, temporary connectivity failures, and confirmed missing sources in availability state.
- [x] **CS1.14** Make full-quality export require an original; expose preview-size export explicitly and prevent silent smart-preview substitution.
- [x] **CS1.15** Split library capabilities currently inferred from `Library::on_disk` so cloud libraries retain local catalog, cache, and smart-preview support.

### Acceptance checks

- [x] **CS1.A1** Prove that a cloud-only RAW can open, accept a develop adjustment, save locally, and export at original quality through the shared engine and normal desktop, CLI, and MCP entry points.
- [x] **CS1.A2** Verify reopening uses verified cached bytes without downloading the original again.
- [x] **CS1.A3** Test interrupted and corrupt downloads, invalid remote references, authorization failures, and disk-write failures; retain actionable errors and never publish partial cache entries.
- [x] **CS1.A4** Run `cargo xtask ci`, exercise the desktop flow through its control channel, and inspect a screenshot and performance data before marking the milestone complete.

## Milestone 2 — Make cloud storage feel local

**Outcome:** Browsing and editing remain responsive, available previews work offline, and users control which assets stay on their device.

**Dependencies:** Milestone 1.

Verified device experience (2026-10-12): recipe-matched remote thumbnails, editable proxies and original refresh; four concurrent downloads with active/nearby/visible/background priorities, deduplication, byte progress, cancellation and reconnect; configurable budgets, separate photo/album offline pins and local-original removal. CLI/MCP, native Info/Performance controls and all eight CI stages passed. Native checks covered delayed transfers, cancellation/retry, preview editing and explicit export, and offline reopening/export of a pinned RAW. Durable pending-upload and external-working-copy protection remains unchecked until the safe-import workflow lands.

### Previews and background transfers

- [x] **CS2.1** Define versioned remote keys for smart previews and thumbnails, with thumbnails keyed by source, edits, and rendering parameters; retain the existing thumbnail cache as a separate disposable cache.
- [x] **CS2.2** Adapt `.lcsp` smart-preview loading and discovery to managed assets; load thumbnails and editable proxies before fetching originals where the operation permits.
- [x] **CS2.3** Show an available preview immediately when opening a photo and refresh the rendering when a better source arrives.
- [x] **CS2.4** Deduplicate simultaneous transfers for the same object and prioritize active photos, nearby photos, visible thumbnails, then background work.
- [x] **CS2.5** Add transfer progress, cancellation, retry, and reconnect handling through shared engine commands and consistent desktop, CLI, and MCP behavior.
- [x] **CS2.6** Invalidate source and availability state after download completion and retry previously failed renders even when their render key and catalog revision have not changed.

### Cache retention and offline operation

- [x] **CS2.7** Add a configurable original-cache disk budget and eviction of eligible, safely uploaded, unused content.
- [x] **CS2.8** Add explicit offline pinning for individual photos and albums, with a choice of originals or previews.
- [ ] **CS2.9** Preserve temporary lease pins while decoders, exports, or external editors use cached files; never evict pending uploads or unsynchronized working copies.
- [x] **CS2.10** Add a remove-local-copy command that evicts eligible cached content while preserving the photo and its remote references.
- [x] **CS2.11** Keep cache paths, budgets, pins, progress, selection, and hardware preferences device-local.
- [x] **CS2.12** Define enabled operations and actionable errors for offline states with an original, only a smart preview, or no usable source.

### Acceptance checks

- [x] **CS2.A1** Verify library browsing and preview editing remain responsive during slow downloads; inspect UI performance through the control channel.
- [x] **CS2.A2** Verify pinned originals and smart previews survive restart and remain usable offline; uncached remote photos do not enter the missing-file/relink workflow.
- [x] **CS2.A3** Verify full-quality export waits for the original, reports unavailable originals offline, and only exports a proxy when explicitly requested.
- [x] **CS2.A4** Test request deduplication, cancellation, retry after reconnect, render recovery, cache-budget enforcement, and eviction protection for active leases and offline pins.
- [x] **CS2.A5** Run `cargo xtask ci`, exercise the desktop commands, and visually inspect progress, availability, and offline controls.

## Milestone 3 — Add safe cloud import

**Outcome:** Users import and edit immediately while durable background work uploads their assets safely. This completes useful single-computer cloud storage with a local catalog.

**Dependencies:** Milestones 1–2. Full multi-device mutation synchronization follows in Milestone 4.

Verified staging and uploads (2026-10-12): durable original/proxy/thumbnail preparation, atomic photo/asset/upload intent, recovery without duplicate imports, resumable multipart transfer, SHA-256 readback verification and conditional initial publication. CLI/MCP response-loss, cancellation, credentials, process-crash and local-acknowledgement failure checks passed. Native connection/import dialogs, immediate editing/export, byte progress, failure and retry were visually inspected; retry reused its multipart ID and published once. All eight CI stages passed. Explicit idle multipart cleanup includes deleted-photo receipts, destination binding and confirmed aborts; cancellation and retry preserve durable bytes. Staging/cache migration, path-dependent operations and external-editor ingestion remain open. Initial import publication does not synchronize subsequent edits between devices.

### Durable import and upload pipeline

- [x] **CS3.1** Stage a durable local original before completing a managed import; probe it and generate its thumbnail and smart preview.
- [x] **CS3.2** Persist the imported catalog record and its upload intent together; recover pending work after restart without losing or duplicating the import.
- [x] **CS3.3** Implement a durable upload queue with progress, cancellation, retry, and persisted state; allow editing immediately after the local save.
- [x] **CS3.4** Upload immutable originals and required supporting objects, verify successful storage, publish remote catalog references last, and mark work synchronized only after publication succeeds.
- [x] **CS3.5** Make retries idempotent when storage accepted a request but the client lost the response; reconcile uploaded objects whose catalog publication failed.
- [x] **CS3.6** Implement multipart uploads for large assets and persist upload IDs and completed-part information for resumption.
- [x] **CS3.7** Clean up abandoned multipart uploads with explicit retention and cancellation rules that preserve resumable work.
- [x] **CS3.8** Enforce that unuploaded originals and externally edited working copies awaiting upload are durable local data and cannot be evicted.
- [x] **CS3.9** Expose distinct saved-locally, uploading, failed/retryable, and synchronized states to the user.

### Operations that assume local paths

Logical organization verified on 2026-10-12: mixed local/cloud rename, undo/redo, album-folder membership
and restart preserve managed asset references and original bytes. Native offline rename/organization and
full-quality export passed and its screenshot was inspected. Rename sequence overflow is rejected before
any changes. `target/cloud-organization-ci.log` records all eight CI stages passing.

- [ ] **CS3.10** Audit and adapt original export, DNG conversion, merge, and denoise to acquire required originals through the resolver.
- [x] **CS3.11** Implement logical rename and folder organization for managed assets without moving immutable image objects.
- [ ] **CS3.12** Model XMP sidecars as associated assets and route reads, writes, and uploads through managed storage rather than neighboring-path assumptions.
- [ ] **CS3.13** Materialize a pinned working file for external editing, detect and ingest the returned revision, and keep it durable until uploaded.
- [ ] **CS3.14** Separate remove-local-copy behavior from delete-from-library behavior; define deletion and remote-object retention policies for managed assets.

### Acceptance checks

- [ ] **CS3.A1** Inject crashes at each import stage: staging, preview generation, local save, multipart transfer, verification, remote publication, and local synchronization acknowledgement; verify recovery preserves data and publication ordering.
- [x] **CS3.A2** Test lost upload and publication responses, interrupted multipart uploads, insufficient disk space, revoked credentials, and restart with queued work.
- [x] **CS3.A3** Verify an imported photo is immediately editable, cannot be evicted before safe upload, and only appears remotely after all required objects exist.
- [ ] **CS3.A4** Exercise the adapted path-dependent operations and prove removing a local copy preserves library membership while library deletion follows the explicit policy.
- [ ] **CS3.A5** Run `cargo xtask ci` and inspect the desktop import, upload-status, and external-edit flows through the control channel.

## Milestone 4 — Synchronize libraries between devices

**Outcome:** Multiple devices share originals, edits, ratings, flags, albums, keywords, versions, and asset references, including safe reconciliation after offline work.

**Dependencies:** Milestones 1–3.

### Identities and local synchronization durability

- [ ] **CS4.1** Add stable library and device IDs, globally unique synchronized entity IDs, mutation IDs, base revisions, and a durable remote cursor; migrate existing compact IDs or persist a global-ID mapping.
- [ ] **CS4.2** Define versioned mutation records for all persistent changes, preserving mutation IDs across retries to prevent duplicated imports, history, or album operations.
- [ ] **CS4.3** Persist catalog changes and unacknowledged synchronization intent atomically; retain unsent records through checkpoints, compaction, and local save completion.
- [ ] **CS4.4** Persist remote acknowledgements before discarding pending work, and persist incoming changes together with their received cursor.
- [ ] **CS4.5** Capture command changes, system bookkeeping, undo, and redo; preserve completed slider-edit grouping and avoid echoing remote changes as new local mutations.
- [ ] **CS4.6** Synchronize library state while excluding device-local cache, transfer, pinning, selection, and hardware state.

### Direct-S3 publication protocol

- [ ] **CS4.7** Define and document versioned bucket layouts for originals, previews, thumbnails, immutable change batches, checkpoints, and `catalog/head.json`.
- [ ] **CS4.8** Implement publication by reading the catalog head and ETag, uploading dependencies and a change batch, then updating the head with `If-Match`; create absent immutable objects with `If-None-Match: *` where appropriate.
- [ ] **CS4.9** Handle failed preconditions by fetching concurrent changes, reconciling, and retrying; never resolve a conflict with an unconditional head overwrite.
- [ ] **CS4.10** Poll the small catalog head for changes, download incremental batches, and bootstrap from checkpoints while normal browsing continues to use the local catalog.
- [ ] **CS4.11** Implement mutation deduplication, checkpoint creation, history retention, and recovery when publication or acknowledgement outcomes are uncertain.

### Conflicts, deletion, and undo

- [ ] **CS4.12** Accept concurrent changes to different photos and merge independent metadata fields against their shared base revision.
- [ ] **CS4.13** Represent album membership changes as explicit operations so concurrent additions are preserved rather than overwritten by whole-list replacements.
- [ ] **CS4.14** Preserve conflicting develop recipes as separate versions and provide a user-visible way to inspect and resolve them.
- [ ] **CS4.15** Retain deletion tombstones and recoverable offline edits when deletion conflicts with editing; prevent a returning device from silently recreating deleted photos.
- [ ] **CS4.16** Make undo and redo produce revision-aware changes that respect received remote edits.
- [ ] **CS4.17** Define retention and rebootstrap behavior for devices older than the retained synchronization history, preserving their pending local changes.

### Acceptance checks

- [ ] **CS4.A1** Run two-device scenarios with independent offline imports and edits; verify globally unique identities, eventual convergence, and no duplicate mutations.
- [ ] **CS4.A2** Test concurrent album additions, independent metadata edits, competing develop settings, and delete-versus-edit conflicts; verify the documented preservation policies.
- [ ] **CS4.A3** Test simultaneous head publication, failed conditional writes, lost responses, replayed batches, and restart before acknowledgement persistence.
- [ ] **CS4.A4** Test compaction with unsent changes, atomic incoming-change/cursor persistence, remote-change echo suppression, and revision-aware undo after remote edits.
- [ ] **CS4.A5** Test deletion followed by offline reconnection and rebootstrap after history expiry, including pending local edits.
- [ ] **CS4.A6** Verify existing local libraries migrate safely and remain usable without a configured cloud connection.
- [ ] **CS4.A7** Run `cargo xtask ci` and inspect synchronization, conflict-version, and recovery flows in the desktop app.

## Milestone 5 — Add browser cloud support

**Outcome:** The web app shares higher-level asset and synchronization logic through browser-specific storage and transfer adapters.

**Dependencies:** Milestones 1–3 for cloud storage; Milestone 4 for shared multi-device libraries.

- [ ] **CS5.1** Trace browser storage and asynchronous original-loading hooks and adapt them to managed assets, local caches, and smart previews.
- [ ] **CS5.2** Implement a browser transfer provider using `fetch` and authorized URLs; keep native provider dependencies outside the browser build.
- [ ] **CS5.3** Define browser-safe authentication and URL refresh behavior, including expiration during transfers.
- [ ] **CS5.4** Document required bucket CORS configuration for the browser adapter's requests, authorization, and conditional operations.
- [ ] **CS5.5** Persist browser cache and pending work through existing browser storage mechanisms, respecting quota limits and the no-eviction rule for unsynchronized data.
- [ ] **CS5.6** Test browser browse/edit/export, reload with pending work, smart-preview fallback, offline operation, storage-quota errors, expired URLs, and multi-device synchronization where enabled.
- [ ] **CS5.7** Run WASM and applicable web checks, including `cargo xtask ci`, and visually inspect browser cloud workflows.

## Milestone 6 — Optional managed metadata backend

**Outcome:** A metadata service supports managed accounts, sharing, permissions, or larger device fleets while photo bytes transfer directly to object storage.

**Dependencies:** Milestone 4 and a product decision to support managed service capabilities. Direct user-bucket access remains the initial implementation.

- [ ] **CS6.1** Define requirements for managed accounts, sharing, permissions, and device scale before selecting a backend architecture.
- [ ] **CS6.2** Add a metadata synchronization adapter behind the existing boundary, preserving revision checks, mutation deduplication, conflict handling, and recovery semantics.
- [ ] **CS6.3** Implement authentication and authorization for revision-aware metadata writes and incremental reads since a cursor.
- [ ] **CS6.4** Record metadata changes transactionally and issue narrowly scoped presigned upload/download URLs for direct photo transfers.
- [ ] **CS6.5** Define migration and compatibility behavior between direct-S3 libraries and service-backed libraries.
- [ ] **CS6.6** Test permissions, cursor recovery, URL expiry, concurrent writes, and behavioral equivalence with the direct-S3 synchronization protocol.

## Completion requirements for every implemented milestone

- [x] **CS.Q1** Add focused regression and end-to-end tests for new behavior and failure recovery; keep test media procedural or in the approved gitignored corpus.
- [x] **CS.Q2** Keep storage operations available through registered engine commands and shared desktop, CLI, control-channel, and MCP dispatch where applicable.
- [x] **CS.Q3** Document configuration, credentials, offline/export policies, cache retention, conflicts, and recovery behavior as each capability lands.
- [x] **CS.Q4** Update applicable parity-checklist rows and gap entries with evidence; refresh roadmap and parity summaries, dates, and revision histories when a listed gap actually closes.
- [x] **CS.Q5** Run required quality gates before each implementation commit and check off only tasks whose behavior and acceptance checks have been verified.
