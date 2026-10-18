# Cloud acceptance history

Last updated: 2026-10-18

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

## 2026-10-17: batched enrollment and focused cloud controls (CUX.12)

The deterministic engine regression holds the preparation coordinator and verifies one
worker reports 0 of 20 photos immediately, with all 20 represented by cloud.overview.
All 40 import regressions passed, including missing-original isolation and recovery:
partial automatic batches retain their original request identity instead of stranding
old failures when healthy siblings have already been adopted.

Shared native/browser cadence tests cover 60-second active scans, the strict greater-than
60-second inactivity boundary, 15-minute idle scans, renewed activity, and immediate
explicit requests. The full workspace CI run passed all eight gates
(`target/cloud-cux12-ci.log`). Final popup tests passed after the submenu width adjustment
(`target/cloud-cux12-popup-ready.log`); their screenshots in `target/cloud-cux12-ui-final/`
were inspected, including preparation, conflict routing and guarded deletion via More.

Presented native acceptance passed in 19.49 seconds against the independent signed
loopback fixture for both conditional-head and append-log protocols
(`target/cloud-cux12-native-acceptance.log`). Each protocol uploaded 20 initial photos,
reused an already-remote original without PUT, preserved pause behavior, and uploaded a
later import after Resume. No DELETE requests or signature errors occurred. The final
native build is recorded in `target/cloud-cux12-native-ready.log`.

Presented screenshots and ui.inspect results in `target/cloud-cux12-native/` were
inspected. Both runs used DX12 on NVIDIA RTX 4000 Ada, with no GPU fallback and empty
render queues. Observed frame times were 1.67/2.40 ms; full update times were 42.77/57.12 ms
(these acceptance snapshots are not a performance benchmark). The tests used disposable
procedural photos and storage; no real B2 bucket or personal library was modified.

## 2026-10-17: CR.58 — Local background media revisions

The local-only engine regression covers all four operations and restart. Presented native control-channel acceptance covers asynchronous XMP, external copy and reload, asserting zero extra network requests and unchanged cloud objects. The local-only Info screenshot was inspected. Evidence: `target/cloud-cr58-engine-recheck.log`, `target/cloud-cr58-native-acceptance.log`, `target/cloud-cr58-native/toggle/off-info.png`.

All eight final-source CI gates passed (`target/cloud-review-cr58-cr62-ci-final.log`).
This run used disposable local fixtures; fresh live AWS/B2 acceptance remains open.

## 2026-10-17: CR.59 — Original-owner conversion certification

The real-CLI signed fixture passes both protocols after another device compacts receipts, including a remote write whose local acknowledgement was lost. Conversion clears the exact retained pending ID, receives the newer rating, and performs only GET/HEAD with unchanged remote objects. The fingerprint regression distinguishes publication position from proof transport. Evidence: `target/cloud-cr59-compaction-ack.log` and `target/cloud-cr59-stability.log`.

All eight final-source CI gates passed (`target/cloud-review-cr58-cr62-ci-final.log`).
This run used disposable local fixtures; fresh live AWS/B2 acceptance remains open.

## 2026-10-17: CR.62 — Stable absolute local-original destinations

The real-CLI regression converts `./photos`, verifies absolute destinations in preview and the durable mode record, checks the original bytes, and reopens from another working directory without network requests. Evidence: `target/cloud-cr59-cr62-cli.log` and the final workspace CI run.

All eight final-source CI gates passed (`target/cloud-review-cr58-cr62-ci-final.log`).
This run used disposable local fixtures; fresh live AWS/B2 acceptance remains open.

## 2026-10-17: CR.61 — Incremental append discovery

A shared-planner regression catches up through 300 sequential 256 KiB replacements (about 75 MiB cumulative payload), with a second author arriving before its dependency. Ready staging returns to zero, both frontiers advance and no gaps remain. Native workspace tests and the browser WASM build cover both adapters; browser runtime and live-provider execution were not repeated. Evidence: `target/cloud-cr61-planner.log`.

All eight final-source CI gates passed (`target/cloud-review-cr58-cr62-ci-final.log`).
This run used disposable local fixtures; fresh live AWS/B2 acceptance remains open.

## 2026-10-17: CR.60 — Permanent-local editing and causal rejoin

All 44 focused local/revision tests passed, including 10,001 local edits, unchanged original cloud outbox, restart, undo/redo, original pending IDs, remote concurrency, baseline/edit atomicity and rejoin limit refusal. Real-CLI rejoin passes both protocols with a new local photo and concurrent ratings (target/cloud-cr60-cli-rejoin-tests.log). Native rejoin automatically uploads final revisions and the added photo, with zero network requests while local-only; its screenshot was inspected (target/cloud-cr60-native-rejoin.log, target/cloud-cr60-native/local-rejoin/rejoined.png). The original conversion native check passed a retry and three further repetitions after one unexplained control connection reset; command/exit diagnostics were added, but the initial reset cause was not established. Inspection sample: frame 1.43 ms, logic 30.78 ms, last render 4.72 ms; no large-library performance claim.

All eight final-source CI gates passed (`target/cloud-review-cr60-ci.log`).
This run used disposable local fixtures; fresh live AWS/B2 acceptance remains open.

## 2026-10-17: CR.49 — Durable successor exposure

Thirteen shared control tests and ten real-CLI upgrade tests pass, including both orders of cancellation/output-authorization CAS, takeover, v1 unknown exposure, failure before manifest, after manifest and after the complete checkpoint seed but before retirement, interruption of rebase output, a lost exposure-CAS response, unchanged remote objects on refusal, retained pending IDs, retry completion and typed update recovery. Native control-channel acceptance refuses cancellation after seed publication, preserves every remote object and finishes on retry; refusal and completion screenshots were inspected. Evidence: target/cloud-cr49-control.log, target/cloud-cr49-upgrade-final.log, target/cloud-cr49-native-acceptance.log and target/cloud-cr49-native/upgrade-exposure/. Sample inspection: frame 0.93 ms, logic 31.00 ms, last render 7.93 ms; no large-library performance claim. All eight final-source CI gates pass (target/cloud-review-cr49-ci.log). Browser WASM compilation is covered; fresh browser runtime and live AWS/B2 acceptance remain CR.45.

Earlier cancellation-after-seed acceptance describes stopping a local Activity worker. It does not authorize returning the remote head to Active after output may exist.

## 2026-10-17: CR.53 — Safe cache relocation cleanup

All 26 cache-focused tests pass, covering outstanding old byte leases through reopen, removal after release, exact replacement hash/size, destination fallback, damaged/changed retention settings, durable selection rollback, immediate reuse of the old root in the same session and after dropping/reopening the library, and stale maintenance/policy against a current lease. Native control-channel acceptance moves a pinned original and preview, checks original bytes and offline ownership, and verifies the selected folder in Cache settings; the screenshot was inspected. Evidence: target/cloud-cr53-cache-tests-final.log, target/cloud-cr53-native-acceptance.log, target/cloud-cr53-native/cache-relocation/. Sample inspection: frame 1.31 ms, logic 32.94 ms, last render 5.55 ms; no large-library performance claim. All eight final-source CI gates pass (target/cloud-review-cr53-ci.log). These checks use disposable local fixtures, not live AWS/B2 data.

## 2026-10-17: CR.63 — Library-wide cloud diagnostics

All 17 focused cloud-status engine tests, nine popup tests and the halted-popup Details regression pass, including an empty library with authentication, integrity, capacity, newer-reader and disk failures, retry snapshots, future classifications, exact diagnostic visibility and cause-specific repair navigation. Native control-channel acceptance starts a real configured empty library against the disposable signed S3 fixture, observes refused credentials, opens Details, checks its public diagnostic, opens Edit connection, and recovers to healthy after service access is restored. Evidence: target/cloud-cr63-engine.log, target/cloud-cr63-ui.log, target/cloud-cr63-halted-ui.log, target/cloud-cr63-native-acceptance.log and target/cloud-cr63-native/empty-error/. The native screenshot was inspected. Inspection sample: frame 0.98 ms, logic 0.05 ms, no photo render; no large-library performance claim. All eight final-source CI gates pass (target/cloud-review-cr63-ci.log). These are disposable loopback fixtures; fresh browser runtime and live AWS/B2 acceptance remain CR.45.

## 2026-10-17: CR.64 — Explicit cloud authentication source

All nine focused setup tests pass, including saved-key/profile replacement, inactive credentials, explicit environment selection, missing/partial replacement refusal, stable saved binding, source-change invalidation and refusal to save an untested or stale source. Native control-channel acceptance configures expired managed credentials in an isolated fixture file, explicitly selects the valid acceptance profile, probes it, saves and verifies the persisted profile. Requests are read-only and signature errors remain zero; the expired credentials cannot pass the fixture. The screenshot was inspected. Evidence: target/cloud-cr64-ui.log, target/cloud-cr64-native-acceptance.log and target/cloud-cr64-native/profile-replacement/. Inspection sample: frame 0.57 ms, logic 0.40 ms, no photo render; no large-library performance claim. All eight final-source CI gates pass (target/cloud-review-cr64-ci.log). Real user credential files and remote libraries are untouched. Fresh browser runtime and live-provider acceptance remain CR.45.

## 2026-10-17: CR.65 — Complete recovery navigation

All 15 focused engine recovery tests, 39 existing cloud UI tests and four new navigation tests pass. Real retained state reaches conflict 17, value nine and deleted photo 201 without changing the catalog or earlier records; search finds text beyond the 80-character preview. UI tests cover those boundaries, structural conflict 17, Info choice nine, independent Info/dialog search, Previous, query caching and revision/library reset. Native control-channel acceptance browses conflict 17/value nine and searches deleted photo 201 in a disposable real library, checking unchanged catalog revision, retained state and remote objects. Both native screenshots were inspected; performance inspection is saved with the artifacts, without a large-library performance claim. Evidence: target/cloud-cr65-engine-final.log, target/cloud-cr65-ui-navigation-final.log, target/cloud-cr65-native-acceptance-final.log and target/cloud-cr65-native/recovery-pages/. All eight final-source CI gates pass (target/cloud-review-cr65-ci.log). Browser WASM compilation is covered; fresh browser runtime and live AWS/B2 acceptance remain CR.45.

## 2026-10-17: CR.66 — Shared cloud actions

Three engine action tests and 46 cloud UI tests pass. They compare Info/popup commands, disabled draining, typed recovery and migration states. A real paused Session first rejects ordinary cloud.sync, then successfully executes the shared Resume action. Native control-channel acceptance starts with a positively asserted waiting upload, clicks Resume in managed-photo Info, waits for upload and publication to finish, and verifies exact remote bytes and zero signature errors. Both native screenshots were inspected and ui.inspect/perf saved. Evidence: target/cloud-cr66-engine-final.log, target/cloud-cr66-ui-final.log, target/cloud-cr66-native-acceptance-final.log and target/cloud-cr66-native/info-resume/. All eight CI gates pass on the final source (target/cloud-review-cr66-ci.log). WASM compilation is covered; fresh browser runtime and live AWS/B2 acceptance remain CR.45.

## 2026-10-17: CR.67 — Fresh retention consent

All 48 cloud UI tests pass, including four new retention lifecycle tests. They exercise Done, Escape, control cancel/confirm, replacement, explicit reopen, successful/failed Resume on engine and intercepted routes, completion with the view closed, and same-identity library-generation invalidation. An eligible audit cannot Apply after reopening until the checkbox is checked again; Apply consumes the assertion. The native signed-loopback acceptance waits for actual consent state before subsequent commands and screenshots, then verifies checked-before-close, unchecked-after-reopen and unchecked-after-Resume. All three screenshots were inspected; perf inspection is saved. Zero storage DELETE requests and zero signature errors are asserted. Evidence: target/cloud-cr67-ui-final.log, target/cloud-cr67-native-acceptance-final.log and target/cloud-cr67-native/retention-consent/. All eight final-source CI gates pass (target/cloud-review-cr67-ci-bounded-d.log). WASM compilation is covered; fresh browser runtime and live AWS/B2 acceptance remain CR.45. Validation used RUST_TEST_THREADS=2 and a workspace-local TEMP/TMP directory after earlier full runs exhausted system commit memory and the system temporary volume; test assertions and gates were unchanged.

## 2026-10-17: CR.68 — Shared dialog confirmation lifecycle

All 57 focused cloud UI tests pass. Full setup is exercised by widget and control confirmation, including no duplicate probe, read-only signed fixture checks and identical final configure/enable/sync commands. Additional tests cover asynchronous HTTP 403, missing/invalid settings, invalid library choice without key saving, final settings-write failure, CloudStorage/Connect/Migration/Import error retention, replacement dialogs and stale migration confirmation after a library switch. Recovery navigation explicitly reopens after the old library view is dismissed and verifies reset search/cursors. The native signed-loopback acceptance uses real UI frame barriers, preserves a missing-profile form, corrects it, creates an append library, verifies the saved profile and enabled state, waits for publication to settle, and exits cleanly within a deadline. Zero signature errors and no DELETE requests are asserted. Both native screenshots were inspected; frame 0.49 ms and logic 0.03 ms are a sample without photo rendering, not a large-library claim. Evidence: target/cloud-cr68-ui-final.log, target/cloud-cr68-native-acceptance-final.log and target/cloud-cr68-native/dialog-lifecycle/. All eight final-source CI gates pass (target/cloud-review-cr68-ci.log). Native fixtures use EXAMPLE credentials; browser compilation is covered, while fresh browser runtime and live AWS/B2 acceptance remain CR.45.

## 2026-10-17: CR.69 — Managed media preparation and catalog plans

Two new subsystem tests pass: ordinary add/revision plans work without SyncState, cloud identities or upload intents, preserve virtual-copy identities and ratings, and reject invalid working paths and stale revisions without applying media to the owner catalog. All 41 existing import/adoption regressions pass, including local-only background XMP/DNG/external/reload work, supporting XMP, identity/undo/restart, journal failures, abandoned workers, bulk recovery and source changes during adoption. The 17 real-CLI append tests also pass, including DNG/XMP/external revisions and durable publication dependency/acknowledgement behavior; conversion/rejoin and conditional-head integration tests remain green. Evidence: target/cloud-cr69-regressions.txt and target/cloud-review-cr69-ci.log. Strict focused engine Clippy passes (target/cloud-cr69-clippy.log). All eight final-source CI gates pass. This is an internal extraction with unchanged UI; no additional native visual acceptance is claimed. Fresh browser execution and live AWS/B2 acceptance remain CR.45.

## 2026-10-17: CR.70 — Typed cloud failures through transport and scheduling

Signed loopback S3-to-CLI health tests cover HTTP 401 and 403, exact Edit connection recovery, newer-reader Update recovery, disabled automatic retries and unchanged pending edits/catalog on refusal. Shared planner regression reaches the actual 64 MiB unresolved-dependency bound, preserves the remote projection and reports Review capacity through PollHealth; catalog, publication-order and exact entity-boundary tests preserve typed capacity. A constructed Head scope test checks its typed API separately from early wire decoding. Native empty-library Details acceptance verifies actual HTTP 403 reaches the diagnostic, opens the connection editor and recovers after the fixture accepts requests; screenshots are inspected. All eight CI gates pass, with final formatter verification after the browser-only startup guard adjustment. Evidence: target/cloud-review-cr70-ci-final.log, target/cloud-cr70-native-acceptance.log and target/cloud-cr70-native/empty-error/details.png. No real provider credentials or user cloud data were used; fresh browser runtime/live AWS/B2 acceptance remains CR.45. Native acceptance passed in 4.82 s. Empty-library sample: frame 0.77 ms, logic 0.05 ms, no image renders; this is not a large-library performance claim.

## 2026-10-17: CR.71 — Precise shared cloud status and manual checks

Strict native Clippy passes. Shared-model and action regressions cover first-check, successful scan, pending work, explicit pause, retrying with exact HTTP cause, read-only publication, local destination, erasure and upload diagnostics. UI tests exercise actual popup and Info controls, identical cloud.sync {wait:false} dispatch and disabled scans, plus shared paused/read-only/local labels. Existing dialog assertions were updated to the new wording, including negative Up to date assertions under failures; retrying retains the warning icon. All eight CI gates pass (target/cloud-review-cr71-ci-final.log). After correcting only the ignored native harness to reopen a dismissed popup and wait for layout, final workspace formatter, strict all-target Clippy and test compilation pass again. Native popup_sync_now_checks_the_library_and_pause_resume_stays_explicit passes in 5.36 s: an actual signed head GET follows Sync now, Pause changes device state, the reopened popup shows Resume, and resumption finishes before bounded exit. Native empty-library error Details acceptance also passes in 2.59 s. Evidence: target/cloud-cr71-native-acceptance.log, target/cloud-cr71-native-error-acceptance.log, target/cloud-cr71-native-clippy.log and target/cloud-cr71-native-tests-build.log. Inspected native screenshots: target/cloud-cr71-native/precise-status/up-to-date.png and paused.png, plus the earlier headless healthy capture. ui.inspect/perf in the empty diagnostic view sampled frame 0.87 ms, logic 0.05 ms, max startup/update 19.47 ms and no image renders; this is not a large-library/image-render performance claim. Fresh browser execution and live AWS/B2 acceptance remain CR.45.

## 2026-10-17: CR.72 — Read-only retained-value inspection and exact edit comparison

Engine regressions extend the real 17-conflict/9-choice/201-deleted recovery fixture with complete value retrieval, exact photo/writer/sequence information, hostile/stale request rejection and unchanged registers/outbox/catalog revision. Exact-render regression verifies retained crop/orientation, source level, distinct temporary key, unchanged current hash cache and preserved ordinary Before alignment. A library-switch regression covers reused photo IDs. Seven focused conflict UI regressions pass, including actual full-value queries and rendered comparison, unchanged catalog/history/outbox/selection/Before state, context-change cancellation including late and failed render cleanup, small-window Done visibility, library replacement, paging and distinct Info/dialog controls. All eight CI gates pass (target/cloud-review-cr72-ci-geometry.log). Supplemental current-source formatting, strict all-target workspace Clippy, workspace test compilation and the full UI suite pass in target/cloud-cr72-current-{fmt,clippy,tests-build,ui}.log. The new raw-preview regression asserts an unchanged catalog after an inspection decode, then proves the ordinary view still learns white balance. The crop-density regression crosses the full-resolution boundary only after a 90-degree orientation. Native retained_values_show_full_metadata_and_independent_previews_without_resolving passes in 7.72 s (target/cloud-cr72-native-geometry-acceptance.log) using two real signed loopback publishers and normal metadata/develop publication. Full captions, Preview, Compare and Done leave conflict rows/catalog revision and remote objects unchanged. Inspected native captures: target/cloud-cr72-native/retained-values/details.png and compare.png; each edit visibly uses its own geometry. Native inspect sampled frame 1.59 ms, logic 0.06 ms, max update/startup 60.18 ms, nine completed renders and no pending slots. This small 32x24 fixture does not establish large-library or raw-render performance. Fresh browser execution and live provider acceptance remain CR.45.

## 2026-10-17: CR.73 — Preserve Recently Deleted originals during local conversion

Eight focused permanent-transfer regressions, the fresh-reader unchanged-tombstone/withdrawn-file regression and five transition UI regressions pass. Four CLI cloud-toggle tests pass, including a new end-to-end test on both conditional-head and append-log: publish an original plus immutable XMP, delete it, join from a fresh reader with zero local photo records, review the exact deleted count/byte union, convert, verify every permanent blob by SHA-256/length, remove only fixture-owned disposable cache/staging, then select, restore and export the original offline with zero additional requests and unchanged cloud objects. Shared virtual copies and concurrent content alternatives are included and deduplicated correctly. Native cloud_toggle_native_confirmation_blocking_progress_and_durable_completion passes in 11.41 s (target/cloud-cr73-native-acceptance.log), exercising the real confirmation, held download, blocking navigation, verified completion, permanent files, local background edits and unchanged remote objects. Inspected target/cloud-cr73-native/toggle/confirmation.png: count, additional bytes, permanent destination and actions fit. Native inspect sampled frame 1.73 ms, logic 28.00 ms, max update 29.73 ms with nine completed renders and no pending slots; this tiny fixture does not establish scale performance. Initial full CI found two unrelated fixture failures: a PID-only upload-test directory reopened stale data, corrected with a unique directory and explicit cleanup; a simulated preview-drive removal returned Windows DirectoryNotEmpty and passed independently without production changes. All eight CI gates pass in target/cloud-review-cr73-ci-final.log, including both previously failing fixtures in the full run. Fresh browser and live-provider acceptance remain CR.45.

## 2026-10-17: CR.74 — Explain local-only cloud re-enablement as a merge

All eight CI gates pass (target/cloud-review-cr74-ci.log), including 814 UI tests with no failures and the new mode/baseline re-enable regression. It tests untouched local-only mode, local-only mode with a durable local-edit baseline and active synchronization with a baseline; the two local-only cases show merge/conflict/permanent-copy copy and confirm the exact bound library, connection and protocol, while the active case opens the popup without enable/sync calls. Existing both-protocol local_edits_new_photos_and_concurrent_remote_values_survive_rejoin_on_both_protocols passes in this full run. Initial focused test setup incorrectly expected CloudConnect for an active library; corrected its assertion to the existing popup contract without changing production routing. Native local_only_rejoin_uploads_final_revisions_and_new_local_photos passes in 6.36 s (target/cloud-cr74-native-acceptance.log): local revisions and a new import make zero requests while sync is off, real dialog confirmation re-enables the same library/connection, both photos become synchronized managed content, the rejoin baseline clears, superseded work is retained correctly and the original permanent blob remains byte-identical. Inspected target/cloud-cr74-native/local-rejoin/local-only-connect.png and rejoined.png. Native inspect sampled frame 2.03 ms, logic 42.88 ms, max update/startup 54.16 ms, 70 completed renders and no pending slots; this two-photo fixture does not establish large-library performance. Fresh browser and live-provider acceptance remain CR.45.

## 2026-10-17: CR.48 — Refuse unsupported append epoch namespaces

All eight cargo +1.95.0 xtask ci gates passed (target/cloud-review-cr48-ci.log), including strict Clippy, 1,099 engine tests and all 11 signed CLI upgrade regressions. The namespace regression covers readable Active epoch two plus conditional roots at epochs one and two: zero extra PUTs of any type, unchanged objects/catalog/discovery state and exact pending mutations; the final ordinary epoch-one retry still publishes. Shared tests verify native AssetError and browser direct Blocked conversion both retain Unsupported/Update and are nonretryable. The current workspace build passed (target/cloud-cr48-native-build.log). Explicit native control-channel acceptance passed against that binary (target/cloud-cr48-native-acceptance.log): open popup and Details, retain exact pending edits/rating, no remote object changes or PUTs, zero signature failures, clean quit/reopen. Both screenshots were inspected in target/cloud-cr48-native/unsupported-append-epoch. Details reports Unsupported/Update, no scheduled retry. The tiny procedural fixture inspection recorded 1.26 ms frame, 38.50 ms update and 6.67 ms last render; these are observations, not large-library performance acceptance. Initial preflight compilation ran out of available virtual memory; subsequent complete CI and runtime checks succeeded with one compiler worker, without changes to unrelated processes. Epoch routing/exclusive maintenance remains CR.43; this is a fail-closed guard.

## 2026-10-17: CR.51 — renew and fence ownership throughout conditional-head upgrades

All eight `cargo +1.95.0 xtask ci` gates passed (`target/cloud-cr51-ci-rebuilt.log`), including strict workspace Clippy, 1105 engine tests and all 12 signed CLI upgrade tests. The new activation regression first failed with rating 0 at the concurrent edit's cursor (`target/cloud-cr51-activation-before.log`), then passed for both a normal and lost activation response (`target/cloud-cr51-upgrade-tests.log`). Two shared-policy and four native-supervisor regressions cover multiple injectable-clock lease intervals, takeover after renewal stops, stale activation, private cancellation provenance, blocked renewal cancellation and unwind cleanup (`target/cloud-cr51-{policy,supervisor}-tests-final.log`). A stale incremental engine-test link cache was isolated and rebuilt; no unrelated source changes were made for that tooling failure. Native workspace binaries and the development browser bundle built (`target/cloud-cr51-{native,web}-build.log`). Actual native app upgrade/cancellation acceptance passed; refusal/completion PNGs and `ui.inspect` were inspected under `target/cloud-cr51-native/upgrade-exposure/`. Actual Chromium acceptance passed healthy renewal, takeover and cancellation on both OPFS and IndexedDB (`target/cloud-cr51-browser/acceptance-durable.log`, `results.json`). Each healthy case confirmed three renewals through 1.5 simulated lease intervals while a manifest PUT was held. Every case replayed copies of its durable browser snapshot/log through the Rust catalog loader and retained the exact pending mutation map plus rating 4; failed workers emitted no successor checkpoint and did not retire the old authority. Takeover remained contention and user cancellation remained cancelled. All 787 recorded requests across the owned fixture's seeding/acceptance history had valid independently checked SigV4 signatures (`fixture-summary.json`). Popup screenshots were inspected; sampled browser frames/updates were 1-3 ms and the native sample was frame 1.20 ms/update 31.67 ms/render 4.67 ms. These small-fixture observations are not scale/performance or live-provider acceptance; live AWS/B2 validation remains CR.45. Browser harness startup MIME and scheduling/ID assumptions were corrected in ignored test tooling before the accepted run. Existing WASM warnings remain; the changed cloud files introduced no warnings.

## 2026-10-17: CR.50 — Retain typed publication capability on every outcome

All eight cargo +1.95.0 xtask ci gates pass (target/cloud-cr50-ci-final.log). The signed clean-client regression failed before the fix (target/cloud-cr50-before.log) and all 13 signed CLI upgrade tests pass afterward (target/cloud-cr50-upgrade-tests.log). It checks repeated unchanged scans under maintenance, newer-writer and retirement, exact catalog.snap/catalog.log bytes, no remote writes and no signature errors. Outcome tests cover all variants and recertification; the UI regression covers independent publication/polling Details and safe legacy fallback. Actual native app acceptance passes all three causes and captures popup/Details plus typed recovery (target/cloud-cr50-native-acceptance-final.log; target/cloud-cr50-native/). Chromium acceptance passes the same three cases on both OPFS and IndexedDB, replays copied durable catalogs with the Rust loader, compares exact before/after digests and checks zero remote writes (target/cloud-cr50-browser/acceptance.log; results.json). Native binaries and browser WASM bundle build; popup and Details screenshots were inspected. These are signed loopback fixture checks; live AWS/B2 CR.45 remains open.

## 2026-10-17: CR.54 — Isolate saved credential bindings and coordinate saves

All eight cargo +1.95.0 xtask ci gates pass (target/cloud-cr54-ci.log). The independent-form regression failed before the fix because both forms selected lightcraft-b2 (target/cloud-cr54-before.log). Ten S3 credential tests and nine setup tests pass (target/cloud-cr54-credential-tests.log; target/cloud-cr54-setup-tests.log), including exact saved keys, rotation, concurrent saves, busy refusal without writes, growth bounds and missing-binding refusal. Actual native wizard acceptance passes two separate libraries sharing the default connection name and one scoped credentials file, with independently verified SigV4 for different fake keys; rotating A leaves A's old and B's profiles intact, reopening B keeps Saved keys, and a fully probed retarget is refused with byte-for-byte unchanged credentials/config binding (target/cloud-cr54-native-acceptance-final.log; target/cloud-cr54-native/isolated-credentials/). Screenshots were inspected, including the retained rejected form and reopened Saved keys. Native workspace/all-target and WASM web checks plus native app/test builds pass. The first native harness omitted its explicit initial sync before retarget setup; corrected test setup passes. Tests use scoped fake credentials and do not write the real AWS credentials file.

## 2026-10-17: CR.52 — Recover browser cache transactions and durable offline protections

All eight cargo +1.95.0 xtask ci gates pass (target/cloud-cr52-ci.log). The actual pre-fix browser regression reported zero bytes for a 28-byte verified orphan on both OPFS and IndexedDB (target/cloud-cr52-before.log); fixed orphan accounting reports 28 on both (target/cloud-cr52-orphan.log). Three pure recovery tests and the in-flight/failed-flush fence regression pass, covering current source replacement, shared and empty owners, malformed/missing/future inputs, orphan adoption and phantom removal. Actual signed-browser acceptance passes both backends (target/cloud-cr52-browser-acceptance-final.log; target/cloud-cr52-browser/results.json): a real verified download commits before injected policy failure, a real delete commits before replacement-index failure, reload recovers exact usage/readiness, stale closed-library membership is recomputed, shared and newly resolved pins survive Clear, low-space admission evicts only the verified unpinned orphan, held OPFS/failed IDB metadata saves defer Clear until retry, and missing/future policy or future closed-library intent retain copies and refuse cleanup. Final physical/index usage is 21,143 bytes with offlineReady true on both. No page errors; popup screenshots and ui.inspect performance were inspected. Native workspace tests and the WASM web build/check pass; the web crate adds no warnings. Browser tests use isolated contexts and independently verified fake SigV4 credentials, without real cloud mutations.

## 2026-10-17: CR.55 — Validate complete guided cloud library discovery

All eight cargo +1.95.0 xtask ci gates pass (target/cloud-cr55-ci-final.log). Six of the original seven signed-counterparty regressions failed against the old scanner, demonstrating omitted pages, partial success, unsupported folders, duplicate protocol choices and unchecked controls (target/cloud-cr55-before.log). All eight discovery tests pass, including cancellation and a 1,025-library bound that refuses before manifest reads (target/cloud-cr55-discovery-tests-final.log). All nine setup tests pass, with fixtures supplying a supported manifest/control listing (target/cloud-cr55-setup-tests.log). Actual native guided acceptance passes paginated three-library discovery, one append choice per retired-head successor, and a malformed-manifest error retaining the editable form with no join choices (target/cloud-cr55-native-acceptance.log; target/cloud-cr55-native/discovery/). Its requests are independently SigV4 verified, GET-only, and leave every remote object unchanged. Screenshots and ui.inspect/performance were inspected. The first native harness expected a Test Connection button in the failure state; corrected retained-form expectation and a frame barrier pass. Initial strict CI caught two redundant string conversions in the new test; corrected final CI passes. The actual CLI read-only probe also succeeds against authorized dykstrap-lightcraft using saved lightcraft-b2 and a fresh empty prefix, returning no libraries/head with no cloud writes (target/cloud-cr55-b2-live/result.json).


### 2026-10-18 — CR.43 partial: native B2 revision CAS

Nine pure/loopback regressions pass, covering scoped canonical pointers, capacity, credentials/API endpoint refusal, one winner from competing revisions, unchanged unrelated metadata, lost/unconfirmed responses, redirects/authentication/cancellation, malformed/oversized success bodies and permission/scope refusal before writes and catalog-readable library identities. All eight workspace CI gates pass (target/cloud-cr43-primitive-ci-accepted.log). The explicit ignored Rust live CAS test passes against authorized disposable dykstrap-lightcraft using saved lightcraft-b2 (target/cloud-cr43-native-live-cas-accepted.log): two contenders on one revision yield one success and one conflict; the committed pointer is verified; cleanup rereads and removes only its generated entry plus the two exact probe object versions; all unrelated bucket info, type, CORS, lifecycle/encryption/lock/replication settings remain unchanged. A prior live attempt was refused before remote output by the append-only namespace validator; the narrow B2 staging helper fixes that without weakening append validation. CI caught one nested-if style warning in the new adapter; the corrected final run passes. This test validates the provider primitive, not the desktop/browser upgrade workflow or live AWS interoperability.


### 2026-10-18 — CR.43 partial: native B2 head transitions and legacy intake

Eighteen B2 pure/loopback tests plus three legacy model tests and the retired-source fence regression pass. Readable future-writer and unsupported-converter transitions refuse authority modification, and newer descriptor/pointer formats retain Update recovery. The explicit ignored Rust adapter test passes on the authorized disposable dykstrap-lightcraft bucket using saved lightcraft-b2: initial exposed maintenance, epoch-2 activation, late existing-author and previously unseen-author append operations with original IDs/digests, committed checkpoint/intake reread without duplicate effects, historical audit rejection of a conflicting operation behind its author cursor, permanent nonpersonal deletion and rejected revival. Its fresh random prefix and exact tracked object versions plus its own bucket-info entry are cleaned; unrelated bucket info/type/CORS/lifecycle/encryption/lock/replication settings are checked unchanged. Evidence: target/cloud-cr43-head-adapter-live-final.log. All eight CI gates pass at target/cloud-cr43-head-adapter-ci-final.log. An earlier opt-in test invocation omitted its bucket environment and refused before remote I/O; an earlier successful live run covered late-author recovery before the explicit historical-audit case. This does not certify the desktop/browser upgrade workflows or live AWS interoperability.
