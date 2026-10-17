# Cloud storage milestones and tasks

Based on [docs/cloud-storage.md](docs/cloud-storage.md). Checkboxes reflect verified work; milestone acceptance remains separate. Current behavior: [docs/cloud-storage-implementation.md](docs/cloud-storage-implementation.md). Dated evidence: [docs/cloud-acceptance-history.md](docs/cloud-acceptance-history.md). The five scoped milestones cover native cloud storage/synchronization and the browser adapter; Milestone 6 is deferred.

The supplied reliability review is tracked separately in
[docs/cloud-reliability-tasks.md](docs/cloud-reliability-tasks.md), in execution order.
Its unchecked items remain open even when earlier milestone acceptance passed.

The source document describes a particular source snapshot. Verify its implementation assumptions before changing code. Task IDs use the `CS` prefix to distinguish this backlog from the existing project plan.

Scope confirmed by the user: complete direct S3 storage/synchronization and browser support (Milestones 1–5).
Milestone 6 is deferred. Live Backblaze B2 read/edit/export and original reacquisition passed on 2026-10-12.
B2 rejected `If-None-Match` and `If-Match` with HTTP 501, so conditional catalog publication remains
unavailable there; the failed upload retained its staged original. Loopback fixtures cover publication
and synchronization failure scenarios. Evidence: `target/cloud-acceptance/live-b2*-results.json`.
Explicit append-only B2 publication and synchronization passed native and browser live acceptance
on 2026-10-14; this protocol avoids the unavailable conditional writes (see the B2 tasks below).

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

Verified device experience (2026-10-12): recipe-matched remote thumbnails, editable proxies and original refresh; four concurrent downloads with active/nearby/visible/background priorities, deduplication, byte progress, cancellation and reconnect; configurable budgets, separate photo/album offline pins and local-original removal. CLI/MCP, native Info/Performance controls and all eight CI stages passed. Native checks covered delayed transfers, cancellation/retry, preview editing and explicit export, and offline reopening/export of a pinned RAW. Durable pending-upload and external-working-copy protection passed with the managed revision workflow on 2026-10-13.

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
- [x] **CS2.9** Preserve temporary lease pins while decoders, exports, or external editors use cached files; never evict pending uploads or unsynchronized working copies.
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

Verified staging and uploads (2026-10-12): durable original/proxy/thumbnail preparation, atomic photo/asset/upload intent, recovery without duplicate imports, resumable multipart transfer, SHA-256 readback verification and conditional initial publication. CLI/MCP response-loss, cancellation, credentials, process-crash and local-acknowledgement failure checks passed. Native connection/import dialogs, immediate editing/export, byte progress, failure and retry were visually inspected; retry reused its multipart ID and published once. All eight CI stages passed. Explicit idle multipart cleanup includes deleted-photo receipts, destination binding and confirmed aborts; cancellation and retry preserve durable bytes. Verified staging/cache migration and explicit local-copy/library deletion policies preserve pending data and remote objects. Managed DNG conversion, associated XMP and external-editor revision ingestion passed on 2026-10-13; merge and denoise acquire originals through the resolver. Initial import publication does not synchronize subsequent edits between devices.

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

- [x] **CS3.10** Audit and adapt original export, DNG conversion, merge, and denoise to acquire required originals through the resolver.
- [x] **CS3.11** Implement logical rename and folder organization for managed assets without moving immutable image objects.
- [x] **CS3.12** Model XMP sidecars as associated assets and route reads, writes, and uploads through managed storage rather than neighboring-path assumptions.
- [x] **CS3.13** Materialize a pinned working file for external editing, detect and ingest the returned revision, and keep it durable until uploaded.
- [x] **CS3.14** Separate remove-local-copy behavior from delete-from-library behavior; define deletion and remote-object retention policies for managed assets.

CS3.14 verified on 2026-10-12: six staging-retention regressions, eleven real CLI/MCP upload checks,
and one desktop completion-feedback regression passed. Native Info-button refusal/removal, proxy export,
full-original reacquisition, permanent deletion and Performance staging-error display were inspected.
Remote objects and working data were preserved. `target/cloud-managed-retention-ci-final.log` records
all eight CI stages passing. Legacy receipts require explicit re-verification before migration.

Managed revision acceptance (2026-10-13): all eight CI stages passed in `target/cloud-managed-revisions-ci-final.log`. Fifteen focused preparation/recovery tests and real CLI/MCP revision tests cover partial rendition recovery, source changes, protected hard-link aliases and immutable upload descriptions. Native control-channel checks converted a procedural RAW to DNG, saved/read associated XMP, launched an actual test editor, ingested its returned 16×12 TIFF under the same derived photo ID, exported at full dimensions and removed eligible cache data while retaining the working file. All 54 fixture requests passed independent signing checks. Screenshots `native-revisions-dng.png` and `native-revisions-external.png` were inspected; results are in `target/cloud-acceptance/native-revisions-results.json`. Maximum inspection latency was 59 ms on this tiny fixture.

### Acceptance checks

- [x] **CS3.A1** Inject crashes at each import stage: staging, preview generation, local save, multipart transfer, verification, remote publication, and local synchronization acknowledgement; verify recovery preserves data and publication ordering.
- [x] **CS3.A2** Test lost upload and publication responses, interrupted multipart uploads, insufficient disk space, revoked credentials, and restart with queued work.
- [x] **CS3.A3** Verify an imported photo is immediately editable, cannot be evicted before safe upload, and only appears remotely after all required objects exist.
- [x] **CS3.A4** Exercise the adapted path-dependent operations and prove removing a local copy preserves library membership while library deletion follows the explicit policy.
- [x] **CS3.A5** Run `cargo xtask ci` and inspect the desktop import, upload-status, and external-edit flows through the control channel.

## Milestone 4 — Synchronize libraries between devices

**Outcome:** Multiple devices share originals, edits, ratings, flags, albums, keywords, versions, and asset references, including safe reconciliation after offline work.

**Dependencies:** Milestones 1–3.

Synchronization core verified (2026-10-13): catalog format 11 atomically retains global identities, field mutations, pending work and incoming cursors. Eleven shared-engine scenarios, two owner/persistence regressions, five reconciliation-core checks and four real CLI protocol scenarios cover offline imports, independent fields and album additions, competing recipes, delete-versus-edit recovery, revision-aware undo, grouped sliders, compaction, failed saves, conditional races/response loss, incremental tails and expired-history rebootstrap. The protocol and retention policy are in [docs/cloud-sync-protocol.md](docs/cloud-sync-protocol.md). Desktop acceptance also passed, as recorded below.

### Identities and local synchronization durability

- [x] **CS4.1** Add stable library and device IDs, globally unique synchronized entity IDs, mutation IDs, base revisions, and a durable remote cursor; migrate existing compact IDs or persist a global-ID mapping.
- [x] **CS4.2** Define versioned mutation records for all persistent changes, preserving mutation IDs across retries to prevent duplicated imports, history, or album operations.
- [x] **CS4.3** Persist catalog changes and unacknowledged synchronization intent atomically; retain unsent records through checkpoints, compaction, and local save completion.
- [x] **CS4.4** Persist remote acknowledgements before discarding pending work, and persist incoming changes together with their received cursor.
- [x] **CS4.5** Capture command changes, system bookkeeping, undo, and redo; preserve completed slider-edit grouping and avoid echoing remote changes as new local mutations.
- [x] **CS4.6** Synchronize library state while excluding device-local cache, transfer, pinning, selection, and hardware state.

### Direct-S3 publication protocol

- [x] **CS4.7** Define and document versioned bucket layouts for originals, previews, thumbnails, immutable change batches, checkpoints, and `catalog/head.json`.
- [x] **CS4.8** Implement publication by reading the catalog head and ETag, uploading dependencies and a change batch, then updating the head with `If-Match`; create absent immutable objects with `If-None-Match: *` where appropriate.
- [x] **CS4.9** Handle failed preconditions by fetching concurrent changes, reconciling, and retrying; never resolve a conflict with an unconditional head overwrite.
- [x] **CS4.10** Poll the small catalog head for changes, download incremental batches, and bootstrap from checkpoints while normal browsing continues to use the local catalog.
- [x] **CS4.11** Implement mutation deduplication, checkpoint creation, history retention, and recovery when publication or acknowledgement outcomes are uncertain.

### Conflicts, deletion, and undo

- [x] **CS4.12** Accept concurrent changes to different photos and merge independent metadata fields against their shared base revision.
- [x] **CS4.13** Represent album membership changes as explicit operations so concurrent additions are preserved rather than overwritten by whole-list replacements.
- [x] **CS4.14** Preserve conflicting develop recipes as separate versions and provide a user-visible way to inspect and resolve them.
- [x] **CS4.15** Retain deletion tombstones and recoverable offline edits when deletion conflicts with editing; prevent a returning device from silently recreating deleted photos.
- [x] **CS4.16** Make undo and redo produce revision-aware changes that respect received remote edits.
- [x] **CS4.17** Define retention and rebootstrap behavior for devices older than the retained synchronization history, preserving their pending local changes.

### Acceptance checks

- [x] **CS4.A1** Run two-device scenarios with independent offline imports and edits; verify globally unique identities, eventual convergence, and no duplicate mutations.
- [x] **CS4.A2** Test concurrent album additions, independent metadata edits, competing develop settings, and delete-versus-edit conflicts; verify the documented preservation policies.
- [x] **CS4.A3** Test simultaneous head publication, failed conditional writes, lost responses, replayed batches, and restart before acknowledgement persistence.
- [x] **CS4.A4** Test compaction with unsent changes, atomic incoming-change/cursor persistence, remote-change echo suppression, and revision-aware undo after remote edits.
- [x] **CS4.A5** Test deletion followed by offline reconnection and rebootstrap after history expiry, including pending local edits.
- [x] **CS4.A6** Verify existing local libraries migrate safely and remain usable without a configured cloud connection.
- [x] **CS4.A7** Run `cargo xtask ci` and inspect synchronization, conflict-version, and recovery flows in the desktop app.

Native synchronization acceptance (2026-10-13) passed through control port 18877 with a signed loopback S3 fixture. Two catalogs converged after independent metadata/develop edits; the Info conflict controls opened Versions and explicitly resolved a recipe while preserving its alternative. Permanent deletion followed by an offline peer edit did not recreate the photo; clicking Recover restored the retained five-star edit, then both catalogs synchronized. Cancellation retained pending work, rejected credentials produced actionable Info text, and clicking Sync now completed retry. Setup, progress, failure, conflict, versions and recovery screenshots were inspected. All 140 scenario requests passed independent signing checks; every write was conditional. Results are in `target/cloud-acceptance/native-sync-results.json`. The isolated delayed-head check recorded 61 inspection samples with a maximum of 52 ms; the complete feedback flow recorded a 493 ms outlier around screenshot/control interaction. Final UI update time was 8.87 ms with empty render queues. These tiny procedural checks do not establish large-library performance or live AWS compatibility. All eight CI stages passed in `target/cloud-sync-ci-final.log`.

## Milestone 5 — Add browser cloud support

**Outcome:** The web app shares higher-level asset and synchronization logic through browser-specific storage and transfer adapters.

**Dependencies:** Milestones 1–3 for cloud storage; Milestone 4 for shared multi-device libraries.

- [x] **CS5.1** Trace browser storage and asynchronous original-loading hooks and adapt them to managed assets, local caches, and smart previews.
- [x] **CS5.2** Implement a browser transfer provider using `fetch` and authorized URLs; keep native provider dependencies outside the browser build.
- [x] **CS5.3** Define browser-safe authentication and URL refresh behavior, including expiration during transfers.
- [x] **CS5.4** Document required bucket CORS configuration for the browser adapter's requests, authorization, and conditional operations.
- [x] **CS5.5** Persist browser cache and pending work through existing browser storage mechanisms, respecting quota limits and the no-eviction rule for unsynchronized data.
- [x] **CS5.6** Test browser browse/edit/export, reload with pending work, smart-preview fallback, offline operation, storage-quota errors, expired URLs, and multi-device synchronization where enabled.
- [x] **CS5.7** Run WASM and applicable web checks, including `cargo xtask ci`, and visually inspect browser cloud workflows.

Browser acceptance (2026-10-13) passed in Chromium with two render workers on both OPFS and IndexedDB. A native CLI peer and browser converged after independent develop/rating/flag edits. Browser checks covered verified smart-preview browsing, full-quality refusal without originals, explicit preview export, genuine offline editing/export, reload with a durable pending outbox and cached preview, reauthorization after reload, a 403 URL refresh, lost conditional-head responses, checkpoints, quota failures that retain pending work, retry, and original reacquisition/full-quality export. All 205 fixture requests passed independent SigV4 checks; every write was conditional. Results and inspected screenshots are under `target/cloud-acceptance/browser-acceptance-results.json` and `browser-*.png`. These procedural, development-build tests establish neither live bucket interoperability nor large-library performance. Final UI updates were 2 ms, with startup/update maxima of 6.8–7.2 seconds and thumbnail work still in flight. All eight CI stages passed in `target/browser-cloud-ci.log`; the WASM bundle and native release binaries built successfully. Setup, masked credentials, preview/original renders, reload, failed acknowledgement saving and Info authorization/retry screenshots were inspected. The browser Info Sync now button completed retry; the rebuilt native app also synchronized to the browser's generation through its Info button. Additional IndexedDB acceptance rejected saving an acknowledgement after remote publication, retained pending work, then completed the saved proof after storage recovered and the page reloaded (`browser-durability-results.json`). Backup verification confirmed cloud hashes and excluded login tokens/signed URLs (`browser-backup-results.json`).

## B2 append-only synchronization — follow-up implementation

**Authorized:** 2026-10-13. Implement the append-only design in [docs/cloud-sync-protocol.md](docs/cloud-sync-protocol.md), retaining the existing conditional-head protocol. Test against the supplied B2 bucket using isolated acceptance prefixes; credentials stay outside repository/settings files. The optional managed backend remains deferred.

### Protocol, storage and local durability

- [x] **B2.1** Add explicit, persistent append/CAS protocol selection and backward-compatible local synchronization progress; reject silent mode changes or a fallback after HTTP 501.
- [x] **B2.2** Add bounded, prefix-scoped ListObjectsV2 adapters with delimiter discovery, continuation pagination, start-after handling and malformed/out-of-scope response rejection.
- [x] **B2.3** Add narrowly scoped, verified immutable uploads and ordinary multipart completion for append mode; never expose unconditional shared-head replacement.
- [x] **B2.4** Add canonical hash-addressed manifests and per-author sequence/mutation records, preserving exact mutation IDs and bytes across retries.
- [x] **B2.5** Persist exact publication proofs, incoming changes and contiguous discovery progress before removing pending work; retain staging/receipts after uncertain outcomes or failed saves.
- [x] **B2.6** Rotate author identity on backup restore/ownership changes while retaining historical pending mutation IDs and causal context; detect cloned-author content collisions without overwriting either record.

### Native reconciliation and recovery

- [x] **B2.7** Implement gap-aware, paginated author discovery and causal dependency staging; never advance a frontier past a missing earlier operation.
- [x] **B2.8** Route managed imports and binary revisions through append mutation publication after verifying all asset dependencies; preserve upload recovery and cache promotion protections.
- [x] **B2.9** Publish/merge immutable checkpoints from remotely verified state only; support incomparable snapshots and pending-preserving bootstrap without remote garbage collection.
- [x] **B2.10** Add explicit coordinated CAS-to-append migration with verified seed state, preserved pending identities/mappings and retained legacy history; block unsafe mixed-mode continuation.
- [x] **B2.11** Expose append setup/status/progress/retry through registered commands and shared desktop/CLI/MCP control paths; preserve existing conflicts, tombstones and revision-aware undo.

### Browser integration

- [x] **B2.12** Extend browser connection/signing contracts with validated bucket roots and typed scoped listing/immutable-write requests, including URL expiry and cancellation.
- [x] **B2.13** Implement browser append publication/discovery/checkpoints with durable OPFS/IndexedDB progress and exact-ID acknowledgement proofs.
- [x] **B2.14** Wire browser protocol selection, setup/status/retry and restored-owner handling; preserve verified cache, smart-preview/export and quota/no-eviction behavior.

### Verification and documentation

- [x] **B2.A1** Test simultaneous writers, same-object races, lost upload/multipart/acknowledgement responses, restart with pending work and mutation identity collisions.
- [x] **B2.A2** Test late authors during pagination, missing earlier sequences, cross-author causal dependencies, malformed/listing scope/token bounds and failed-page recovery.
- [x] **B2.A3** Test historical-author restore, migration, incomparable checkpoints, pending exclusion from snapshots, delete-versus-offline-edit recovery and CAS regressions.
- [x] **B2.A4** Test browser OPFS/IndexedDB offline/reload/quota/expired-URL flows and native/browser append convergence; inspect real browser workflows.
- [x] **B2.A5** Verify small and multipart immutable uploads against live B2, then two native devices with independent/concurrent edits, checkpoints and reconnect/rebootstrap; ensure no shared-head or conditional-write request is emitted.
- [x] **B2.A6** Verify browser synchronization against live B2 through a scoped stateless signer, including original/proxy browse/export and native-peer convergence.
- [x] **B2.A7** Update protocol/authentication/migration/retention docs and applicable progress rows with verified evidence; keep unimplemented/unsupported work explicit.
- [x] **B2.A8** Run required full CI, WASM/web builds and desktop control-channel acceptance before committing implementation; check off only verified tasks.

Protocol verification (2026-10-14) passed all nine real CLI append scenarios, including a committed multipart completion response being lost, external binary revisions, deletion versus an offline edit with explicit recovery, historical-author restore, migration, pagination/gaps/forks and bootstrap. Shared engine tests cover causal dependency staging, remote-only/incomparable checkpoints and exact acknowledgements; owner tests cover full-disk rollback and the explicit-sync window after a slow scan. Browser receipt tests retain later and historical pending IDs across durable progress/acknowledgement recovery. All eight stages of `cargo +1.95.0 xtask ci` passed on the final source (`target/b2-ci-verified.log`); CAS regressions remain enabled. Native and web builds and desktop/browser screenshots are recorded in `target/b2-native-final.log`, `target/b2-web-final.log` and the acceptance folder.

Live B2 acceptance (2026-10-14) used isolated `lightcraft-acceptance/append/` and `lightcraft-acceptance/browser-append/` prefixes. Native small/9 MiB multipart imports, independent and concurrent metadata/develop edits, checkpoints, reconnect/rebootstrap and DNG/XMP revisions converged without shared-head publication or conditional-write headers (`target/cloud-acceptance/b2-append-live-results.json`, `b2-revision-live-results.json`; 132 requests in the native append run). Chromium passed OPFS and IndexedDB preview browsing/export, genuine offline edits, durable pending work across reload, URL expiry, a lost committed PUT response, quota failure/no eviction/retry, original reacquisition/export and native-peer convergence. IndexedDB also migrated a historical pending outbox from CAS mode. The two browser flows recorded 195 B2 responses (`b2-browser-live-results.json`, `b2-browser-wire-*.json`). Both finished with no pending work or gaps and two real render workers. These development/procedural checks do not establish live AWS compatibility or large-library performance. Browser final update times were 1–2 ms, with startup/update maxima of 12.1–13.9 seconds. Desktop control-channel acceptance rendered the managed DNG and inspected append setup/status; the desktop and browser Info Sync now buttons completed live B2 synchronization (`b2-browser-visual-results.json`) (`b2-native-visual-results.json`); final update time was 1.4 ms and maximum 37.8 ms with empty render queues. Temporary CORS was removed and the original absent configuration verified (`b2-cors-restored.json`); credentials and signed URLs were excluded from saved evidence.

After the complete live acceptance runs, an additional post-build B2 recheck reached the account's download bandwidth/Class B cap. Scoped listing still returned 200, while object GET returned 403 `AccessDenied` with the explicit cap-exceeded message (`target/cloud-acceptance/b2-final-access-probe.json`). This blocks further live download rechecks until the cap resets or the owner changes Caps & Alerts. It does not invalidate the earlier completed native/browser runs. The current native 403 error labels this as rejected credentials; the isolated diagnostic established the cap as the actual cause. Account caps were not changed.

## Coordinated migration workflow — follow-up

The migration command already verifies and seeds committed history. This follow-up exposes the coordinated cutover in desktop/browser UI and moves desktop network work off the UI thread.

- [x] **MIG.1** Run native migration on the cancellable replication worker; apply protocol/progress atomically on its owner, preserving edits created during migration, with save-failure rollback and retry.
- [x] **MIG.2** Add a shared migration dialog and File/setup/Info entry points; pause local publication, require stopped/upgraded peer confirmation and keep cancelled preparation paused until explicit Sync now.
- [x] **MIG.3** Verify CLI migration, active-transfer exclusion, cancellation after publication, stale owner/connection handling, pending-ID retention and durable failure recovery.
- [x] **MIG.4** Inspect and drive native/browser migration through their control paths, including preparation, confirmation, progress, cancellation/retry and completion; preserve credentials and staged originals.
- [x] **MIG.5** Update migration/progress documentation and run full CI, native and WASM/web builds; record live B2 verification or an actual service-cap limitation separately.
- [ ] **MIG.B2** Repeat the coordinated desktop/browser migration workflow against live B2 after its download/Class B cap clears. Current GET recheck returned HTTP 403 `AccessDenied` with the explicit cap-exceeded response (`target/cloud-acceptance/migration-b2-access-probe.json`); account limits were not changed.

Owner/CLI verification (2026-10-14): all eight replication-worker tests and nine real CLI append scenarios passed in the final workspace run. Migration regressions cover late-edit preservation, atomic full-disk rollback/retry, cancelled/stale owner or connection results, preparation cancellation and direct library replacement clearing the pause. Upload acknowledgement tests also reject cutover while a transfer is active and retain the catalog. Shared modal tests verify disabled confirmation and cancellation leaving preparation paused.

Workflow acceptance (2026-10-14) passed on the final native release and browser development bundle against an independently signed loopback S3 fixture. Desktop control requests and Chromium OPFS/IndexedDB exercised setup → migration, disabled confirmation, cancellation of preparation with pause retained, explicit resume, cancellation after the remote marker committed, retry, cutover without acknowledging the pending edit, subsequent append publication and native-peer convergence. Browser reconnect/reconfiguration preserved preparation pause; reload retained completed append mode and no pending work. Desktop Activity and browser Activity/Info migration progress, confirmation and completion screenshots were inspected. Native control inspections stayed below 39 ms during the deliberate network delay. Evidence: `target/cloud-acceptance/migration-native-results.json`, `migration-browser-results.json`, `migration-native-*.png` and `migration-browser-*.png`. These fixtures do not constitute a new live-B2 migration test; MIG.B2 remains blocked by the measured service cap.

Quality gates (2026-10-14): all eight stages of `cargo +1.95.0 xtask ci` passed on the final source (`target/migration-ci-final.log`). Native release and WASM/browser builds passed (`target/migration-native-final.log`, `target/migration-web-final.log`). The fixture independently verified all 429 requests across the final workflow runs and retained legacy heads (`target/cloud-acceptance/migration-fixture-summary.json`). Owned fixture/static helpers were stopped. The final signed B2 GET recheck at 2026-10-13 21:56 UTC again returned the explicit cap-exceeded response, so MIG.B2 stays unchecked.

## Cloud reliability review of CUX.12 — follow-up

The supplied 2026-10-17 review is tracked before implementation in
[cloud-reliability-tasks.md](docs/cloud-reliability-tasks.md#follow-up-review-local-conversion-recovery-and-focused-ux-2026-10-17).

- [x] **CR.58** Repair local-only background media completion.
- [x] **CR.59** Certify conversion against the original owner after receipt compaction.
- [x] **CR.60** Separate permanent-local editing from the bounded cloud outbox and reconcile rejoin safely.
- [x] **CR.61** Drain ready append operations during bounded discovery.
- [x] **CR.62** Persist absolute conversion destinations for relative library paths.
- [x] **CR.63** Preserve cloud diagnostics and cause-specific recovery in library-wide Details.
- [x] **CR.64** Make saved/new keys and named/default profiles explicit; probe and save the selected source.
- [x] **CR.65** Add bounded recovery pages and search, including Info and structural conflicts.
- [x] **CR.66** Share Info/popup Sync, Retry and Resume commands and migration guards.
- [x] **CR.67** Reset retention consent on view changes, Resume and library changes; retain task progress separately.
- [x] **CR.68** Unify control and widget dialog confirmation, preserve failed forms and discard stale cloud dialogs after library changes.
- [ ] **CR.69–CR.74** Extract managed media; preserve typed failures; refine cloud status; add conflict previews, Recently Deleted conversion and explicit rejoin guidance.
- [x] **CR.49** Fence successor publication, refuse unsafe cancellation and preserve explicit finish-upgrade recovery.
- [x] **CR.53** Verify cache destination adoption, honor old byte leases and fence cleanup before folder reuse.
- [ ] **CR.48, CR.50–CR.52, CR.54–CR.55** Recheck and close the previously tracked epoch, migration, status, lease, browser-cache, relocation, credential-isolation and discovery findings with current evidence. Detailed items remain individually tracked in the reliability checklist.

## Cloud queue UX — follow-up

- [x] **CUX.1** Add indexed per-photo sync status and shared queue/data totals, including metadata pending work, failed uploads and virtual-copy deduplication; support filters/chips and backward-compatible catalog loading.
- [x] **CUX.2** Persist device-local pause/resume, stop native uploads and metadata publication while preserving receipts/staging, and honor pause during restart/reconnect; support browser metadata pause.
- [x] **CUX.3** Make the upper-right cloud icon open a status popup with file counts, byte progress, errors and pause/resume; add Sync Status filtering and expose the controls to automation.
- [x] **CUX.4** Verify queue pause/resume with real native control requests and browser OPFS/IndexedDB workflows; inspect the popup and filters with procedural photos.
- [x] **CUX.5** Run focused regressions, full CI and native/web builds; update cloud and progress documentation with verified scope and limitations.
- [x] **CUX.6** Implement the supplied cloud-popup redesign: library summary, live upload progress, grouped attention with Choose/Details, contextual navigation and one Pause/Resume/Retry footer; verify representative states, small-window fit and native interactions.
- [x] **CUX.7** Match the corrected cloud-popup mockup: compact proportions, semantic vector icons, single-row photo/byte progress, idle-only last check, and accurate remote original bytes; verify rendering and actions.
- [x] **CUX.8** Add explicit desktop whole-cloud-library erasure with a fresh scope review, exact typed phrase and stopped-device acknowledgement; remove historical versions and unfinished uploads, retain terminal barriers, and verify interrupted retries plus empty-bucket initialization.
- [x] **CUX.9** Remove the duplicate cloud-popup settings action; automatically stage and upload the whole added native library on cloud setup, including later imports, while retaining IDs/edits, verified remote reuse, pause and restart/retry recovery.
- [x] **CUX.10** Include generated demo originals in automatic cloud synchronization as full-resolution float DNGs, retaining edits, IDs, virtual copies and scene rendering; verify staged recovery and native append-mode upload.
- [x] **CUX.11** Finish explicit native cloud on/off: merge/upload local library content, preserve container references and identities, materialize and verify all live cloud originals/supporting data into permanent local storage, blocking staged progress and recovery, safe cancel/restart, local-only network gates and browser capability guidance. Engine cloud tests (212 passed, one ignored), five UI regressions, three CLI loopback tests, actual desktop acceptance and all eight full CI stages passed; [evidence and limits](docs/cloud-acceptance-history.md#2026-10-17-device-local-cloud-onoff-cux11).
- [x] **CUX.12** Show whole-library preparation/upload batches promptly; use activity-aware 60-second/15-minute remote polling; replace the synchronization dialog with popup controls and focused conflict/storage tools.

Workflow verification (2026-10-14): the signed loopback fixture and real desktop control channel passed a six-file queue with a 9 MiB multipart original. Pause kept the queue idle across reconnect, cancelled active transfers while preserving confirmed part 1, the upload ID and durable originals, and allowed a new pending edit. Resume reused that multipart identity, completed all six files and cleared pending mutations. Control inspections stayed below 56 ms during the delayed transfer. Native popup/byte progress/paused-state and Pending filter screenshots were inspected (`target/cloud-ux-acceptance/native-results.json`, `native-*.png`). Chromium OPFS and IndexedDB passed the popup, status picker, persisted pause across reload/reconnect, explicit resume and native-peer metadata convergence (`browser-results.json`, `browser-*.png`). Browser original uploads remain unsupported; these are fixture checks, not a new live-B2 acceptance run.

Quality gates (2026-10-14): all eight stages of `cargo +1.95.0 xtask ci` passed (`target/cloud-ux-ci.log`). Native release and browser builds passed (`target/cloud-ux-native-final.log`, `target/cloud-ux-web-final.log`). The final full suite passed the indexed-status, pause persistence/write-failure and popup/filter regressions. Incomplete setup in concurrent reliability tests was corrected by selecting the slider's photo, supplying the dialog connection response and initializing its first frame, and enabling sync before testing the cleanup reservation. These corrections affect test fixtures only. Owned cloud-UX fixture/static helpers were stopped.

Redesign verification (2026-10-16, CUX.6): 24 focused cloud UI checks passed, including four
new state/navigation/fit regressions. The up-to-date, uploading, attention and paused screenshots
were inspected. Native presented-window control checks passed pause, resume, storage navigation
and synchronization navigation using an isolated local catalog; the app was closed afterward.
All eight `cargo +1.95.0 xtask ci` stages and a final UI clippy check passed. Evidence and
reproduction notes: [cloud-acceptance-history.md](docs/cloud-acceptance-history.md#2026-10-16-cloud-popup-redesign).

## Cache preferences — follow-up

Settings → Cache, laid out like Lightroom's Preferences → Cache: the cache's drive and what
uses it, the size limit, Clear cache, library-wide offline copies and the originals location.

- [x] **CC.1** Report the cache drive (label, size, free space), library app data, photo cache and offline bytes (`cloud.storage`); clear every eligible download without changing the limit (`cloud.clearCache`); keep every managed original and/or smart preview offline, including later arrivals, with a version-2 retention policy that loads version 1 (`cloud.cache` `keepAllOriginals` / `keepAllPreviews`).
- [x] **CC.2** Settings → Cache page with Lightroom's layout (side navigation, usage bar and legend, Manage Storage, size-limit menu, Clear cache with its explanation, the two Store-a-copy choices, originals location) driven by those commands.
- [x] **CC.3** Choose another storage location for the cloud cache (Browse…): verified move of the cached blobs and retention settings, then reopen on the new folder; an unavailable folder reports an error and falls back to the library folder.
- [ ] **CC.4** Verify in the running app against the reference screenshot; update cloud storage documentation and the parity tracker.

## Milestone 6 — Optional managed metadata backend

**Deferred at the user's request.** These tasks remain unchecked for a future decision.

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
