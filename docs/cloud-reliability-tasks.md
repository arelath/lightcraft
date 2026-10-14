# Cloud storage reliability tasks

Source: supplied static review of `cloud-support` at `208edb8`.
Created: 2026-10-14. Check an item only after implementation and relevant verification;
record evidence and remaining limitations below it. Historical acceptance does not verify
this checkout. Work in the order below, preserving unrelated changes.

- [x] CR.1 — Restore synchronization liveness with failed, deleted, orphaned, or other-connection uploads. Receive independently of publication; gate publication on exact media dependencies. Preserve append causal continuity by publishing frozen imports before tombstones. Test deletion before upload and unrelated failed uploads.
- [x] CR.2 — Fix automatic multi-photo append imports. Represent dependency waiting separately from failure and reconcile every upload intent covered by verified publication. Test delayed sibling transfers and restart/retry.
- [x] CR.3 — Materialize valid folder rearrangements independently of entity ID order. Validate the desired graph, detach before attaching, and resolve concurrent cycles deterministically while retaining conflict choices. Test reversed ancestry and concurrent cycles; audit stack uniqueness.
- [x] CR.4 — Bound import replica identities. Route all current imports through normal causal mutations; reject the obsolete synthetic-author format across an explicit pre-release version boundary. Test more than 4,096 managed photos without widening the device limit. The maintainer confirms there are no users or existing libraries to migrate, only disposable test data (2026-10-14).
- [x] CR.5 — Specify and implement causal-history compaction, including retired replicas, tombstones, duplicate detection, and old offline clients. Test checkpoint maintenance restores editing capacity without losing conflict or deletion safety.
- [x] CR.6 — Expose synchronization-state utilization, early warnings, and accurate recovery errors before record/byte bounds.
- [x] CR.7 — Define and test oversized-entity bootstrap behavior, especially large album membership sets; implement streaming or an explicit actionable product limit.
- [x] CR.8 — Make unchanged conditional-head synchronization a genuine local no-op (no materialization, state operation, or journal write without change, acknowledgement, or progress).
- [ ] CR.9 — Index global-to-local identities once, materialize changed entities, and persist synchronization deltas. Measure unchanged 10,000- and 50,000-photo request counts, journal growth, state size, and owner-thread latency.
- [ ] CR.10 — Track consumed append checkpoints and avoid downloading unchanged covered history. Separate budgeted integrity audits from ordinary polls; test remote request counts.
- [ ] CR.11 — Enforce one immutable published asset-manifest contract with device-local connection binding; test permitted/rejected descriptor changes and extract shared blob rebinding.
- [ ] CR.12 — Replace subtractive catalog JSON projection/materialization with explicit portable photo, content, album, and reference types and typed boundary conversions; retain wire compatibility tests.
- [ ] CR.13 — Extract testable upload readiness, transfer, verification, dependency waiting, publication, acknowledgement, and failure transitions; test deletion/restart behavior.
- [ ] CR.14 — Consolidate current behavior/limitations, protocol specification, and dated acceptance evidence. Remove obsolete current-status claims and distinguish historical verification.
- [ ] CR.15 — Persist live offline owner-plus-quality policy, derive dependencies after membership/content changes, and allow empty albums to be pinned. Test new members and asset revisions.
- [ ] CR.16 — Add a command to move existing local photos into managed cloud storage while preserving photo IDs, virtual copies, edits, albums, and history. Test identity/history preservation and restart.
- [ ] CR.17 — Handle browser cache exhaustion with safe verified-blob eviction, catalog/outbox space reservation, and temporary viewing versus guaranteed offline retention. Test quota failures and pinned data preservation.
- [ ] CR.18 — Make cloud deletion retention prominent and add explicit previewable retention/reclamation tooling with safe reference checks.
- [ ] CR.19 — Align and expose native input, pixel, and blob limits; validate representative large `.3fr` inputs before migration and test actionable limit errors.
- [ ] CR.20 — Provide supported browser signer deployment instructions and a desktop/browser capability matrix; verify the deployment workflow and stated scope.
- [ ] CR.21 — Classify sync failures and retry transient connectivity with bounded backoff; pause authentication, integrity, schema, and identity failures with actionable status. Test native/browser recovery.

## Verification log

2026-10-14 — CR.1/CR.2: 11 append and 5 conditional-head real-CLI tests passed
(`target/cloud-reliability-tests.log`). Regressions cover soft/permanent deletion before
upload, receiving a remote rating while distinct media is missing, single-import jobs
sharing a bulk mutation, restart between sibling transfers, and exact reconciliation.
`objectsVerified` is the durable non-error publication-waiting state; no wire enum change.
Receipts retain conservative staging protection until exact proof is saved on disk too.

Native desktop acceptance on the rebuilt debug binary passed through its control channel:
permanent deletion before upload, automatic upload/sync recovery, peer rating arrival,
valid folder reversal, and automatic two-photo bulk upload. All four intents synchronized,
no pending mutations remained, and the deleted photo stayed absent from the rendered grid.
The loopback fixture independently checked all 181 signed requests. Inspected images and
performance output are recorded in `target/cloud-reliability/native-results.json` and
`target/cloud-reliability/native-8b410948/`. The sampled frame update was 1.20 ms and the
last render 6.07 ms on an RTX 4000 Ada laptop GPU; these tiny procedural photos are not
a large-library performance benchmark. This is fixture acceptance, not live B2/AWS evidence.

CR.3: folder reversal, concurrent cycles, stack ownership swaps/overlap/dissolution,
explicit resolution and conflict-cache regressions passed in the workspace engine tests.
Final rebuilt native acceptance passed for both protocols: 303 independently verified
signed append requests and 247 head requests. It exercised the rendered conflict dialog,
clicked Keep current structure through `ui.clickWidget`, then verified second-catalog
convergence, zero pending work and no structural conflicts. Both before/after screenshots
were inspected. Evidence: `target/cloud-reliability/native-results-append-log-v1.json`,
`native-results-conditional-head-v1.json`, `native-05ddafff/` and `native-9d510db6/`.
Sampled update time was 1.49/1.71 ms; last render 7.51/9.85 ms. These are small fixture
checks, not provider or scale benchmarks. The final-source project gate remains pending.

CR.4: conditional-head imports use the normal causal outbox and exact
publication acknowledgement, with version-4 device-local receipts. All uploads now require
explicit synchronization setup; native/browser legacy bridges and the standalone publisher
are removed. Obsolete pre-release heads, import batches/checkpoints and synthetic replicas
are rejected with a fresh-catalog/prefix recovery message, preserving local bytes. A
4,097-managed-photo causal bootstrap/peer test passed (31.90 s; one author on both peers,
portable state validated). This exercises the causal model/bootstrap, not 4,097 staged file
uploads; the import API intentionally takes at most 1,000 paths per call. All 33 focused
real-CLI tests passed (11 append, 7 head, 15 upload), including explicit setup, obsolete-head
rejection, receipt recovery, author clock and exact publication proof assertions.
Native final-source runs passed on both protocols, including automatic recovery after
enabling synchronization in a previously disabled session: 592 signed fixture requests
verified in total. Screenshots were inspected. Evidence: C: build `cr4-cli-tests.log`,
`cr4-scale-test.log`; `target/cloud-reliability/native-results-*.json`,
`setup-results-*.json`, `native-58f67a97/`, `native-ffd1eb5b/`, `native-2f971724/`,
`native-bc9b5761/`. The enable-restarts-attempted-work unit test also passed.
At CR.4 completion, history maintenance was still proposed; CR.5 below records its implementation. The contract is in [cloud-history-compaction-design.md](cloud-history-compaction-design.md).
The maintainer confirmed on 2026-10-14 that old libraries are disposable test data;
preserving their legacy format is no longer required. Fail closed on obsolete formats
instead of remapping receipt identities. Current-format offline devices retain their original clocks and pending IDs. Author-slot retirement remains unsupported; CR.5 uses conservative retention instead of an epoch cutover.

Full project gate passed on 2026-10-14: `cargo +1.95.0 xtask ci`, all eight stages
(fmt, clippy, HEIF, workspace tests, parity, layers, assets, WASM). Final log:
`C:/Users/pdykstra/AppData/Local/Temp/lightcraft-reliability-target/cr4-ci.log`.
The complete workspace run includes the new UI, structural, setup, scale and upload
interaction regressions. These changes were saved in `64ac4d07` alongside the completed
queue controls.

Earlier gate attempts: the initial run stopped
on a mechanical lint in concurrent popup work; a subsequent run found newly added
unformatted UI tests. Both were formatted/fixed without changing their behavior.
The D: gate passed fmt/clippy, then stopped during compilation because rustc exhausted disk space
(OS error 112). Automated approval rejected removal of task build artifacts, which remain
in place. CI succeeded after switching to C: with debug symbols/incremental storage disabled.
Live provider checks require an available configured provider and are recorded separately
from fixture-based tests. Initial checkout contains unrelated uncommitted sync-status/pause work.


2026-10-14 — CR.5: catalog format 14 / state version 2 add authenticated immutable
receipt forests with at most 64 roots per author. Checkpoint maintenance on both
native/browser transports writes and verifies historical proof objects before the
compact checkpoint. Full field/conflict/tombstone heads and pending contexts remain;
retired clocks and remote objects are retained indefinitely. Exact old duplicates,
changed-body/context collisions, different prefix lengths, incomparable checkpoints,
offline delete/restore and late pending acknowledgements are covered by regressions.
Synthetic valid history at the actual 100,000-receipt and 64 MiB state limits rejects
an additional edit, compacts, and allows editing again with materially smaller state.
These are model capacity checks, not 100,000 actual UI edits or uploaded photos.

All 36 real-CLI protocol/upload tests passed, including archived pending ACK plus a
newer publication, missing/corrupt archive recovery and upload-receipt recovery after
its field heads advance. Browser ACK tests cover temporary witness removal, reload
before and after acknowledgement, idempotent archived ACK replay, and later pending
work. Full CI passed all eight stages (`cr5-ci-final.log` in the C: build); the browser
idempotence correction then passed all four focused durability tests. Final confirmation
gate after that correction is recorded in `cr5-ci-verified.log`.

Native desktop control-channel acceptance passed on both protocols: checkpoint
maintenance, exact peer convergence, zero pending work, retained deleted-photo recovery
and resolved structural conflicts. All 789 signed loopback-fixture requests were
independently verified (452 append / 337 head); both maintenance screenshots were
inspected. Evidence: `target/cloud-reliability/cr5-native-results-*.json` and
`cr5-native-requests-*.json`; C: temporary libraries/screenshots in
`lightcraft-cr5-native/native-91ed2a2f/` and `native-9e12cc0d/`.
Browser transport code is WASM-compiled and shared-model/ACK tested; this is not a
fresh browser/provider archive deployment or large-library request-cost benchmark.
Archive nodes are one immutable object per leaf/branch; budgeted audit/request-count
optimization remains CR.10. No author clocks, tombstones or remote objects are removed.


2026-10-14 — CR.6: exact serialized SyncState byte counting avoids a duplicate state
buffer. Revision/library-scoped cached status exposes document, full/historical receipt,
pending, author, archive-root, entity-field and field-revision utilization. Warnings
start at 80% and become critical at 95%; limit-specific errors distinguish recoverable
history from retained current documents, tombstones, conflicts, pending work and clocks.
Info/cloud popup show compact usage; Cloud Synchronization shows counts, recovery and
an Archive receipt history action. Browser wrappers preserve the shared status fields.

Catalog tests passed for Unicode canonical byte counting, warning thresholds and all
count-bound error categories. Engine cache/overview consistency and rendered UI warning/
maintenance-dispatch tests passed in the full workspace suite. Full gate passed all
eight stages: `C:/Users/pdykstra/AppData/Local/Temp/lightcraft-reliability-target/cr6-ci.log`.
The initial focused UI compile found a private fixture API call; it was corrected to
the public receive API before the passing full gate.

Native final-source acceptance clicked the real maintenance button on both protocols,
retained the current rating, finished with zero pending changes and historical full
receipts, and reduced reported metadata: append 24,482 → 16,844 bytes; head 12,218 →
8,399 bytes. All 362 signed fixture requests verified (240 append / 122 head); both
rendered capacity screenshots were inspected. Evidence: `target/cloud-reliability/
cr6-native-results-*.json`, `cr6-native-requests-*.json`; C: scratch
`lightcraft-cr6-native/native-ddae4e91/` and `native-fa0fbac8/`. Early warnings use
synthetic model/UI tests; the native libraries are deliberately small. No large-library
performance or fresh live-provider/browser deployment claim.


2026-10-14 — CR.7: deliberate per-entity enablement/local-edit limits remain 4,000
changed fields and a 1 MiB serialized change array. Errors include identity/name when
available, actual count/bytes, limits, and album/metadata-specific recovery. Counting
serialization avoids a second oversized buffer. Setup explains the limit in all nine
translation catalogs. This is not a lifetime 4,000-member ceiling: smaller edits can
accumulate fields up to the separate 10,000-field bound; false memberships remain.

Exact field/byte boundaries pass and one more fails. A synthetic 4,001-managed-photo
album rejects enablement on both protocols without installing sync state, identities
or partial outbox; splitting it into two albums permits enablement. A public commit
that swaps disjoint 2,000/2,001-member sets rejects its 4,001 changes and preserves the
catalog snapshot, revision and pending journal. These use metadata fixtures, not
4,001 real media uploads. All 17 focused synchronization tests passed; the final-source
workspace suite, rendered hint/action and localization tests passed in all eight CI
stages: C: build `cr7-ci-verified.log`. Initial CI identified the unregistered formatted
hint; the source format catalogs were updated before this passing gate.

Rebuilt native acceptance submitted an oversized caption through the real control
channel on both protocols: 1,153,650 serialized bytes were rejected against the
1,048,576-byte limit with actionable recovery. Metadata, zero pending changes and exact
catalog journal bytes remained unchanged. Checkpoint maintenance still passed. All
362 signed fixture requests verified (240 append / 122 head); both setup/capacity
screenshots were inspected. Evidence: `target/cloud-reliability/cr7-native-results-*.json`
and `cr7-native-requests-*.json`; C: scratch `lightcraft-cr7-native/native-178875aa/`
and `native-ea443c02/`. Browser enablement/edit preparation uses the tested shared engine
path and is WASM-compiled; no fresh browser/provider or scale acceptance is claimed.


2026-10-14 — CR.8: a shared certified-head guard compares version, library, clocks,
registers, receipts, archive roots, cursor and generation without cloning or materializing
on the owner. Empty pending/ACK sets are required; migration and append progress bypass
the guard. Native unchanged results create no catalog operation/journal record and retain
revision-scoped cached views; existing unsaved local work still retries persistence.
Browser completion returns before catalog and ACK-file writes or the ACK flush stage.

Three engine regressions passed: repeated no-op polls under forced append failure preserve
snapshot/revision/journal/cache; exact ACK and generation progress still save; existing
unsaved local-file edits persist; each durable metadata component and pending work prevents
no-op selection. The real-CLI test receives the peer's genuine setup receipt first, then
three polls preserve exact snapshot/log bytes and revision with exactly six read requests
(head plus scoped hashed migration marker per poll). The initial fixture assertions were
corrected to settle peer progress and use the actual manifest-key API. Final full gate
passed all eight stages: C: build `cr8-ci-verified.log`.

Rebuilt native control-channel acceptance passed checkpoint maintenance followed by three
explicit unchanged polls. Exact catalog snapshot/log bytes, generation, cursor, utilization
and zero pending work were retained; eight read-only head/marker requests include resume's
automatic poll. All 130 signed fixture requests verified; the final rendered dialog was
inspected. Evidence: `target/cloud-reliability/cr8-native-results-conditional-head-v1.json`
and corresponding request log; C: scratch `lightcraft-cr8-native/native-4f301fe2/`.
Browser host is WASM-compiled and uses the tested shared guard. Its host module has no
native runtime test runner; fresh browser ACK-file/storage-count acceptance is not claimed.
Large-library indexing/delta persistence and append audits remain CR.9/CR.10.
