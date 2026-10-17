# Cloud acceptance history

Last updated: 2026-10-17

These are historical reports from specific releases, not a current acceptance claim.
Statements such as "not implemented", format versions, test counts and "remaining"
describe the dated snapshot only. Current behavior is in
[cloud-storage-implementation.md](cloud-storage-implementation.md), normative rules in
[cloud-sync-protocol.md](cloud-sync-protocol.md), and current completion state in
[cloud-reliability-tasks.md](cloud-reliability-tasks.md). The original milestone index
is [../tasks.md](../tasks.md). The optional managed metadata backend is deferred.

Paths under `target/` are gitignored local evidence, not portable test fixtures.
Checked-in regression tests are runnable via full repository CI; small procedural
fixtures do not establish real-camera fidelity or large-library performance.
Live B2 reports precede later reliability changes. Live AWS remains unverified.
Account-cap and CORS restoration caveats remain attached to the runs below.

## 2026-10-14–17: subsequent reliability work

[The reliability verification log](cloud-reliability-tasks.md#verification-log) retains
the exact task/commit context, final-source CI, browser/native fixture runs and CR.9
10,000/50,000-photo and CR.10 request-count measurements. Consult it for CR.1–CR.41;
the earlier reports below do not supersede those changes or verify newer source.

## 2026-10-17: device-local cloud on/off (CUX.11)

The on/off controls distinguish disconnecting from Pause. Turning on retains photo
identities and conflicting edits while merging and uploading local content; immutable
SHA-256 bytes are deduplicated. Turning off confirms the permanent destination and
scope, blocks library interaction, and shows stage, file/byte progress, remaining work
and recovery actions. Sync remains enabled until the received catalog and every live
original/supporting blob are verified and durably saved. Other devices are not frozen;
another scan incorporates arrivals observed during transfer. See the
[transition contract](cloud-sync-protocol.md#turning-synchronization-off-on-this-device).

Isolated validation with Rust 1.95.0 on Windows:

- `cargo +1.95.0 xtask ci` passed all eight stages: fmt, strict Clippy, HEIF,
  workspace tests, parity references, layers, assets and WASM. Final log:
  `cargo-xtask-ci-final-success.log` in the evidence directory below.
- Engine cloud tests: 212 passed, one ignored. Coverage includes retained content
  choices, sidecars, insufficient space, cancellation, corrupt copies, failed durable
  writes, restart, cache/output protection and queued-download network gates.
- Five headless UI tests passed, covering confirmation, blocked editing/navigation and
  Escape, interrupted recovery, safe Quit, completed mode, and persistent Retry/Cancel
  errors, plus the local-only Info action. Screenshots at 1200×900 and 900×600 were inspected.
- Three real-CLI loopback-provider tests passed, exercising both protocols, local/cloud
  merge, album membership, concurrent arrivals, restart and retry. Removing only the
  fixture's staging/cache still permits byte-exact offline original export with no
  requests. Local edits publish after explicit enable; conversion permits GET/HEAD only.
- The native control-channel test passed with a held original GET: confirmation,
  presented download progress, rejected navigation, Escape protection, verified
  completion, local-only popup, Info and cache-settings guidance. The durable bytes
  match the original and the provider objects are unchanged. The final `ui.inspect`
  sample reports 26.82 ms UI update, 5.17 ms last render and empty render queues; this tiny fixture is not a
  large-library performance benchmark.

Local evidence is under
`C:\Users\pdykstra\AppData\Local\Temp\lightcraft-transition-evidence`:
`before-settings.png` and `after-settings.png` are CLI headless renders;
`ui-final-audit/` contains mocked headless UI fixtures. Actual desktop window captures
using an isolated loopback provider are under
`native/toggle/{confirmation,downloading,complete,off,off-info,off-cache-settings}.png`
with `inspect.json` and the native log (no compositor-fallback warnings).
Engine, UI, CLI and native logs are retained alongside them. The native test is
`cloud_toggle_native_confirmation_blocking_progress_and_durable_completion` in
`apps/lightcraft-cli/tests/cloud_library_mode.rs`; it requires
`LIGHTCRAFT_NATIVE_LIBRARY_APP` and `LIGHTCRAFT_LIBRARY_SNAPSHOTS` and runs with
`--ignored`. The ordinary CLI tests are in `cloud_toggle.rs`.

No real library or live provider was changed. Browser conversion remains unavailable
with desktop guidance; permanent storage requires a filesystem supporting hard links.
Optional CJK fonts were inaccessible in this environment, so screenshots use the
bundled English font. Browser interaction and live-provider acceptance were not rerun.

## 2026-10-12-13: native foundation and managed revisions

### Verification and remaining work

Live Backblaze B2 acceptance on 2026-10-12 used a private bucket at the
`s3.us-east-005.backblazeb2.com` endpoint and a dedicated random test prefix. Download,
develop, full-quality export, explicit preview export, local-original removal and original
reacquisition passed with the generated RAW. Credentials were supplied only through process
environment variables and were not stored in the library, app settings or test evidence.
The endpoint returned HTTP 501 `NotImplemented` for both `If-None-Match: *` and `If-Match`.
Conditional-head catalog publication therefore remains unavailable on this B2 endpoint. LightCraft
reports unsupported conditional writes and retains staged bytes and multipart receipts;
it never falls back to an unconditional catalog overwrite. Live AWS remains unverified.
Sanitized results are `target/cloud-acceptance/live-b2-results.json` and
`live-b2-read-results.json`. Only the dedicated procedural test objects were written;
existing objects and bucket policies were left untouched. Provider setup follows the
[Backblaze S3 API documentation](https://www.backblaze.com/apidocs/introduction-to-the-s3-compatible-api);
the conditional-write limitation above was observed directly in the live test.

Explicit append-only B2 synchronization passed live native and browser acceptance on
2026-10-14. Immutable operations, checkpoints and verified small/multipart media use ordinary
requests and no shared head. Existing CAS libraries require coordinated migration; mode never
changes automatically after a 501. See [cloud-sync-protocol.md](cloud-sync-protocol.md) and
[browser-cloud.md](browser-cloud.md) for setup, migration, durable recovery and evidence.
The temporary browser acceptance CORS rule was restored to the original absent configuration.

Catalog tests cover malformed references, shared originals/virtual copies, journal replay and compaction, format-7 upgrade and legacy file sources. A synthetic regression rejects `PhotoId(u64::MAX)` as an error instead of overflowing.

Engine tests cover a cloud-only synthetic Bayer DNG opening, developing, exporting and reopening from verified cache; concurrent requests; lease protection; corrupt/interrupted/cancelled/auth-failed transfers; injected failed writes; and path traversal rejection. A proxy-only render cannot satisfy a managed export's original requirement. A UI regression verifies retry after download completion without a catalog edit.

`apps/lightcraft-cli/tests/cloud.rs` drives the real CLI and MCP binaries against a loopback S3-shaped server with generated RAW bytes and fake credentials. This test passed on Windows with Rust 1.95.0. Set `LIGHTCRAFT_CLOUD_ACCEPTANCE_DIR` during the test to save its generated DNG for a separate desktop acceptance run.

The native desktop acceptance also passed on 2026-10-12 using control port 18837 and a separate loopback server that independently checked SigV4 with Python's HMAC implementation. It attached the remote-only RAW, applied +0.75 exposure, reached a completed native render, saved its catalog and exported a 32×24 PNG. One verified request acquired the original; the export reused it. The inspected native screenshot and results are in gitignored `target/cloud-acceptance/`. The final inspected frame measured 0.82 ms for UI update and 6.29 ms for the last render, with no queued or in-flight renders. These measurements use a tiny synthetic fixture and are not a large-library performance benchmark. This does not establish live AWS interoperability. Per-task completion and actual validation results are recorded in [../tasks.md](../tasks.md).

Synchronization/conflicts and browser adapters are implemented and verified, as described below. The optional backend is deferred at the user's request. Durable local import, recovery, verified uploads, staging/cache migration, explicit idle multipart retention and conditional publication are implemented; unsynchronized originals and external working files are protected from cache eviction. Browser workers accept verified managed original/preview bytes; the separate acceptance evidence below establishes synchronization behavior.

Managed DNG conversion, XMP saves and external-edit return ingestion use the durable preparation worker.
They create new asset and upload identities while retaining the photo's stable cloud identity. DNG
conversion verifies decoded samples before relinking the photo and its virtual copies. Existing
originals and revisions are retained for undo and recovery. Managed XMP uses
`associated/xmp/<sha256>.xmp` under the library prefix, preserves foreign properties, and is verified
and uploaded with the image dependencies before catalog publication. An unreadable associated packet
is retained with an error. Automatic neighboring-file XMP writing remains specific to local files;
managed XMP is explicitly saved through the shared metadata command.

External edit copies are rendered into durable `Managed/working/<identity>/<name>` files, staged as
managed photos, and stacked with their sources in one journal record. The native host launches the
editor after successful preparation. On focus return, Reload stages changed bytes as an immutable
revision with the same photo identity; unchanged bytes create no additional upload. Working paths
and ingested-byte baselines remain device-local. Cache eviction never visits this working namespace,
even after upload; ordinary exports cannot overwrite a bound working file. Merge results from managed
inputs are durable local DNG files in `Managed/working/merges`, acquired through the original resolver
and imported normally; they can subsequently be imported into cloud storage. Denoise continues to
acquire original bytes through the resolver on its worker.

Catalog format 10 adds associated assets, global identity mappings and working-copy bindings while
preserving older catalogs. Staging manifest version 2 identifies a revision's target and base asset,
so recovery relinks the existing photo rather than duplicating it. Publication batch version 2 records
the base asset explicitly and accepts legacy version-1 imports; a previously pending binary revision
uses its retained immutable description when a newer revision has replaced it locally. This binary
publication support does not synchronize later ratings or develop edits between devices.

`cargo +1.95.0 xtask ci` passed fmt, strict Clippy, HEIF checks, workspace tests, parity references, layering, asset attribution and WASM on Windows. The required Rust 1.95 toolchain exposed older Clippy suggestions and Unix-specific test assumptions, which were corrected. Recovery tests also exposed duplicate Windows `.xmp`/`.XMP` alias moves: sidecar operations now deduplicate physical aliases, preserve actual filename spelling and retain distinct packets on sensitive volumes. Synthetic and native regressions cover these cases.

Native UI dispatch is cached-only: it cannot start a transfer or wait for another download's lock. Metadata, external-edit and foreground export/control-render requests acquire remote content on a worker, then resume on the owner thread with leases held. Pending actions retain their initial target IDs and are rejected after a library/source change. Control clients receive their ordinary result after acquisition; GUI commands expose an activity task, including cancellation. Normal view rendering, merge, denoise and background exports already acquire on workers. Private original-reader handles include a NUL separator so valid local filenames cannot collide with them.

User-output guards protect managed originals and renditions, and reject writes anywhere inside the managed cache, including prospective filenames and paths through existing directory links. Internal verified-cache writers retain their own publication path.

A second native acceptance used a 2.5-second server delay. Concurrent metadata and `ui.render` requests completed after one independently verified download while another connection's `ui.inspect` answered in 26 ms. It returned 31 metadata rows, rendered 32×24, exported at original dimensions and rejected a render output pointed at the original cache file; its SHA-256 remained intact. The inspected screenshot is `target/cloud-acceptance/native-deferred.png`; results are `native-deferred-results.json`. Final UI update time was 0.73 ms with empty render queues. These are small-fixture responsiveness checks.

Native retention acceptance passed on 2026-10-12 through control port 18837. Clicking Info → Keep original offline persisted the pin; Settings → Performance displayed the separate budget and its pinned-only option. After the local S3-compatible server stopped, reopening retained the pin and a zero-byte budget, and an adjusted RAW exported at 32×24. Pinned removal was refused. Unpinning and explicit removal deleted the cached original while preserving the photo, its remote reference and an empty Missing Photos result; browsing did not recreate the removed original. Screenshots (`native-cache-info.png`, `native-cache-settings.png`, `native-cache-removed.png`) and results are in gitignored `target/cloud-acceptance/`. Final native update time was 7.79 ms with no queued/in-flight renders; this tiny fixture is not a large-library benchmark. Engine tests additionally verify editable preview pins across restart, separate album/photo ownership, corruption recovery and atomic-save failures. CLI/MCP retention acceptance passed too.

Native preview acceptance passed on 2026-10-12 with a 2.5-second proxy delay: a verified thumbnail appeared first, then an editable `.lcsp` render. Browsing fetched one thumbnail and one proxy and never requested the original. The maximum observed control-inspection response during the transfer was 56 ms; the final native update was 0.40 ms with empty render queues. +0.5 exposure rendered from the proxy. Full export reported an HTTP 503 while the original was unavailable, explicit proxy export succeeded at 32×24, and enabling/downloading the original refreshed the view and allowed full export. The decoded proxy/full export channels differed by at most one 8-bit level on this procedural fixture; `.lcsp` is a reduced-quality editing proxy. Screenshots and request/result logs are in gitignored `target/cloud-acceptance/native-preview-*` and `preview-server-requests.json`. This is not a large-library performance or real-camera fidelity benchmark.

Native transfer acceptance passed on 2026-10-12 through port 18847 against a signed loopback server that sent an original in delayed chunks. The Info panel displayed partial byte progress and cached-preview availability. Cancelling the activity left the original uncached and editing from the proxy succeeded. Clicking Reconnect and retry acquired the original, restored cached availability and allowed 32×24 full export. All HTTP requests passed independent SigV4 verification. Screenshots and results are `target/cloud-acceptance/native-transfer-*`; maximum observed inspection latency was 36 ms and the final native UI update was 1.18 ms with empty render queues. These measurements use a tiny procedural fixture, not a large-library benchmark. Focused tests also cover the four-download limit, priority ordering and duplicate promotion, cancellation while waiting for a shared blob or network slot, preview use during an original transfer, render-driven activity cancellation/reconnect, and one-shot CLI wait completion. Live AWS remains unverified.

Native staging acceptance passed on 2026-10-12 through port 18857 with storage unavailable. A background RAW import produced durable original/proxy/thumbnail bytes and a saved local upload intent. After removing its private source copy, the photo accepted develop changes and exported at 32×24, survived restart with a zero-byte cache budget, and exported again. Local-copy removal was refused; explicit recovery skipped the recorded import without duplication. Inspected screenshots and results are `target/cloud-acceptance/native-import-*`. The restarted app's final UI update was 0.71 ms with empty render queues; this tiny procedural fixture does not measure large-library or camera fidelity. CLI/MCP staging and export acceptance also passed without HTTP requests. Regression tests cover abandoned prepared results, journal-append failure, immutable output protection, overlapping directory mappings, durable working-directory preservation, and library switching during background preparation.

Native upload acceptance passed on 2026-10-12 through port 18867 against an independently signed S3-compatible loopback fixture. The connection and import dialogs, byte-progress activity, failure text, Retry upload and Original synchronized state were visually inspected. A generated RAW padded to 9 MiB was saved locally, edited and exported at 32×24 after its private source was deleted, while multipart upload was still pending. A deliberately lost part response retained the original and refused local-copy removal. Clicking Retry upload resumed the same upload ID, verified all three object keys and published one catalog head. All requests passed independent Python HMAC/SigV4 checks. Results/screenshots are in gitignored `target/cloud-acceptance/native-upload-*`; maximum observed inspection latency was 43 ms and the final UI update was 0.50 ms with empty render queues. This procedural fixture is not a large-library performance or real-camera fidelity benchmark; live AWS remains unverified.

Six real CLI/MCP upload tests cover response loss, restart after remote publication, embedded HTTP-200 completion errors, explicit abort, rejected credentials, independent queued work and byte-progress/ping/cancellation. Owner-event tests cover failed local acknowledgement and retry, old-library events and changed connection settings. Staging/cache promotion and managed path-dependent flows subsequently passed their own acceptance checks below.

Native organization acceptance passed on 2026-10-12 through control port 18867 with storage offline.
The renamed `Trip-12.dng` appeared under Cloud archive → Trip, retained its synchronized original and
asset identity, and exported at 32×24. All staged bytes stayed identical. The inspected screenshot is
`target/cloud-acceptance/native-cloud-organization.png`, with results in `native-organization-results.json`.
The last render took 14.04 ms; the captured UI update took 33.37 ms with empty render queues. These
measurements use a tiny procedural fixture. Regression tests additionally cover mixed local/cloud
rename, undo/redo, folder membership across restart and overflow rejection before any name or path changes.

Native multipart retention acceptance passed on 2026-10-12 through control port 18867 against an independently
signed loopback fixture. A generated RAW remained editable/exportable after a deliberately failed completion.
After its catalog photo was permanently removed, preview found its eight-day-old receipt without changing it.
Clicking the activity Cancel control retained an uncertain abort receipt. Retry confirmed a still-present part,
repeated abort, then removed the recorded session. All staged blobs and a pending working file stayed byte-identical;
no catalog head was published. The inspected progress screenshot and results are in gitignored
`target/cloud-acceptance/native-cleanup-*`. Maximum observed inspection latency was 47 ms; final UI update
was 7.01 ms with empty render queues. These are tiny procedural fixtures; live AWS remains unverified.

CLI/MCP regressions cover deleted-photo cleanup, preview/young-session preservation, repeated abort confirmation,
rejected credentials/destination changes, ping/cancellation and uncertain-response recovery. Engine checks cover
future timestamps, corrupt/future/legacy/misnamed receipts, connection reservations and failed retirement writes.

### Staging migration and deletion acceptance

Staging/cache and deletion acceptance passed on 2026-10-12 through native control port 18867.
The generated 9 MiB RAW was editable while upload credentials were rejected. Pending and pinned
removals were refused; the Info button displayed the actual refusal and later successful removal.
Proxy export succeeded without original access, full-quality export reacquired the original, and
permanent library deletion left every remote object and a durable working file unchanged. A corrupt
ownership record appeared as a staging error in Performance settings and was preserved until repaired.
Screenshots were inspected: `native-managed-retention-pinned-refusal.png`,
`native-managed-retention-removed.png` and `native-managed-retention-staging-error.png`.
Results and independently verified signed requests are in `target/cloud-acceptance/`.
Maximum inspection latency was 48 ms; final UI update was 18.74 ms with empty render queues.
These are tiny procedural images, not real-camera or large-library benchmarks. Live B2 read/export acceptance passed; B2 conditional publication is unsupported as recorded above. Engine regressions cover six durability, ownership, cancellation, corruption and
restart scenarios; real CLI/MCP upload coverage includes legacy proof upgrade and retention/reacquisition.

### Managed revision acceptance

Managed revision acceptance (2026-10-13): all eight CI stages passed in `target/cloud-managed-revisions-ci-final.log`. Fifteen focused preparation/recovery tests and real CLI/MCP revision tests cover partial rendition recovery, source changes, protected hard-link aliases and immutable upload descriptions. Native control-channel checks converted a procedural RAW to DNG, saved/read associated XMP, launched an actual test editor, ingested its returned 16×12 TIFF under the same derived photo ID, exported at full dimensions and removed eligible cache data while retaining the working file. All 54 fixture requests passed independent signing checks. Screenshots `native-revisions-dng.png` and `native-revisions-external.png` were inspected; results are in `target/cloud-acceptance/native-revisions-results.json`. Maximum inspection latency was 59 ms on this tiny fixture.

DNG encoding verifies decoded sample identity before publication. XMP is a bounded associated immutable blob; explicit reads and writes use managed storage, while automatic neighboring-file sidecar writes remain local-file-only. External working copies are durable device data, never disposable cache. Recovery refuses changed source bindings and keeps staged originals. Full-resolution merges acquire originals on workers and produce durable local outputs. These checks do not establish synchronization of subsequent edits between devices.


## 2026-10-13: native causal synchronization

Verified core evidence is in `target/cloud-sync-engine-tests.log`, `target/cloud-sync-owner-tests.log`, `target/cloud-sync-cli-tests.log` and `target/cloud-sync-ci-final.log`. Eleven shared-engine scenarios, two owner/persistence tests and four real CLI scenarios complement five catalog reconciliation tests. They cover independent offline imports, metadata/membership convergence, competing recipes, deletion recovery, revision-aware undo, bounded migration, compaction, atomic failure/retry, conditional races/response loss, incremental downloads and checkpoint rebootstrap with pending work. Browser acceptance and its authorization/storage contract are described in [browser-cloud.md](browser-cloud.md).

Native synchronization acceptance (2026-10-13) passed through control port 18877 with a signed loopback S3 fixture. Two catalogs converged after independent metadata/develop edits; the Info conflict controls opened Versions and explicitly resolved a recipe while preserving its alternative. Permanent deletion followed by an offline peer edit did not recreate the photo; clicking Recover restored the retained five-star edit, then both catalogs synchronized. Cancellation retained pending work, rejected credentials produced actionable Info text, and clicking Sync now completed retry. Setup, progress, failure, conflict, versions and recovery screenshots were inspected. All 140 scenario requests passed independent signing checks; every write was conditional. Results are in `target/cloud-acceptance/native-sync-results.json`. The isolated delayed-head check recorded 61 inspection samples with a maximum of 52 ms; the complete feedback flow recorded a 493 ms outlier around screenshot/control interaction. Final UI update time was 8.87 ms with empty render queues. These tiny procedural checks do not establish large-library performance or live AWS compatibility. All eight CI stages passed in `target/cloud-sync-ci-final.log`.


## 2026-10-13: browser conditional-head adapter

Both browser stores passed Chromium browse/edit/export, offline preview, pending reload,
reauthorization, expired URL, lost response, checkpoint, native-peer convergence, quota retention
and original reacquisition checks. All 205 fixture requests verified and every write was
conditional. Results: `target/cloud-acceptance/browser-acceptance-results.json`. The development
bundle's final UI updates were 2 ms; initial/update maxima were 6.8–7.2 seconds with procedural
thumbnail work in flight. No live provider or large-library performance claim follows. All eight CI stages passed in
`target/browser-cloud-ci.log`. Browser Info retry and the rebuilt desktop Info synchronization
button passed; a deliberately failed post-publication acknowledgement retained pending work,
then recovered its saved proof on reload. Backup inspection verified cloud hashes and excluded
session login tokens and signed URLs.


## 2026-10-14: cloud queue controls

Verification (2026-10-14): native six-file queue/multipart pause-resume and browser OPFS/IndexedDB
popup/filter/reload/native-peer workflows passed against the signed loopback fixture, with
screenshots inspected (`target/cloud-ux-acceptance`). Native release and browser builds passed
(`target/cloud-ux-native-final.log`, `target/cloud-ux-web-final.log`); all eight full CI stages
passed (`target/cloud-ux-ci.log`). This UX acceptance does not add a fresh live-B2 or large-library
performance claim.


## 2026-10-12: superseded import-only protocol

Historical initial import publication used a bounded version-1 protocol (superseded by
the causal protocol in [cloud-sync-protocol.md](cloud-sync-protocol.md)):

| Object | Role |
|---|---|
| `<prefix>/originals/<sha256>` | Immutable original bytes |
| Versioned preview/thumbnail keys from `assets::keys` | Immutable supporting renditions |
| `<prefix>/catalog/changes/<sha256>.json` | Immutable import batch with its parent batch reference |
| `<prefix>/catalog/head.json` | Small head containing protocol version, generation and content-addressed tip |

The publisher reads the head and its ETag, reconciles an already published import identity, creates a batch with `If-None-Match: *`, and creates or advances the head with `If-None-Match: *` or `If-Match`. A failed condition reloads and rebases against the new head; there is no unconditional head write. A lost head response is resolved by finding the same mutation and exact immutable snapshot in the verified chain. Unsupported/malformed heads and mismatched identities stop publication without replacing remote data. Batch size is capped at 2 MiB, publication retries at 16, and history traversal at 4,096 batches. Checkpoints and history compaction remain future work.


## 2026-10-14: append protocol and coordinated cutover

### Verification evidence

Nine real CLI tests cover concurrent writers, lost PUT/multipart completion responses, one-entry listing pages, late author
insertion before a page cursor, missing predecessors, failed pages, cloned-author collisions,
checkpoint bootstrap, historical pending recovery, migration, binary revisions and explicit
delete-versus-offline-edit recovery. Pure tests
cover cross-author dependencies, incomparable checkpoints, remote-only snapshots and exact
acknowledgements. A failed owner-side journal save rolls back incoming fields, append progress
and pending acknowledgement together. The original CAS tests remain enabled.

Live B2 evidence is in `target/cloud-acceptance/b2-append-live-results.json` and
`b2-revision-live-results.json`: small/9 MiB multipart originals, independent/concurrent native
edits, checkpoints/rebootstrap, DNG/XMP revisions and peer convergence. The append run recorded
132 native S3 requests with no conditional-write headers or shared-head publication. This is
procedural acceptance against the supplied bucket, not live AWS or large-library benchmarking.
Browser results and final CI are recorded separately in [browser-cloud.md](browser-cloud.md)
and [../tasks.md](../tasks.md). Acceptance objects use owned prefixes; no existing library objects are removed.

All eight final CI stages passed (`target/b2-ci-verified.log`). Desktop and browser Info Sync now
completed live B2 reconciliation; append setup/status screenshots were inspected. The temporary
browser CORS rule was removed and the original absent configuration verified.

An extra post-build recheck later hit B2's download bandwidth/Class B cap (object GET 403,
scoped LIST 200). Complete live acceptance passed before that limit. See
`target/cloud-acceptance/b2-final-access-probe.json`; account caps were not changed.


Coordinated workflow acceptance (2026-10-14) passed on the native release and Chromium OPFS/
IndexedDB adapters against an independently signed loopback S3 fixture. It exercised disabled
peer confirmation, cancelled preparation/explicit resume, cancellation after remote marker
publication, retry, preserved pending edits, append convergence and browser reload. Native
and browser Activity/Info progress and completion were visually inspected. Evidence:
`target/cloud-acceptance/migration-native-results.json`, `migration-browser-results.json`
and `migration-*.png`. A fresh B2 GET still returned 403 `AccessDenied` with an explicit download/
Class B cap-exceeded response (`migration-b2-access-probe.json`); the new live workflow recheck
remains pending. No account caps or billing settings were changed.

All eight final CI stages passed (`target/migration-ci-final.log`), including the owner-worker
rollback/cancellation/library-switch regressions and the shared modal confirmation test. Native
release and web builds passed; fixture/static helpers were stopped after acceptance.


## 2026-10-13: browser storage and conditional publication

### Verification and limits

Chromium acceptance passed on both OPFS and IndexedDB with two real render workers:
browse/edit/export, offline preview operation, pending edits and cache across reload,
reauthorization, URL expiry, lost head response/checkpoint recovery, native/browser convergence,
quota failure/retry and original reacquisition. All 205 signed-fixture requests verified;
every write was conditional. A separate failure after remote publication retained pending IDs
until the acknowledgement receipt actually saved; reload then finished acknowledgement without
network access. Cloud backup entries verified against their SHA-256 keys and excluded session
tokens/signed URLs. Info's Sync now button recovered from a rejected login. All eight stages of
`cargo +1.95.0 xtask ci` passed (`target/browser-cloud-ci.log`). Screenshots and results are in
`target/cloud-acceptance/browser-acceptance-results.json`; focused native web tests also cover
source replacement, corrupt bytes, acknowledgement recovery and backup path/hash validation.

These are procedural tests of a development bundle. They do not establish live AWS/provider
interoperability or large-library performance. The provided B2 bucket returns
HTTP 501 for conditional writes; use explicit append mode below. See
[the native compatibility evidence](cloud-storage-implementation.md). No unconditional fallback
is enabled. Bucket CORS and a user-controlled signer must be configured for a live deployment.
The optional managed metadata backend remains deferred.


## 2026-10-14: browser live append and migration

Live append acceptance (2026-10-14) passed on OPFS and IndexedDB through a scoped stateless
signer and the actual B2 endpoint. It covered offline preview editing/export, pending work
across reload, expired URLs, a lost committed PUT response, quota failure/no eviction/retry,
original reacquisition/export and native-peer convergence. IndexedDB migrated its historical
pending CAS outbox. Both finished with no pending work or gaps and two real render workers;
195 B2 responses were recorded. Results and screenshots live in
`target/cloud-acceptance/b2-browser-live-results.json` and `b2-browser-*.png`. Final UI updates
were 1–2 ms, with startup/update maxima of 12.1–13.9 seconds. This is procedural acceptance of
a development bundle, not a large-library performance measurement. Test origins receive
temporary CORS rules; the original absent configuration was restored and verified after testing.
Desktop and browser Info Sync now completed live B2 reconciliation, with append setup/status
screenshots inspected. All eight final CI stages passed (`target/b2-ci-verified.log`). The optional metadata backend remains
out of scope; a live browser deployment still needs its own signer, login and bucket CORS.

After acceptance completed, an extra B2 object GET returned 403 because the account download
bandwidth/Class B cap had been reached; scoped listing still worked. The native adapter
currently labels a 403 as rejected credentials, so check B2 Caps & Alerts as well as key
permissions. Further live download rechecks await a cap reset or an owner-approved cap change.
The complete earlier acceptance results remain recorded; account caps were not changed.


The coordinated migration dialog was additionally verified on 2026-10-14 in Chromium with
both OPFS and IndexedDB against the independently signed loopback fixture. Preparation pause,
confirmation, reconnect without resuming, cancellation after seed publication, retry, pending
retention, native-peer convergence and durable completed-mode reload passed. Visible Activity/
Info migration progress and confirmation/completion were inspected. Results:
`target/cloud-acceptance/migration-browser-results.json` and `migration-browser-*.png`.
The account cap above still blocks a fresh live-B2 workflow test; this fixture evidence is
separate from the earlier completed live append tests.

The final native release and browser bundle built successfully; all eight CI stages passed
(`target/migration-native-final.log`, `target/migration-web-final.log`, `target/migration-ci-final.log`).
Owned fixture/static helpers were stopped after acceptance.


## 2026-10-14: browser status controls

OPFS and IndexedDB popup/
filter/pause-reload/native-peer checks passed against the signed loopback fixture:
`target/cloud-ux-acceptance/browser-results.json` and `browser-*.png`.


## 2026-10-16: browser append audit acceptance

2026-10-16 CR.10 acceptance: actual Chromium OPFS and IndexedDB resumed the same historical-audit
round after reload (64 then 35 record GETs after the first 64-record slice of a 162-operation history).
Three subsequent automatic polls per backend made 12 manifest/LIST requests and no covered operation,
checkpoint or receipt-node GETs. All fixture requests were signed; no page errors occurred. See the
[dated reliability evidence](cloud-reliability-tasks.md). This is loopback acceptance, not a live B2
or large-photo-library performance result.


## 2026-10-16: cloud popup redesign

CUX.6 implements the supplied summary/uploading/attention layout, actionable conflict and
error links, and the Pause/Resume/Retry footer. Twenty-four focused cloud UI checks passed;
four new regressions exercise the redesigned states, navigation and a 900×600 window.
Screenshots under `target/cloud-popup-acceptance/` (`up-to-date.png`, `uploading.png`,
`needs-attention.png`, `paused-small-window.png`) were inspected. These headless UI fixtures
supply representative cloud state; they do not perform provider operations.

The final desktop build passed real control-channel pause/resume and storage/synchronization
navigation with a separate local catalog. Presented-window captures `native-popup-final.png`
and `native-paused-final.png` were inspected; no headless fallback occurred. Inspection/perf
and interaction results are retained in `native-final-inspect.json` and
`native-final-results.json`. The scratch app was closed after verification.

All eight `cargo +1.95.0 xtask ci` stages passed in `target/cloud-popup-ci.log`, including
774 passing UI tests and the WASM checks. The final UI clippy check passed in
`target/cloud-popup-final-clippy-verified.log`. The native build log is
`target/cloud-popup-native-final-build.log`.

To reproduce the representative screenshots from the repository root in PowerShell:

```powershell
$env:LIGHTCRAFT_POPUP_SNAPSHOTS = "$PWD/target/cloud-popup-acceptance"
cargo +1.95.0 test -p lightcraft-ui-egui --lib cloud_popup_redesign
```

## 2026-10-16: corrected cloud popup mockup

CUX.7 adds original check-circle, sync, warning, pause, conflict and cloud-off vectors;
compact 214-point cards; inline photo/byte progress; idle-only last-check information;
and aligned attention links and footer actions. The cloud-original total excludes
locally staged originals, retaining deduplication and remotely received catalog records.
The photo progress numerator is actively uploading photos, not completed photos.

Twenty-four focused cloud UI checks passed. The final workspace suite also passed,
including the popup states/actions and catalog remote-byte regressions. Final fixture
screenshots (`up-to-date.png`, `uploading.png`, `needs-attention.png`, and
`paused-small-window.png`) under `target/cloud-popup-corrected-acceptance/` were inspected
against the corrected mockup. Fixtures supply representative states, not provider I/O.

The final native build passed pause/resume and both settings-navigation checks through
the control channel using a separate scratch catalog. Presented-window normal/paused
screenshots were inspected with no headless fallback; the app was closed afterward.
Results and perf are retained in `native-final-results.json` and
`native-final-inspect.json` in the same artifact directory. Build and final lint logs are
`target/cloud-popup-corrected-final-build2.log` and
`target/cloud-popup-corrected-final-clippy.log`.

All eight CI stages passed (`target/cloud-popup-corrected-ci-final.log`): formatting,
clippy, HEIF, workspace tests (774 UI tests passed, 2 ignored), parity, layers, assets,
and WASM. The final popup-only lint pass also covers the local-library icon correction.

## 2026-10-16: guarded whole-cloud-library erasure (CUX.8)

The native signed loopback fixture covers both sync protocols, exact typed consent,
fresh-review rejection, historical versions and delete markers, unfinished multipart
uploads, foreign-data rejection, mixed head/append publication authorities, late writes,
interrupted retries and terminal upload refusal after restoring a catalog without its
local erasure marker. An empty existing bucket is probed without writes and initialized
on its first synchronization. These checks use procedural media and disposable storage;
they are not live AWS/B2 deletion acceptance.

The real desktop app passed the separate opt-in control-channel acceptance test:
an incorrect phrase caused no deletion, the exact phrase erased the fixture, and the
popup reported a deleted library with zero cloud original bytes and a Details action.
Reopening Details preserved the completed state. The review, typed confirmation,
completion, popup and reopened-details screenshots under
`target/cloud-library-delete-acceptance/native-*.png` were visually inspected, with no
headless fallback. Local catalog/photos and the bucket remained. The test closed its
isolated app afterward. Headless UI regressions also cover dismissal clearing consent.

All eight stages of `cargo +1.95.0 xtask ci` passed
(`target/cloud-library-delete-ci-verified.log`), including 777 UI tests, parity, layers,
asset attribution and WASM. Final native build and presented-window acceptance logs are
`target/cloud-library-delete-native-verified-build.log` and
`target/cloud-library-delete-native-verified.log`. The recovery-list regression now
scrolls the outer dialog to verify its footer remains reachable after adding the action;
it retains conflict-choice and bounded-list assertions.

## 2026-10-16: whole-library native cloud mode (CUX.9)

Native cloud mode now discovers added file-backed originals and stages them through
durable per-photo adoption requests. Existing and later imports retain IDs, edits,
virtual-copy relationships and local files. Browsed Local-folder records are excluded
until added. Automatic adoption does not insert user Undo steps. Preparation failures
stay visible, healthy photos continue, and Resume retries failed preparation.

Focused engine checks cover reopening, later imports, pause, virtual-copy sharing,
missing-original isolation and retry without duplicate records. The opt-in presented
desktop test drives both protocols against the signed disposable S3 fixture. It checks
automatic upload after enablement, byte-verified reuse without PUT of an existing
original, later import while paused, upload after Resume, and absence of the duplicate
popup settings action. Procedural-photo popup screenshots under
`target/cloud-whole-library-acceptance/` were visually inspected. These small fixtures
are not live-provider or large-library performance acceptance. CLI/MCP one-shot commands
still require explicit adoption/upload; browser original uploads remain unsupported.

All eight stages of `cargo +1.95.0 xtask ci` passed
(`target/cloud-whole-library-ci.log`), including 777 UI tests and the new engine
regressions. The final native build and both-protocol desktop acceptance passed
(`target/cloud-whole-library-native-final-build.log`,
`target/cloud-whole-library-native-final.log`). The desktop recheck was run after CI
released the Windows CLI executable lock. Presented-window screenshots and performance
inspection are retained per protocol in the artifact folder; no headless fallback was used.
