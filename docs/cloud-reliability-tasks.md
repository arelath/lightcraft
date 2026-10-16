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
- [x] CR.9 — Index global-to-local identities once, materialize changed entities, and persist synchronization deltas. Measure unchanged 10,000- and 50,000-photo request counts, journal growth, state size, and owner-thread latency.
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

## Follow-up review: completion ordering and shared semantics

Source: supplied static review of `64ac4d0` against `208edb8`, received 2026-10-14.
Current starting commit: `1118cf59`. This review predates the verified CR.5–CR.8
work below. The following tasks now take priority, in this order; remaining scale
work (CR.9/CR.10) and other original unchecked tasks remain open.

- [x] CR.22 — Apply verified synchronization results monotonically across upload and ordinary workers. Distinguish stale valid completions from inconsistent remote history, retain newer head cursors, merge append publication progress, and qualify phase events by attempt so obsolete failures cannot overwrite success. Test held generation 10 after generation 11 and append frontier preservation; retain deliberate integrity-verification failures.
- [x] CR.23 — Claim installation/catalog writer ownership in both protocols before editing, preserving pending IDs. Provide a safe writing epoch for explicit same-directory backup restore and causally schedule retained historical-author mutations. Test copied conditional-head catalogs making distinct offline edits and restoring an older backup.
- [x] CR.24 — Materialize smart-album references to deleted or unresolved albums deterministically without broadening their criteria or blocking fresh-device bootstrap. Test a retained smart album referencing a deleted regular album.
- [x] CR.25 — Define a nonportable smart-album policy that preserves originating-device semantics. Preserve folder criteria and local-only selections, and distinguish absent selection restrictions from explicitly empty sets. Test self-sync with folder-restricted and exclusively local-photo rules.
- [x] CR.26 — Resolve shared stack ownership and validity independently of local-only members, then apply local overlays. Test identical shared proposals S1=(P,Q), S2=(Q,R), S3=(R,S) with and without a local-only member of S2, comparing projected cloud relationships.
- [x] CR.27 — Durably finalize every upload receipt covered by shared acknowledgement with exact publication evidence. Retry local proof writes after restart independently of media transfer; test delayed bulk siblings and subsequent local-copy removal without manual verification. Keep staging protection intact.
- [x] CR.28 — Make per-import preflight failures explicit queue outcomes with retained blocked reasons and defined retry eligibility. Test one unreadable manifest alongside a healthy import: healthy work completes and the blocked item exposes its specific recovery action.
- [x] CR.29 — Model subsystem health explicitly: user pause, polling halted by error, publication waiting, structural conflicts, and last successful remote scan. A success clears only the recovered subsystem's failure. Test failed automatic sync followed by upload success, and structural-only conflicts; never infer healthy synchronization solely from no pending local work.
- [x] CR.30 — Record logical fields changed by actions and construct undo/redo inverses against current state. Guard those fields' revisions while preserving independent remote metadata and membership changes. Test local title undo after remote caption editing, plus concurrent changes to the same title.
- [x] CR.31 — Separate verified media readiness, metadata publication, and covered-receipt finalization under one authoritative per-library coordinator (with CR.13/CR.22/CR.27). Share bounded fresh media evidence across active producers, journal exact late-worker proof, and assert bulk request/byte costs for batch and sibling workers.
- [x] CR.32 — Replace contradictory protocol outcome combinations and sentinel cursors/generations with explicit unchanged, conditional-head, and append outcome variants. Test evidence merging, monotonic cursors, and no-op writes.
- [x] CR.33 — Extract independently testable shared structural reconciliation. Add property tests: identical shared synchronization state yields identical shared relationships regardless of local IDs, local-only content, iteration order, or delivery order. Test local-only preservation separately.

## Follow-up review: history boundaries and ordinary photo workflows

Source: supplied static review of `924e9fd` against `64ac4d0`, received 2026-10-15.
Starting commit: `5f15ad37`. These tasks take priority next; original unchecked
CR.9-CR.21 remain separate. Preserve the verified coordinator, ownership and typed
outcomes. Check items only after implementation and relevant acceptance.

- [x] CR.34 — Make conditional-head checkpoint maintenance recover beyond the ordinary 4,096-batch replay budget. Automatically checkpoint before exhausting the normal tail, and provide bounded resumable recovery after it. Test 4,097-batch fresh bootstrap, pending local publication, maintenance and acknowledgement after boundary crossing.
- [x] CR.35 — Bound aggregate history-replay memory independently of final state size and per-object limits. Avoid retaining the decoded tail; use bounded references/spooling/segments with actionable temporary-storage failure and resumable progress. Measure decoded resident batches and memory across longer superseding histories, plus network costs.
- [x] CR.36 — Distinguish library creation from joining. Learn existing immutable identity before authoring join changes; preserve frozen mutation IDs/contexts. Test 129 fresh writers without accumulating identical initialization heads, and reject predictable append capacity failures before PUT.
- [x] CR.37 — Lease browser export originals through deferred command completion, sharing download admission/reservations with renderer loads. Add cache-only lookup; release reservations on cancellation/failure. Test acquisition paused between A/B while unrelated C churns the cache, export without reacquiring A, and total in-flight bounds.
- [x] CR.38 — Preserve supporting XMP throughout managed import and original revisions. Reuse ordinary sidecar discovery/application, durably stage packets and preserve foreign properties. Define explicit carry/transform policy for external-editor/DNG/XMP-write operations; compare local/cloud RAW+XMP import and foreign-property access after revision.
- [x] CR.39 — Make interrupted managed import retry idempotent through durable request/session identity and explicit duplicate policy. Recover independent candidates with per-item errors while preserving invalid manifests and prior-asset consistency. Test interruption after staging A/B, retry/recovery producing one intended set, and corrupt manifest alongside healthy imports.
- [x] CR.40 — Bind storage destination independently of protocol. Reject bucket/prefix/endpoint retargeting before settings/resolver changes; permit credential/profile rotation for the same destination. Share destination identity across configuration, receipts and publication evidence. Test both protocols and actionable deliberate-migration guidance.
- [x] CR.41 — Separate incremental archive proof planning from final merge. Batch missing hashes, verify immutable nodes once, avoid repeated state merges/forest rebuilds per node, and retain exact-proof safety. Add counters for full merges, forest construction, witness checks and archive GETs across small/large compacted histories.

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


2026-10-14 — CR.22: native metadata publication is serialized per open library
through durable owner acknowledgement; media transfer and staging writes retain
separate locks. Both blocking owner loops pump each other's queues. Certified
late head completions retain newer cursors, while scans below their own captured
generation and equal-generation cursor collisions fail. Append progress merges
remote effects/frontiers and removes covered gaps; receipt/manifest collisions
remain errors. Old completions lacking witnesses for newer archives retain state
and outbox and request a fresh scan without pausing automatic polling.

Upload phases carry attempt identity. Obsolete failures cannot downgrade shared
acknowledgement or cause a redundant journal write. Explicit verification begun
on an acknowledged import can still report confirmed remote media damage.
Append merging retains incoming archived ACK witnesses until delayed acknowledgement;
the browser ACK records the merged progress digest. Shared transport certification
includes prior publication progress as well as main catalog state. The initial CI
found two missing-proof append checkpoint regressions; this certification correction
passed all 12 append CLI cases before final verification.

Final source passed all eight CI stages: C: build `cr22-ci-verified.log`, including
three append model tests, held generation 10 after 11, genuine rollback/fork errors,
archival-witness refresh, permit retention through interaction/owner save, obsolete
versus integrity phase failures, and the existing browser ACK/reload tests. Real-CLI
rollback with pending changes and checkpoint maintenance preserves local state.

Rebuilt native acceptance passed automatic three-photo bulk upload alongside normal
synchronization and checkpoint maintenance on both protocols. All four imports
finished synchronized, zero pending mutations remained, and the current rating was
retained after compaction. All 526 signed loopback requests verified (351 append /
175 head); both final rendered dialogs were inspected. Evidence: ignored
`target/cloud-reliability/cr22-native-results-*.json`, `cr22-native-requests-*.json`;
C: scratch `lightcraft-cr22-native/native-1773ad8b/` and `native-df5a5821/`.
Browser transport/host changes are WASM-compiled and ACK/shared-model tested; no
fresh browser deployment or live-provider acceptance is claimed. Shared receipt
finalization and request-cost/stage refactoring remain CR.27/CR.31; scale remains
CR.9/CR.10. Only CR.22 of the follow-up review is complete at this checkpoint.


2026-10-14 — CR.23: native installation/catalog ownership now covers both protocols.
Writer reservations outside catalog backups are durably advanced before local journal
application. A catalog-only rollback at the same path starts a fresh writing epoch;
historical pending IDs and clocks are preserved. Restoring the whole installation
configuration as well requires fresh persistent installation storage. Browser restore
already targets a fresh directory; ownership failures now block local mutation capture.
Conditional-head publication schedules causal dependencies across retained authors
rather than sorting global sequence numbers. Unknown reservation formats fail closed.

Final source passed all eight CI stages: C: build `cr23-ci.log`. Regressions cover
copied head catalogs with divergent offline edits, retained historical pending IDs
before new-author changes, same-path older-backup restoration on both protocols,
reservation-ahead recovery, durable reservation write failure and malformed/oversized
records. Focused engine synchronization/ownership tests passed (71); real CLI head,
append and upload suites passed (41). Rebuilt native acceptance restored older catalog snapshot/journal files at the same
path on both protocols. Both changed writer, preserved the later remote flag and
synchronized the new rating with zero pending mutations. All 93 signed loopback
requests verified (38 head / 55 append); both rendered dialogs were inspected.
Evidence: ignored `target/cloud-reliability/cr23-native-results-*.json` and request
logs; C: scratch `lightcraft-cr23-native/native-634f6b4c/` and `native-aa73c898/`.
The first native script correctly hit the persisted pause and was amended to resume.
Browser ownership/causal ordering is WASM-compiled and shared-model tested; fresh
browser deployment and live-provider acceptance are not claimed.


2026-10-14 — CR.24: materialization now reserves stable local identities for
album tombstones and valid references whose target is not yet delivered, without
creating live placeholder albums. Mapping replay advances the album allocator;
snapshot/restart cannot reuse a reserved identity. Album-rule projection converts
numeric strings through the same ID parser as local matching. Missing/folder
references make a saved criterion unavailable and nonmatching, including negative
operators, nested any/none groups and dependencies through other smart albums.
Saved references remain intact; existing diagnostics show missing targets.

The focused catalog unavailable-rule regression and all 22 focused engine sync tests
passed. They cover fresh bootstrap with different IDs, delayed target delivery,
restart and delete undo restoring the same identity. The first restoration test used
the photo-only recovery command and was corrected to use album-delete undo. Final source passed all eight CI stages: C: build `cr24-ci-verified.log`, including
numeric-string references, mapping replay/snapshot and real-CLI fresh checkpoint
bootstrap on both protocols. The initial full run was stopped before completion to
include numeric-string projection in this final passing gate.

Rebuilt native fresh-device acceptance passed for both protocols. Positive and
negative smart albums opened with zero matches and missing-target diagnostics;
restart and another album allocation retained the absent target identity. All 202
signed loopback requests verified (65 head / 137 append); both rendered empty-album
views were inspected. Evidence: ignored `target/cloud-reliability/cr24-native-results-*`
and request logs; C: scratch `lightcraft-cr24-native/native-78d6f8a6/` and
`native-536562df/`. Shared browser materialization is WASM-compiled and engine tested;
fresh browser/provider acceptance is not claimed. Nonportable filters remain CR.25.


2026-10-14 — CR.25: smart albums with local folder criteria or unmappable photo
selections, and dependent smart albums, stay device-local. Their complete rules are
preserved across enablement and self-sync. Shared-to-local transitions withdraw the
shared copy while keeping the origin; removing nonportable criteria republishes the
local copy. Selection restrictions have an explicit empty-set marker, retained when
incoming target photos are absent. Later recovery resolves the saved global selection.
Legacy empty-list command clearing still works, and an empty similarity result remains
constrained. The setup policy hint is registered in all nine translation catalogs;
`albums.list` reports smart-album scope.

Local format 15, mutation version 2 and checkpoint/state version 3 prevent older readers
from silently ignoring the new marker. Legacy records remain readable without changing
IDs/digests. Compaction retains the newer state version. Catalog regressions passed
(223); focused sync tests passed (26) before the final legacy-clear adjustment. The
first dependency tests used the direct album field, which forbids smart targets, and
were corrected to use supported Album rules; the test helper uses public publication
prefixes. Final source passed all eight CI stages: C: build `cr25-ci.log`, including the legacy
clear adjustment, version/compaction tests, two-protocol real-CLI self-sync/checkpoint
and fresh-peer tests, rendered setup hint and WASM compilation.

Rebuilt native acceptance passed on both protocols. All five origin smart albums
retained exact rules and match counts after synchronization/checkpoint maintenance;
the fresh peer received only the shared empty selection, which matched nothing even
with an added local photo. The real `Only 0 photos` chip cleared after its queued UI
action, and legacy command clearing also worked. Both empty-grid and policy-dialog
screenshots were inspected. All 165 signed loopback requests verified (74 head /
91 append). Evidence: ignored `target/cloud-reliability/cr25-native-results-*.json`
and request logs; C: scratch `lightcraft-cr25-native/native-eaf723bb/` and
`native-d6f2920a/`. The native script was corrected to decode UTF-8 and wait for queued
UI clicks. Shared browser behavior is engine-tested/WASM-compiled; no fresh browser
or provider deployment acceptance is claimed.


2026-10-15 — CR.26: shared stack proposals resolve against live global
photo documents in stable global stack order, before local catalog IDs or members enter
the calculation. Deleted and unresolved photo targets cannot decide ownership. Local
members attach to shared winners afterward; dissolved/deleted proposals retain only
local-only groups of at least two members, with their local collapsed state. Existing
unsynchronized mixed stacks relinquish photos claimed by shared winners. Changes detach
all affected stacks before final installation, preserving uniqueness.

The initial focused synchronization run passed 27 tests. Additional regression coverage
compares projected shared relationships with different local IDs and zero/one/two local
members, surviving and dissolved overlays, an unsynchronized mixed stack, tombstones,
unresolved targets, snapshot/restart and idempotent repeated materialization. The real-CLI
fixture freezes lexical stack identity order, then creates and publishes all overlapping
proposals through normal commands on three devices in both protocols. Final source passed all eight CI stages: C: build `cr26-ci.log`; native binaries were
rebuilt successfully (`cr26-native-build.log`). The first CLI test compile used `cloned()`
on a string slice and was corrected to `str::to_owned` before the final passing gate.
Broader extracted structural property coverage remains CR.33.

Native three-device acceptance passed on both protocols. Shared projections were identical
on all three catalogs and a fresh checkpoint join; the losing shared stack retained its
two local-only photos as a local group through app restart and checkpoint maintenance.
Zero pending mutations remained. Both rendered grids and structural-choice dialogs were
inspected; `ui.inspect` was captured. All 510 signed loopback requests verified (242 head /
268 append). The script's initial paused-sync call was corrected to resume before manual
publication. Evidence: ignored `target/cloud-reliability/cr26-native-results-*.json` and
request logs; C: scratch `lightcraft-cr26-native/native-ae64acdb/` and `native-c0a58e32/`.
Shared browser materialization is engine-tested/WASM-compiled; no fresh browser or live
provider acceptance is claimed.


2026-10-15 — CR.27 implementation: shared publication ACKs now retain a catalog-local exact
receipt proof queue in the same journal batch. Local format 16 prevents older readers from
dropping this queue. Staging acknowledgement requires the queue to be durably cleared, after
its bound sidecar has been finalized. Local receipt jobs use the durable staging lock,
release it before owner ACK, retry after restart or local I/O failure, and run while metadata
publication is paused. Resume resets their 30-second per-import retry backoff. Upload workers
complete their own queue entry after writing final proof. Local-copy removal completes
covered queues first; neither receipt retry nor removal transfers media.

Initial all-target engine check passed. Catalog upload journal/schema tests passed (2);
initial upload-engine tests passed (12). The two-protocol real-CLI shared bulk-import case
held A's unfinished receipt, published through B, preserved staging through local failure,
then restarted and removed A's copy without `verify:true`. Exact proofs were written, the
queue cleared, and request counts and remote objects stayed unchanged (zero network bytes).
All 14 focused upload-engine tests passed, including owner-journal failure and obsolete
library/connection completions. The initial full CI caught the format guard still naming
the previous newest operation; the corrected final source passed all eight stages
(`cr27-ci-verified.log`). Native binaries rebuilt successfully (`cr27-native-build.log`).

Native restart acceptance passed on both protocols while publication was paused. A's
unfinished receipt stayed queued and its original stayed protected through a local-file
failure after B supplied shared publication acknowledgement. After restoring A's receipt
and restarting, local-copy removal finalized its exact proof and durably cleared the queue
without manual verification. Remote object contents were unchanged and finalization/removal
made zero cloud requests. All 100 signed loopback requests verified (46 head / 54 append);
both rendered grids were inspected and `ui.inspect` captured. Evidence: ignored
`target/cloud-reliability/cr27-native-results-*.json` and request logs; C: scratch
`lightcraft-cr27-native/native-bbcdc804/` and `native-27ee4ae9/`. CR.27 is complete.
Shared acknowledgement/schema behavior is engine-tested/WASM-compiled; native local receipt
workers have no browser equivalent. No live-provider acceptance is claimed. Queue/scheduler
health UI remains CR.29, and broader media-stage/request-cost work remains CR.31.

2026-10-15 — CR.28 implementation: preparation failures retain a device-local blocked reason
and retry mode in the upload intent before workers start, including successful batches with
skipped imports and automatic start failures. Format 17 prevents older readers from dropping
this recovery state. Specific popup/photo actions retry that import, preserving deliberate
verification mode. Preparation failures do not revoke synchronized state or verified keys.
Blocked imports become issues instead of ordinary waiting work. Automatic eligibility resets
on resume, relevant configuration/setup changes and reopen; manual import retry bypasses the
attempted set. Healthy work proceeds within causal publication requirements. Manifest identity
is checked even for still-live photos. Preparation/status detail batches are bounded to 32.

Focused schema/status tests passed (2), as did automatic engine persistence/retry and the
rendered popup's concrete retry dispatch. The two-protocol real CLI case passed after enabling
append mode in its storage fixture (the initial fixture rejected legitimate append writes).
It verifies healthy causal-prefix publication, durable manifest errors and preservation of a
prior ACK during deliberate verification preparation failure. Ordinary unselected batches
skip already-blocked work so a bounded batch cannot be starved by repeat preflight failures.
The first full CI exposed a retention-reservation regression from manifest-check ordering;
connection maintenance now checks first, preserving its existing wait/cancel behavior without
creating a blocked-import error. Its focused test passed, then the final source passed all
eight CI stages (`cr28-ci-verified.log`). Native binaries rebuilt (`cr28-native-build.log`).

Native automatic scheduling and restart acceptance passed for both protocols. The healthy
import synchronized while the damaged later import retained its manifest error and appeared
as a sync issue with zero ordinary waiting files. Restart preserved its specific reason;
after repairing the file, clicking the actual per-import Retry button synchronized it and
cleared the block. A later deliberate verification preparation failure retained the prior
ACK/verified keys and exposed verification retry mode. All 118 signed loopback requests
verified (49 head / 69 append). Both blocked popup and recovered grid screenshots were
inspected; `ui.inspect` was captured. The first native fixture timed out at its initial quit;
the passing fixture explicitly pauses/drains background work and renders before quitting.
Evidence: ignored `target/cloud-reliability/cr28-native-results-*.json` and request logs;
C: scratch `lightcraft-cr28-native/native-d461fb22/` and `native-76cf7cf8/`. CR.28 is complete.
Shared status/UI/schema behavior is tested and WASM-compiled; native staged-upload preparation
has no browser equivalent. No live-provider acceptance is claimed. Scheduler health remains CR.29.


2026-10-15 — CR.29 implementation: native/browser polling health now retains its own
error and last verified remote scan independently of upload activity. Resume permits
retry but does not erase the error before a successful check. Verified upload
publication records scan evidence without restarting halted polling. Media/cleanup
failures recover per resource; bounded details retain total failure counts. Direct
library replacement cancels uploads and resets per-library health/attempt state;
superseded results cannot halt the new scheduler. Cached status includes durable
blocked/failed intents and receipt work, even for removed imports. The popup/icon
include structural-only conflicts, pending receipts and append gaps, and show scan
age. Up to date requires verified healthy polling and no recovery work/conflicts.
All nine locale catalogs include the new status labels.

The focused rendered popup test passed for stopped polling and structural-only
conflicts, then recovery to Up to date. The focused owner-worker regression passed
for failed polling followed by a higher-ID upload success and verified scan recovery.
All eight full CI stages passed (`cr29-ci.log`), including native/browser compilation and all workspace tests. Native binaries rebuilt (`cr29-native-build.log`).


Rebuilt desktop acceptance passed on both protocols. Automatic polling failed on
rejected fixture credentials; a later successful upload/publication retained the
polling error and attention badge despite zero pending work. Another peer edited a
photo; the stopped device received it only after clicking the actual Retry Syncing
control, which cleared that subsystem's error. Concurrent folder moves produced
one structural conflict with zero photo issues/local changes; both protocol
popups and badges correctly required attention. Both stopped-polling and structural
popups were visually inspected, as was the recovered grid; `ui.inspect` was captured.
All 307 signed loopback requests verified (128 head / 179 append). The first native
script assumed a synchronous desktop import and was corrected to await its background
report before checking upload completion. Evidence: ignored
`target/cloud-reliability/cr29-native-results-*.json` and request logs; C: scratch
`lightcraft-cr29-native/native-2f2db31d/` and `native-191dfc40/`. CR.29 is complete.
Shared health/UI behavior and browser host are tested/WASM-compiled; fresh browser
and live-provider acceptance are not claimed.


2026-10-15 — CR.30 implementation: undo history records changed metadata keys and
individual album members, then builds inverses from current state while retaining
those saved targets. Independent received caption/member changes survive undo and
redo. Masks carry forward through both directions and merged/nested operations;
local-only membership is preserved separately from shared revisions. Exhaustive
metadata construction makes future fields a compiler obligation. Captured revision
guards cover newly authored fields only; grouped history retains each local field's
guard instead of recertifying remote changes. Normal commit capture needs no extra
whole-catalog clone, and no persisted operation/schema changes are introduced.

The initial membership regressions passed; metadata fixtures were corrected to
select the peer photo required by the existing command. All four initial focused
logical tests then passed. Additional regressions cover remote same-field and
independent changes arriving between grouped local actions. Full CI and rebuilt
native acceptance are pending; CR.30 remains unchecked.


The first full gate passed fmt/clippy/HEIF and 916 engine tests, but exposed two
structural-restore guard regressions (deleted albums and import redo). Recreating
a retained object projects unchanged fields again; the guard now accepts only
saved values equal to every retained head without requiring unrelated authority.
Different retained remote values still require their own guard, with an added
old-payload restore regression. The final full gate remains pending.


Final source passed all eight CI stages (`cr30-ci-verified.log`), including
restoration guards, grouped-history checks, existing filesystem retry regressions
and WASM compilation. Native binaries rebuilt (`cr30-native-build.log`). The first
synthetic retained-caption test incorrectly switched the local device without
claiming ownership; the passing test receives a proper peer mutation instead.

Rebuilt native acceptance passed on both protocols through normal commands. Title
undo preserved a received caption; title redo preserved a newer received caption.
A competing title revision refused undo with values retained. Album-member undo
kept an independently received member, and redo preserved its later remote removal.
The album had an existing cover, avoiding a separate intentional cover conflict.
Both protocol Info screenshots showing exact title/caption values were inspected;
`ui.inspect` was captured. All 530 signed loopback requests verified (290 head /
240 append). Evidence: ignored `target/cloud-reliability/cr30-native-results-*.json`
and request logs; C: scratch `lightcraft-cr30-native/native-499569f9/` and
`native-aa221dfe/`. CR.30 is complete. Browser/shared undo behavior is engine-tested
and WASM-compiled; fresh browser and live-provider acceptance are not claimed.


2026-10-15 — CR.31 implementation: media readiness, one coordinated ready-batch
publication and local receipt finalization are distinct stages. Exact proof queues
commit before sidecar finalization; local failure retries without media transfer.
Append's repeated item passes and implicit retry are removed. Frozen publication
intent stays intact and a batch shares one immutable sync snapshot. A bounded private
fresh-evidence set covers destination/key/version/hash/size, including dependency
verification and uncertain-write recovery. Persisted markers never populate it;
reopen/explicit verify freshly checks media. Verification dependency bytes enter
the transferred-byte counter. Sidecar writes preserve compatible newer proof and
ordinary acknowledged state. Item outcomes/fresh-media signals prevent sibling or
local-receipt failures from creating false verification errors. Typed maintenance
purposes fix a connection named receipt colliding with receipt-worker routing.

Focused initial acceptance passed: four engine evidence/receipt/resource tests and
the two-protocol real-CLI bulk/multipart cost/reopen test. Wire counters assert one
successful full GET and one payload upload per unique object, zero media requests
for metadata-only work, and fresh verification after reopen. Existing lost-completion
bulk acceptance now carries the same byte/request assertions. Initial checks caught
missing trait/mutable-owner imports and a fixture path typo; corrected checks passed.
Final clippy passes; expanded recovery-signal tests, full CI and rebuilt native
acceptance remain pending. CR.31 stays unchecked.

The first full extraction gate passed all eight stages (`cr31-ci.log`). Review of
automatic scheduling found cross-task dependency downloads and late ready imports
whose sibling had already cleared the outbox. The coordinator now carries bounded
fresh readiness across active uploads, defers reads owned by active producers, and
queues sealed exact receipt proof from late verified workers. Fresh verification
invalidates shared evidence; connection epochs reject stale contributions. New
regressions cover readiness visibility, explicit audits, epoch reset, and durable
late proof after the pending mutation is gone. Final gate/native cost checks pending.

The coordinator gate also passed all eight stages (`cr31-ci-coordinated.log`). A
final transaction review found that an obsolete archive result requested a rescan
before journaling its independently checked receipt proof. Owner preparation now
retains that proof while leaving metadata for the fresh scan; its regression keeps
the newer cursor. Separate sibling real-CLI workers now carry transfer/request-cost
assertions as well as the one-task batch. Final-source gate and native checks pending.

Final sibling real-CLI request/byte checks passed on both protocols. The gate found
a synthetic stale-history fixture that did not actually advance an author already
present in the old result; its rescan assertion was therefore invalid. The fixture
now includes that author's earlier revision before advancing/archiving it. The
corrected focused check and final confirmation gate remain pending.

The final confirmation gate was stopped while queued behind the focused build:
explicit verify must bypass shared evidence for waiting/failed imports too, not
only synchronized intents. The verification-request flag now controls that policy;
attempt reverification identity still separately governs acknowledged-state failure.

Final source passed all eight stages (`cr31-ci-verified.log`), including the corrected
stale-archive proof regression and both one-batch/separate-worker transfer-cost
checks. Rebuilt native acceptance is the remaining CR.31 verification step; its
checkbox stays open until those runs finish.

Rebuilt native acceptance passed on both protocols (`cr31-native-build.log`,
`cr31-native-{head,append}.log`). Automatic three-import scheduling (including a
9 MiB multipart original) sent and fully checked each of nine unique objects once:
9,457,252 payload bytes each direction per library. Every receipt finalized and
pending publication reached zero. After reopening, an original was removed without
manual upload verification or any network request; a subsequent explicit audit
read each object once without uploading again. The two cloud-popup screenshots
were inspected and `ui.inspect` captured. All 141 signed loopback requests verified
(64 head / 77 append). Evidence: ignored `target/cloud-reliability/cr31-native-*.json`
and C: scratch `lightcraft-cr31-native/native-9e227312/`, `native-a22f785b/`.
CR.31 is complete. This is native fixture acceptance; no new live-provider/browser
runtime or large-library latency claim is made. Earlier scale work stays open.

2026-10-15 — CR.32 implementation: native/browser producers and owners now use a
shared explicit outcome enum. Published heads require validated digest/nonzero
positions; empty head and unchanged scans carry no ACKs, and append owns its sole
portable state through progress. A common application path handles monotonic cursors,
ACK subsets and empty-head rollback. Unchanged evidence is retained and compared
against current owner state before skipping writes. Browser incoming state flushes
before durable acknowledgement; head receipts use the retained position, append
receipts use merged progress, and empty heads need no fabricated receipt. Legacy
browser receipt-file encoding remains compatible; transport reports omit meaningless
head fields. Initial checks caught two tests mutating removed struct fields; those
fixtures now construct typed variants. Native and browser-WASM checks passed. Focused
regressions, full CI and rebuilt native acceptance remain pending; CR.32 stays open.

Focused CR.32 verification passed: 102 engine cloud tests, five browser durable-ACK
tests, native all-target clippy, and browser-WASM checking. The tests include exact
archived ACK replay/reload, late owner edits, head rollback/fork refusal, append
full-disk rollback, unchanged head no-write behavior, typed empty heads and validated
positions. Published-head legacy helpers now reject sentinel arguments as well.
Full gate and rebuilt native checks remain pending.

CR.32 final source passed all eight CI stages (`cr32-ci.log`), including the opaque
published-position compile-fail documentation test. Native rebuild and both-protocol
acceptance of the changed report shape remain pending before checking the task off.

Rebuilt native acceptance passed on both protocols (`cr32-native-build.log`,
`cr32-native-{head,append}.log`): actual ordinary-sync reports contained validated
head positions or append frontiers with no fabricated cursor/generation. Automatic
bulk upload, exact receipt completion, no-network original removal after restart,
and fresh explicit verification retained their one-object byte costs. All 141 signed
requests verified (64 head / 77 append). Both cloud-pop-up screenshots were inspected,
with no queued work; `ui.inspect` was captured. Evidence: ignored
`target/cloud-reliability/cr32-native-*.json`, C: scratch
`lightcraft-cr32-native/native-74966b4b/` and `native-c246e98c/`. CR.32 is complete.
Browser receipt semantics are unit-tested and full WASM-compiled; this run does not
claim fresh browser runtime or live-provider acceptance. CR.33 remains open.


2026-10-15 - CR.33 implementation: global folder-cycle and shared-stack resolution
now has a pure synchronization-state boundary. Catalog identity allocation and
local overlay preservation remain adapters. Three 64-case generated properties
plus a fixed cycle regression passed: shared projection ignores local IDs, private
members, concurrent proposal delivery and insertion order; restart/reapplication
is stable and private photos/rules/groups survive separately. Initial test macro
formatting and clippy multiple-of diagnostics were corrected. Full CI and rebuilt
native acceptance remain pending; CR.33 stays unchecked.

CR.33 final source passed all eight CI stages (`cr33-ci.log`), including generated
convergence/local-overlay tests in the full workspace run. Native rebuild and
both-protocol control-channel acceptance remain pending before checking it off.

Rebuilt native CR.33 acceptance passed under both protocols (`cr33-native-build.log`,
`cr33-native-{head,append}.log`). Three existing catalogs with different local IDs
and mixed private members, plus a fresh fourth join after checkpointing, produced
the same shared stacks and reversed folder hierarchy. Private folder rules and
local groups survived; retained structural choices remained visible. Grid and
conflict-dialog screenshots were inspected, and `ui.inspect` captured empty render
queues. All 675 signed loopback requests verified (290 head / 385 append).
Evidence: ignored `target/cloud-reliability/cr33-native-*.json`, C: scratch
`lightcraft-cr33-native/native-8c0bb306/` and `native-7227b648/`. CR.33 is complete;
CR.22-CR.33 are checked. Original CR.9-CR.21 remain open. This run adds native
fixture acceptance, not fresh live-provider/browser runtime or scale benchmarks.


2026-10-15 - CR.34 implementation: native history replay now separates compact
reference traversal from one-batch replay. A fixed ring avoids another retained-tail
limit; verified oldest-prefix checkpoints become durable through head CAS before
local pending work participates. Automatic checkpoints begin at 1,024 records,
including unchanged idle heads. Read-only migration replays successive windows.
The initial 4,097-batch interruption/restart, pending preservation, fresh join and
ordinary publication regression passed, along with five head regressions. Ring
refinement, full CI and rebuilt native automatic-maintenance acceptance remain
pending; CR.34 stays unchecked. CR.35 retains memory measurements/browser replay.

CR.34 final fixed-ring source passed all eight CI stages (`cr34-ci.log`), including
the interrupted 4,097-batch maintenance/restart regression, unchanged-head automatic
checkpoint test, 100,000-reference fixed disk/cycle tests, and existing head/append
regressions. Rebuilt desktop automatic-maintenance acceptance remains pending.

Rebuilt native acceptance passed (`cr34-native-build.log`, `cr34-native.log`):
automatic desktop polling checkpointed a 1,024-marker tail (generation 1025 to
1026), retained the managed photo, and allowed a fresh catalog to join. The grid
and healthy cloud popup were inspected; `ui.inspect` captured no render backlog.
All 2,101 signed loopback requests verified. Evidence: ignored
`target/cloud-reliability/cr34-native-results.json`, C: scratch
`lightcraft-cr34-native/native-19b92458/`. CR.34 is complete. Browser replay and
measured resident-memory/request costs remain CR.35; no new live-provider or
large-library latency claim is made.


2026-10-15 - CR.35 implementation: browser head replay now retains only a bounded
reference deque and decodes one batch at a time. It shares native window/checkpoint
limits and cycle detection, including CAS prefix recovery and read-only migration
windows. Native measurement on CR.34's binary used 100 versus 1,500 sequential
256 KiB caption replacements: 26,280,901 versus 394,219,202 history bytes, final
state 271,306 versus 271,585 bytes, peak resident 19,140,608 versus 21,688,320
bytes, and peak commit 7,639,040 versus 10,092,544 bytes. Each large immutable batch
had exactly two GETs (200 versus 3,000), so the memory result does not conceal the
request tradeoff. Evidence: ignored `cr35-memory-results.json`, C: scratch
`lightcraft-cr35-memory/native-9cc67746/`. Browser-WASM checking passed; final-source
CI, repeated native measurement and actual browser recovery remain pending.
CR.35 stays unchecked.

The first CR.35 source gate passed all eight CI stages (`cr35-ci.log`). A checked-in
Windows child-process memory regression and structured-argument PowerShell profiler
are being added before final confirmation, so future runs test the historical-payload
boundary as well as bounded references. Browser runtime acceptance is still pending.

The checked-in child-memory regression passed (`cr35-memory-regression-2.log`). An
initial fixture paused synchronization before the measured operation; removing that
unrelated precondition allowed the intended reader path. For 100/1,500 batches,
peak resident was 28,553,216/31,145,984 bytes and peak commit 12,341,248/14,495,744;
final states were 270,447/270,722 bytes. All large payloads had exactly two GETs,
with valid signatures. The test runs through a structured-argument, hidden child
profiler and reports missing PowerShell 7 as a skip; it executed on this host.
Browser runtime acceptance and final confirmation gate remain pending.

CR.35 final source passed all eight CI stages (`cr35-ci-final.log`), including the
checked-in child-memory regression, bounded reference/cycle tests, cloud recovery
regressions and browser-WASM checking. Browser runtime boundary acceptance is
still running; the task remains unchecked until that result is recorded.

Rebuilt browser runtime acceptance passed under OPFS and IndexedDB
(`cr35-web-build.log`, `cr35-browser-4.log`). Each fresh catalog recovered a
4,097-marker tail through a prefix checkpoint, preserved both pending mutations
through authorization loss and page reload, then published them and a later album
that a native peer received. OPFS generations were 4098 -> prefix 4096 -> final
4100; IndexedDB 8198 -> prefix 8195 -> final 8200. All 16,560 signed requests and
conditional PUTs verified. The editor screenshots were inspected; `ui.inspect`
was captured. Initial harness startup failures came from Python 3.7's WASM MIME
mapping and an undersized fixture deadline; corrected harness runs exercised the
application without source changes. Evidence: ignored `cr35-browser-results.json`,
C: scratch `lightcraft-cr35-browser/browser-d10d2d95/`. CR.35 is complete. This
adds loopback browser recovery acceptance, not live-provider or latency results.

2026-10-15 - CR.36 implementation: create and join are explicit enrollment paths.
Fresh joins omit library initialization and require existing remote storage before
publication. Native/browser append validate ready operations before PUT. Frozen
outbox IDs remain unchanged. Real-binary 129-join, missing-destination and combined
capacity regressions are running; full CI and rebuilt user-path checks remain
pending. CR.36 stays unchecked.

The initial three CR.36 real-binary tests passed (`cr36-focused.log`): 129 fresh
joins in each protocol made no PUTs and preserved one initialization head/clock
author; missing destinations preserved local work; a combined 129th revision was
rejected before operation PUT. Source review found a browser direct-call empty-ID
inconsistency. The first gate was deliberately stopped before editing; both hosts
now reject an explicit empty ID before enrollment, with a fourth regression and
browser runtime assertion. Final gate and rebuilt checks remain pending.

CR.36's next gate passed fmt/clippy/HEIF, then exposed a pagination fixture that
implicitly depended on a read-only join author. With no second operation author,
the fixture never reached a second root-list page and never injected its late
object. The setup now gives the second device a real album edit, retaining the
original late-insertion and next-poll discovery assertions. Source behavior was
unchanged; focused pagination verification and the final full gate follow.

Rebuilt CR.36 desktop acceptance passed under both protocols (`cr36-native-build.log`,
`cr36-native-{head,append}.log`). Automatic read-only joins issued no metadata PUT,
kept one identity head, then published an actual album edit whose receipt context
included the observed remote clock. The peer received the edit. All 61 signed
requests verified (28 head / 33 append). Both cloud popup screenshots were inspected
and showed healthy synchronization; `ui.inspect` recorded no render queue. Evidence:
ignored `cr36-native-results-*.json`, C: scratch
`lightcraft-cr36-native/native-5c374012/` and `native-556b597a/`. Browser runtime and
final full gate remain pending; CR.36 stays unchecked.

Rebuilt browser CR.36 acceptance passed in OPFS and IndexedDB (`cr36-web-build.log`,
`cr36-browser-2.log`). The first harness run encountered the browser's intentional
demo albums, which legitimately queued existing local entities. The corrected
fixture removed only its disposable demo data through normal library commands,
saved/reloaded, then tested a genuinely empty shared projection. Both joins made
zero PUTs, rejected explicit empty IDs without partial enrollment, survived reload,
and published later album edits received by the native peer. Identity heads stayed
at one. All 63 signed requests/conditional PUTs verified; screenshots and
`ui.inspect` were captured. Evidence: ignored `cr36-browser-results.json`, C: scratch
`lightcraft-cr36-browser/browser-9ac1e593/`. The final complete gate is pending.

CR.36 is complete and checked off based on the three focused create/join/capacity
regressions, the corrected pagination regression, rebuilt desktop acceptance in
both protocols, and OPFS/IndexedDB browser acceptance. At the user's explicit
request, further testing was stopped and the changes committed. The final gate
(`cr36-ci-confirm.log`) passed fmt, clippy and HEIF, but was stopped during workspace
compilation; the complete workspace/parity/layers/assets/WASM gate did not finish.
The added native empty-ID regression was compiled by clippy but was not executed;
its browser equivalent passed at runtime. No all-eight-stage CR.36 CI pass is
claimed. CR.37-CR.41 remain unchecked; no further implementation will proceed in
this session.


2026-10-16 - CR.37 implementation: browser acquisition now reserves a command's
complete distinct source set and retains a lease through synchronous deferred
command execution. Renderer misses and commands use one four-slot queue with a
512 MiB source-payload reservation limit. Last-lease cancellation releases queued
work and aborts unused in-flight work while retaining its accounting until the
future settles. Cache-only selection is installed in foreground/worker preview
paths and export preparation. Four initial memory regressions passed; expanded
regressions, full repository CI and rebuilt Chromium acceptance are pending.
CR.37 remains unchecked. Total browser RSS is not bounded by the source-payload
limit; decoded pixels and browser/transport copies have separate costs.


CR.37's first gate passed fmt/clippy/HEIF and the history-memory/create-join tests,
then stopped on an old no-op poll fixture that expected read-only joining to
publish another identity receipt. The fixture now expects CR.36's intentional
unchanged result; its focused regression passed. All 36 browser-library tests
passed, including five source-admission/lease regressions and the foreground/
worker cache-only proxy check. The first browser rebuild exposed native setup
wizard references missing WASM guards after the merged setup branch. Those
references now consistently share the module's native boundary; browser storage
and synchronization dialogs retain their existing paths. Rebuilt runtime and a
complete final gate remain pending.


Rebuilt CR.37 Chromium acceptance passed in OPFS and IndexedDB
(`cr37-web-build-2.log`, `cr37-browser.log`). Three distinct procedural PNGs padded to 200 MiB each
exercised the production 512 MiB budget: with B held, A remained resident and
A/B reserved 400 MiB; renderer C stayed queued. Releasing B produced both full
32x24 PNG exports, with exactly one GET per original and no A reacquisition.
Both commands released all pins; C then loaded and resident/reserved payloads
remained 400 MiB. All 24 signed fixture requests verified, with no page errors.
The editor screenshot and an exported image were inspected; the screenshot was
taken with no photo selected, so no rendered-preview acceptance is claimed.
Evidence: ignored `cr37-browser-results.json`, C: scratch
`lightcraft-cr37-browser/browser-7f9dfdf2/`. The final complete gate is pending;
CR.37 stays unchecked until that passes.


CR.37 final source passed all eight CI stages (`cr37-ci-confirm.log`): fmt,
clippy, HEIF, workspace tests, parity references, layers, assets and WASM.
Clippy required retaining an explicit `cfg(test)` attribute separately from the
native-platform guard so existing test-only unwraps remain recognized as tests.
This does not change the accepted browser runtime. The task is complete and
checked off. CR.38-CR.41 and the separate scale backlog remain open.


2026-10-16 - CR.38 implementation: managed import discovers ordinary preferred/
alternate/uppercase sidecars with raw embedded-XMP fallback, stages the exact
packet, and freezes defaults plus sidecar interpretation in the manifest. Bounded
supporting documents survive XMP writes, DNG embedding and editor revisions.
Fresh owned packets on rendered derivatives use neutral settings so a later read
cannot apply already-rendered source edits again. Foreign properties/history and
other associated documents are carried and staged. Focused RAW/local parity,
embedded, revision-chain, malformed-input and fresh-device CLI regressions are
being added/run. Full CI and native user-path acceptance remain pending; CR.38
stays unchecked.


CR.38 focused verification passed: all 19 managed-import unit regressions
(`cr38-focused.log`) and the real-binary supporting-packet/fresh-peer regression
under both protocols (`cr38-cli.log`). The latter checks remote foreign-property
bytes and an actual GET of the associated object on the new device, followed by
neutral recipe readback and full export. The shared S3 fixture module carries the
same test-only dead-code allowance used by other focused integration tests.

Rebuilt desktop acceptance passed in conditional-head and append modes
(`cr38-native-build.log`, `cr38-native-head-2.log`, `cr38-native-append.log`).
Control-channel import showed the sidecar's title/rating/flag and +0.75 exposure;
source removal, automatic publication, external-render preparation, fresh-peer
XMP read and export succeeded. Screenshots of the imported photo and neutral
0.00-exposure derivative were inspected. All 129 signed requests verified
(68 head / 61 append). An initial harness assertion used the full-photo metadata
shape against the compact query summary; correcting the harness to its documented
`title` field allowed the intended acceptance without source changes. Evidence:
ignored `cr38-native-results-*.json`, C: scratch
`lightcraft-cr38-native/native-2332c36e/` and `native-e01769cf/`. The test prepares
durable editor files through the engine; it does not launch an external editor.
Full repository CI remains pending; CR.38 stays unchecked.


CR.38 final source passed all eight repository CI stages (`cr38-ci.log`): fmt,
clippy, HEIF, workspace tests (including the new two-protocol fresh-device XMP
regression), parity references, layers, assets and WASM. The item is complete and
checked off. Browser original import remains unsupported; no new live-provider
or large-library performance result is claimed.


2026-10-16 - CR.39 implementation: device-local requests precede staging and retain
candidate identities plus frozen recipes across interrupted replies/restart.
Explicit new requests and content duplicate policy distinguish deliberate copies
from retries; manifests retain that policy during later recovery. Independent
preparation/recovery errors persist with per-item retry actions, while healthy
candidates share one bulk journal commit and prior-asset checks remain enforced.
Desktop request/status IO uses a worker/owner acknowledgement handshake. The first
focused run exposed an owner-thread staging-lock attempt; registration/finalization
were moved to workers. Expanded regressions, rebuilt native acceptance and full
repository CI remain pending. CR.39 stays unchecked.


CR.39 verification found two deliberate-duplicate fixture assumptions: the shared
CLI helper and multipart bulk fixture now explicitly request allow. A separate
existing retention test caught cross-connection duplicate skipping; skipping now
matches both original hash and connection. All eight stages passed on the
pre-retirement revision (`cr39-ci-verified.log`). Final review then found skipped
manifests could indefinitely protect otherwise acknowledged shared originals.
Skipped decisions now retain their exact manifests outside active staging, before
retiring only the redundant ownership record; no media bytes are deleted. The
retention/crash-boundary regression passed, as did 27 focused import tests including
serialized overlapping aliases, stale-result suppression and changed skip targets.
Request result ordering has an additional mixed new/skipped regression. Final
native acceptance and a complete final-source CI gate remain pending; CR.39 is
still unchecked.


Final CR.39 focused verification passed: all 28 import regressions plus the staging
retention/crash-boundary regression (`cr39-accepted-focused.log`,
`cr39-accepted-retention.log`), with workspace clippy and rebuilt native binaries.
Native control-channel acceptance passed in both protocols (`cr39-native-head.log`,
`cr39-native-append.log`): partial success, retained error after restart, source-free
retry with stable request/result IDs, independent corrupt-manifest recovery,
skipped-record retirement, intentional duplicates and automatic publication.
Screenshots of both error popups and the recovered library were inspected; errors
are visible with retry actions and final healthy publication shows Up to date.
All 147 fixture requests verified (67 head / 80 append). Evidence: ignored
`cr39-native-results-*.json`, C: scratch
`lightcraft-cr39-native/native-ba3309c7/` and `native-74666b9b/`.
The complete final-source gate (`cr39-ci-complete.log`) is running; CR.39 remains
unchecked until it passes. No browser original-import or live-provider claim is
added.


CR.39 final source passed all eight repository CI stages (`cr39-ci-complete.log`):
fmt, clippy, HEIF, workspace tests, parity references, layers, assets and WASM.
Together with all 28 focused import regressions, the staging-retention regression,
and native acceptance in both protocols, the item is complete and checked off.
CR.40-CR.41 and the separate scale backlog remain open.


2026-10-16 - CR.40 implementation: both protocols and existing managed assets bind
connection configuration to the shared receipt/publication destination identity.
Rejections precede settings/resolver updates; the wizard checks before saving typed
credentials. Browser comparison pins the full optional bucket root, while allowing
authorization/token rotation. Native and browser unit checks and the revised CLI
receipt-protection regression passed; native/browser builds passed.

Native acceptance exposed an existing mismatch: slash aliases compared as the same
destination but the live prefix remained unnormalized, so metadata polling halted
with invalid library object prefix/key. Loading, configuring and probing now use
the same normalized spelling as proof comparison. The expanded native regression
also preserves a legacy settings file while normalizing its loaded connection.
Acceptance reruns and full final-source CI are pending; CR.40 remains unchecked.

CR.40 final verification passed all eight repository CI stages
(`cr40-ci-accepted.log`). The full gate caught restore/synthetic fixture assumptions:
restores now retain their original connection file, upload fixtures configure before
injecting managed assets, and the wizard fixture opens an in-memory library before
its real destination check. No destination guard was weakened. All 967 engine tests
passed (five environment-dependent tests ignored), including missing-settings refusal,
unchanged settings/resolver on rejection, and legacy slash-alias loading.

Native acceptance passed both protocols after profile rotation and removal of the
old profile (`cr40-native-*-final.log`), with 52 signed requests. Browser acceptance
passed six OPFS/IndexedDB cases covering both protocols and optional head bucket roots,
reload after rejection, authorization rotation, and native-peer publication, with
348 signed requests (`cr40-browser-final.log`). Native and browser screenshots were
inspected. Evidence: ignored `cr40-*-results*.json`; native scratch `native-399b1d18`
and `native-702ad363`, browser scratch `browser-f8f455d0`. CR.40 is complete and checked
off. This verifies configuration workflows, not protection against manual settings-file
tampering, live-provider acceptance, or a storage-migration tool.
2026-10-16 - CR.41 implementation: an opaque archive merge session freezes the
certified snapshots, constructs receipt forests once, shares immutable nodes between
main and append-publication targets, and resumes blocked paths by hash. Transports
fetch deduplicated frontiers before the final document merge; conditional-head
checkpoint/replay/migration caches now survive through final certification. Exact
root scope, prefix and receipt/context checks remain required, and a path failure
poisons its session. Wire formats and bounds are unchanged. Small/large model and
real-CLI GET regressions, append-progress coverage, browser archive acceptance and
full repository CI remain pending; CR.41 stays unchecked.
CR.41 focused checks passed: nine archive model regressions (`cr41-model.log`),
workspace clippy (`cr41-clippy-final.log`), the retained append-publication regression
(`cr41-publication.log`) and real-CLI small/large bootstrap (`cr41-get-counts-final.log`).
The first CLI fixture was rejected because its checkpoint generation equalled its
head; it now constructs actual bounded mutation batches and a following marker.

For two authors and two certification targets, model histories of 128/8,192 total
receipts both built three forests and performed two final state merges; unique
witness checks/fetches were 12/24, in 6/12 rounds. Counts describe one proof session;
independent owner/durability checks still validate their current snapshots. Native
fresh-head bootstrap used 7/13 archive GETs for 64/4,096 added archived mutations,
with every requested node unique across checkpoint read and final certification.
No wall-clock, owner-thread or overall large-library performance claim is added.
Browser acceptance and full final-source CI remain pending; CR.41 is unchecked.
Browser archive acceptance caught a CR.41 proof-emission gap during native append
seed setup: certification could share locally generated nodes across both targets,
but omitted them from the returned evidence. The independent owner publication merge
then requested an unavailable witness. Full CI was stopped before source edits.
The session now retains every used path node in its returned evidence, including
locally reconstructed nodes, within the same witness budget. A 64-pending-edit
checkpoint regression covers both owner merges and final acknowledgement with no
remote node fetch. The final model rerun passed all nine tests; remaining focused,
browser and full CI reruns are pending. CR.41 remains unchecked.

CR.41 final source passed all eight repository CI stages (`cr41-ci-final.log`):
fmt, clippy, HEIF, workspace tests, parity references, layers, assets and WASM.
The final focused runs passed nine archive tests (`cr41-model-final.log`), both
publication-target/generated-evidence regressions (`cr41-publication-final.log`),
workspace clippy, and native 64/4,096-mutation bootstrap with 7/13 unique archive GETs
(`cr41-get-counts-accepted.log`). The browser bundle rebuilt successfully.

Chromium acceptance passed all six OPFS/IndexedDB cases after publishing the complete
64-rename tail before checkpointing (`cr41-browser-complete.log`). Conditional-head
joins used seven unique archive GETs; append joins used 64 during their existing
history audit, with no repeated node GETs within the measured join. Reload,
authorization rotation and native-peer convergence passed; all 3,012 requests were
independently signature-verified. Screenshots/status evidence were inspected.
Evidence: ignored `cr41-browser-results.json`, scratch
`lightcraft-cr41-browser/browser-2069a956/`. The earlier browser run covered only
the initial head prefix and is superseded by this stronger run.

CR.41 is complete and checked off. CR.34-CR.41 from the latest review are now complete;
the separate CR.9-CR.21 backlog remains unchecked. This does not establish a live-provider,
large-library latency/memory, append checkpoint-consumption or remote-reclamation result.


2026-10-16 - CR.9 implementation: derived reverse identity indexing replaces
repeated scans (including identity uniqueness checks); incoming projection selects
changed photos, reference dependencies and deterministic structural reconciliation.
The shared native/browser session boundary preserves writer reservation before
compacting full producer states into local-format-18 synchronization deltas.
Field/register, receipt, archive, pending and nested append-publication changes
carry predecessor checks, atomic final validation and inverses for batch rollback.
Remote formats/capacity limits are unchanged. Focused verification and explicit
10k/50k measurements are underway; full repository CI remains pending and CR.9
stays unchecked.

CR.9 focused acceptance: all 134 cloud engine checks passed
(`cr9-focused-accepted.log`), including exact-snapshot invalidation after a held
result, durable publication-permit handoff, copied/restored authors, smart rules,
structural convergence and upload finalization. The existing CR.32 test now commits
the logical local label with its outbox instead of constructing an unmaterialized
state-only fixture. The new held-result test commits shared metadata directly: the
command entry point polls ready jobs, and its local-only photo does not replicate.

Explicit Windows scale runs passed on the final product source. These are minimal
valid managed-metadata fixtures with bounded real mutations and a shared original
blob, not 50,000 distinct media uploads or fully populated recipes/history. Native
requests use the independently checked SigV4 fixture. Request/response counts below
cover three completed unchanged conditional-head polls, excluding setup, the separate
nonwaiting startup measurement and publication. Exact catalog snapshot/log bytes stay
unchanged. Native evidence: `cr9-scale-native-accepted.log`.

| Photos | Sync-state bytes | Catalog snapshot bytes | GETs / response bytes, 3 polls | Unchanged journal growth | Startup dispatch ms after open | Owner result application ms, range | Local rating + ACK journal bytes | Changed result application ms |
|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 10,000 | 4,294,569 | 36,713,527 | 6 / 2,220 | 0 | 85.6 | 0.0125-0.0349 | 4,031 | 315.9 |
| 50,000 | 21,574,169 | 183,793,127 | 6 / 2,223 | 0 | 394.7 | 0.0126-0.1011 | 4,032 | 1,818.8 |

Startup is nonwaiting command dispatch after catalog open, including ownership,
local save checks, snapshot preparation and worker spawn; closing that read-only
CLI session cancels its background poll. `ownerApplyMs` measures result comparison,
reconciliation, application, journal persistence and handoff; it excludes startup,
network/worker time, library opening and subsequent UI work. Native result snapshots
are freed by the existing worker after durable owner acknowledgement. Journal bytes
are observed through `library.info` before CLI-close compaction; measuring only the
post-close file would incorrectly report zero for the changed operation.

The shared-engine MemStore fixture (`cr9-scale-engine-accepted.log`) projected
one photo for a received rating change at both sizes and wrote 4,223 framed journal
bytes, versus full operation payloads of 3,322,824 / 16,730,024 bytes. Its state
sizes were 3,319,400 / 16,726,600 bytes; changed-result times were 360.9 / 1,876.2 ms.
The strict full-state unchanged comparison fallback took median 6.723 / 36.329 ms.
Reference-arrival coverage refreshes unchanged memberships, covers, copies, smart
selections and stacks; unaffected photos retain their existing Arc objects.

These measurements expose remaining linear full-state merge, validation, snapshot
preparation/container-copy and snapshot-writing costs; they do not establish an
interactive-latency budget or larger populated-library capacity. The 64 MiB state
limit remains unchanged. Append checkpoint/audit request scaling remains CR.10.
Shared browser code compiles for WASM; this run adds no browser scale or live-provider
latency claim. Full repository CI is the remaining gate; CR.9 remains unchecked.

CR.9 final verification: all eight full repository CI stages passed
(`cr9-ci-accepted.log`: fmt, clippy, HEIF, workspace tests, parity, layers, assets,
WASM). All eight catalog delta regressions passed with nonempty retained append
proofs, format-17 upgrade, field-sized album membership updates, journal replay,
base mismatch rejection and atomic rollback (`cr9-delta-accepted.log`). The 134
focused cloud checks and both explicit scale runs above passed. The initial CI
run was stopped to correct the held-result test setup; it is superseded by this
complete final-source run. CR.9 is complete and checked off. CR.10-CR.21 remain
open; the measured startup/changed-result costs and scale limitations above remain
explicit, rather than claiming general large-library responsiveness.
