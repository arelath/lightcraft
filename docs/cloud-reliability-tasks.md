# Cloud storage reliability tasks

Source: supplied static review of `cloud-support` at `208edb8`.
Created: 2026-10-14. Check an item only after implementation and relevant verification;
record evidence and remaining limitations below it. Historical acceptance does not verify
this checkout. Work in the order below, preserving unrelated changes.

Current status (2026-10-17): CR.1–CR.42, CR.46–CR.47, CR.49, CR.53, CR.56–CR.62 are complete.
CR.43–CR.45, CR.48, CR.50–CR.52, CR.54–CR.55, CR.63–CR.74 remain open.
Dated verification notes below
describe their original snapshots; current operational limits remain in
[cloud-storage-implementation.md](cloud-storage-implementation.md).

- [x] CR.1 — Restore synchronization liveness with failed, deleted, orphaned, or other-connection uploads. Receive independently of publication; gate publication on exact media dependencies. Preserve append causal continuity by publishing frozen imports before tombstones. Test deletion before upload and unrelated failed uploads.
- [x] CR.2 — Fix automatic multi-photo append imports. Represent dependency waiting separately from failure and reconcile every upload intent covered by verified publication. Test delayed sibling transfers and restart/retry.
- [x] CR.3 — Materialize valid folder rearrangements independently of entity ID order. Validate the desired graph, detach before attaching, and resolve concurrent cycles deterministically while retaining conflict choices. Test reversed ancestry and concurrent cycles; audit stack uniqueness.
- [x] CR.4 — Bound import replica identities. Route all current imports through normal causal mutations; reject the obsolete synthetic-author format across an explicit pre-release version boundary. Test more than 4,096 managed photos without widening the device limit. The maintainer confirms there are no users or existing libraries to migrate, only disposable test data (2026-10-14).
- [x] CR.5 — Specify and implement causal-history compaction, including retired replicas, tombstones, duplicate detection, and old offline clients. Test checkpoint maintenance restores editing capacity without losing conflict or deletion safety.
- [x] CR.6 — Expose synchronization-state utilization, early warnings, and accurate recovery errors before record/byte bounds.
- [x] CR.7 — Define and test oversized-entity bootstrap behavior, especially large album membership sets; implement streaming or an explicit actionable product limit.
- [x] CR.8 — Make unchanged conditional-head synchronization a genuine local no-op (no materialization, state operation, or journal write without change, acknowledgement, or progress).
- [x] CR.9 — Index global-to-local identities once, materialize changed entities, and persist synchronization deltas. Measure unchanged 10,000- and 50,000-photo request counts, journal growth, state size, and owner-thread latency.
- [x] CR.10 — Track consumed append checkpoints and avoid downloading unchanged covered history. Separate budgeted integrity audits from ordinary polls; test remote request counts.
- [x] CR.11 — Enforce one immutable published asset-manifest contract with device-local connection binding; test permitted/rejected descriptor changes and extract shared blob rebinding.
- [x] CR.12 — Replace subtractive catalog JSON projection/materialization with explicit portable photo, content, album, and reference types and typed boundary conversions; retain wire compatibility tests.
- [x] CR.13 — Extract testable upload readiness, transfer, verification, dependency waiting, publication, acknowledgement, and failure transitions; test deletion/restart behavior.
- [x] CR.14 — Consolidate current behavior/limitations, protocol specification, and dated acceptance evidence. Remove obsolete current-status claims and distinguish historical verification.
- [x] CR.15 — Persist live offline owner-plus-quality policy, derive dependencies after membership/content changes, and allow empty albums to be pinned. Test new members and asset revisions.
- [x] CR.16 — Add a command to move existing local photos into managed cloud storage while preserving photo IDs, virtual copies, edits, albums, and history. Test identity/history preservation and restart.
- [x] CR.17 — Handle browser cache exhaustion with safe verified-blob eviction, catalog/outbox space reservation, and temporary viewing versus guaranteed offline retention. Test quota failures and pinned data preservation.
- [x] CR.18 — Make cloud deletion retention prominent and add explicit previewable retention/reclamation tooling with safe reference checks.
- [x] CR.19 — Align and expose native input, pixel, and blob limits; validate representative large `.3fr` inputs before migration and test actionable limit errors.
- [x] CR.20 — Provide supported browser signer deployment instructions and a desktop/browser capability matrix; verify the deployment workflow and stated scope.
- [x] CR.21 — Classify sync failures and retry transient connectivity with bounded backoff; pause authentication, integrity, schema, and identity failures with actionable status. Test native/browser recovery.


## Follow-up review: local conversion, recovery and focused UX (2026-10-17)

Source: supplied source-level review of `b0fd344b` (CUX.12). Its scenarios have not
been treated as executed reproductions. Additions were recorded before implementation.
Existing CR.43–CR.45 and CR.48–CR.55 retain their IDs and unchecked status; this review
reaffirms them rather than creating duplicate tasks. Prioritize CR.58/59/60, then
CR.49/53, CR.61 and the recovery/UI work. CR.62 can land independently. Preserve
existing mutation identities, immutable media, conflict alternatives and offline copies.

- [x] CR.58 — Carry a typed managed-preparation kind through background completion so local XMP, DNG, external-editor copy and reload revisions commit while sync is off, without authorizing explicit cloud enrollment. Reproduce `wait:false` on a converted library and assert zero network requests.
- [x] CR.59 — Certify the final cloud-to-local result against the original owner snapshot, retaining all required archive proofs and append publication state across repeated receive-only scans. Exclude transport proofs from stability fingerprints. Test an older device converting after another device compacts receipts.
- [x] CR.60 — Separate permanent-local editing from the bounded cloud outbox. Durably preserve the rejoin baseline, pending mutation identities and local changes without eventually blocking local edits at 10,000 pending mutations; explicitly reconcile on re-enable. Test the capacity boundary, restart, rejoin and conflicts.
- [x] CR.61 — Drain causally ready append operations during bounded discovery on native and browser; share discovery planning while keeping platform I/O separate. Retain integrity checks and bounds for unresolved dependencies. Test a fresh reader catching up with 300 sequential 256 KiB caption replacements and cross-author ordering.
- [x] CR.62 — Persist a stable absolute permanent-original destination before entering conversion, including relative `--library` paths; retain symlink/junction validation. Test conversion and offline reopening of `./photos`.
- [ ] CR.63 — Add library-wide error Details with the original diagnostic, affected operation, retry state and cause-specific recovery. Use a typed UI status/action model across popup, Info and focused dialogs; offer Edit connection only for relevant configuration/authentication failures. Test an empty library and integrity/capacity/schema/disk errors.
- [ ] CR.64 — Make saved keys, replacement keys and AWS profile explicit authentication choices; use the selected source consistently for probing and persistence. Test replacing saved-key credentials with a different profile.
- [ ] CR.65 — Add bounded cursor pagination and search for conflicting fields, retained choices and deleted photos. Ensure conflict 17, value nine and deleted item 201 are reachable without resolving/restoring earlier items.
- [ ] CR.66 — Derive Info and popup Sync/Retry/Resume labels and commands from shared state, including user pause. Test Info resumes paused sync rather than dispatching a rejected ordinary sync.
- [ ] CR.67 — Reset retention's all-writers-paused assertion on closing/reopening or resuming; retain running-task identity separately. Test reopening requires fresh operator confirmation before reclamation.
- [ ] CR.68 — Route control-channel and widget dialog confirmation through one lifecycle handler, preserving cloud dialogs and values on errors. Test both paths produce the same recovery state.
- [ ] CR.69 — Extract managed media preparation/commit from cloud enrollment into an explicit subsystem with typed operation kinds, preserving durable staging, retries and ordinary local revision behavior. Keep public command/API compatibility.
- [ ] CR.70 — Preserve typed compatibility, control and capacity failures through head decoding, scope validation, transport adaptation and append planning to health/presentation. Test complete transport-to-health paths, including newer-reader update recovery.
- [ ] CR.71 — Replace unsupported backup assurance with precise Up to date/Synchronized status; expose secondary Sync now and accurate healthy/paused/retrying/read-only/local-only information using the shared model. Verify representative popup states.
- [ ] CR.72 — Make conflict choices distinguishable with full readable metadata, useful device/version details and Compare/Preview for develop alternatives. Keep every retained alternative navigable and resolution explicit.
- [ ] CR.73 — Define and implement Recently Deleted behavior during cloud-to-local conversion. Include recoverable deleted originals by default (with count/space in the review) so local-only restoration works offline; preserve retained content alternatives and test restore after conversion.
- [ ] CR.74 — Explain re-enabling a local-only library as a merge with its existing library/connection, preserving local/new photos, cloud changes and retained conflicts. Test the re-enable confirmation and resulting merge semantics.

Reaffirmed work at intake (current completion status is above): CR.49 successor exposure/cancellation, CR.53 adopted-destination
and lease-aware cache cleanup, CR.48 append epoch routing, CR.50 publication capability
on every outcome, CR.51 lease renewal throughout expensive upgrade work, CR.52 browser
blob/index recovery, CR.54 credential-record isolation and CR.55 paginated validated
library discovery. Regression acceptance must cover cancellation after each successor
stage, interrupted browser publication and forced cache-move fallback. CR.43's exclusive
append barrier remains open; CR.44 requires an actual schema converter and CR.45 requires
current live-provider/browser acceptance, so historical fixture success cannot close them.

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

## Format upgrade framework (pre-release)

Source: maintainer-supplied migration design, received 2026-10-16. Starting commit:
`c3d3d687`. Only disposable test data uses the current formats, so the control envelope
is introduced before the first published release, while every client can still be made to
honour it. Specification: [cloud-sync-protocol.md](cloud-sync-protocol.md#library-control-and-format-upgrades-2026-10-17).

- [x] CR.42 — Add a stable library control envelope with separate reader/writer compatibility, carried by a version-4 conditional head and enforced on every reread before each native/browser CAS (receive-only while blocked). Implement CAS-confirmed maintenance transitions (begin, takeover after lease expiry, cancel, rebase activation into a new epoch namespace, append retirement) behind `cloud.upgradeLibrary`, epoch rejoin with unchanged pending mutations, and cooperative append-log control records. Test two upgraders, a commit racing maintenance, crashed-upgrader takeover, a lost activation response and an offline client returning with pending edits.
- [ ] CR.43 — Make append-log maintenance a hard barrier: a provider-specific control adapter that elects one upgrader and serializes metadata commits (prototype B2 `b2_update_bucket` with `ifRevisionIs`, preserving unrelated bucket info and requiring `writeBuckets`; or a small commit coordinator), plus an append upgrade runner and discovery of late operations written to a retired namespace. Test simultaneous upgraders and a late old-epoch write being recovered, not ignored.
- [ ] CR.44 — Implement the first real mutation translation only when a schema change needs it: preserve the original mutation identity/digest, create translated mutations with a durable original→translated provenance/deduplication map, and test a crash between translation and publication (no duplicate edit, no identity collision).
- [ ] CR.45 — Run the upgrade workflow in the desktop and browser UIs against live AWS S3 and (after CR.43) B2, inspect Activity/Info states, and record request costs for begin/activate/rejoin.

## Sync UX review (2026-10-17)

Source: static review of `cloud-support` at `a883fbfb`.

- [x] CR.46 — Let people resolve every retained field conflict (not only develop) from Cloud Synchronization and Info; show a refused publication (maintenance, moved library, newer writer) in the popup, Info and attention state on desktop and browser; replace the per-frame, unbounded deleted-photo scan with a cached, capped list; translate the popup's counts and show the last remote check as a readable age.
- [x] CR.57 — Keep automatic polling alive in an idle window: desktop and browser hosts schedule a repaint for the next poll, retry or maintenance deadline; the browser postpones (rather than halts) automatic polling while its task queue is full; a failing library save postpones polling instead of halting it; native Reconnect restarts a halted scheduler like the browser's.
- [x] CR.56 — Give every cloud failure advice that matches its cause: per-failure recovery for newer formats (update LightCraft), a moved library (Migrate Cloud Library… to rejoin), maintenance and HTTP 301/307/400/404/413 (check bucket, prefix, region, endpoint); keep catalog failures typed (synchronization limits → capacity, catalog I/O or corruption → local resource, newer catalog → unsupported) instead of invalid history; word advice for the Retry Syncing / Resume Syncing buttons and name buttons, not only command ids, in errors.

## Follow-up review: upgrade safety and durable cache recovery

Source: supplied static review of `cloud-support` at `a883fbf` against `0abef0a`,
received 2026-10-16. Starting commit: `edc556a0`. These findings are proposed
regressions from a static review, not executed acceptance. Preserve the confirmed
CAS transitions, ownership tokens, typed failures and browser byte leases.
CR.43–CR.45 remain separately open; do not count their deferred work as complete.

- [x] CR.47 — Resolve migration capability independently of ownership before acquisition or takeover. Introduce a typed specification for plan, actual source/target schemas, converter and activation reader/writer requirements. Reject a newer unsupported target before any control transition or migration output; test an expired maintenance record targeting `SCHEMA + 1` and supported identity rebases. Cross-schema translation remains CR.44.
- [ ] CR.48 — Preserve the append control's effective epoch/root through synchronization. Until epoch routing and discovery-progress reset are implemented, reject unsupported namespace transitions before publication. Test readable Active epoch-two control and assert no operation PUT to epoch one; preserve typed compatibility/publication decisions.
- [x] CR.49 — Define durable successor-exposure stages and cancellation rules for head-to-append migration. Permit cancellation only while the old library can remain usable; after successor exposure require completion or explicit recovery. Never delete a possibly used successor blindly. Test interruption/retry/cancel after manifest, checkpoint and seed publication and before retirement, with expected remote states and next actions.
- [ ] CR.50 — Carry publication capability in every head outcome, including unchanged scans and clean clients with no pending work. Preserve maintenance/newer-writer/retirement reasons and slower polling without no-op journal writes. Test repeated clean-client polls under each readable but nonpublishable control state.
- [ ] CR.51 — Renew upgrade leases during expensive source reads, verification and checkpoint/archive writes. Bind renewals to the current token/revision; stop output after cancellation or ownership loss. Test a healthy paused worker over a lease interval with an injectable clock, takeover after renewal stops and refusal of stale activation.
- [ ] CR.52 — Recover browser blob/index publication across interruption or policy-save failure with a durable transaction boundary or bounded verified reconciliation of the owned namespace. Preserve all libraries' offline protections. Inject failure between blob publication and index persistence, reload and verify actual storage usage plus safe Clear cache/quota reclamation.
- [x] CR.53 — Finish cache-relocation cleanup safety. Verify the reopened resolver adopted the intended destination before removing old files, and use lease-aware cleanup. Test destination fallback and outstanding old byte leases; a successful reopen alone cannot authorize deletion.
- [ ] CR.54 — Isolate credential records between independent libraries sharing a connection name. Preserve validation-before-save for rejected retargets and same-destination credential rotation. Test two libraries with the default connection name and distinct credentials without overwriting each other's profile.
- [ ] CR.55 — Make guided library discovery authoritative and paginated. Validate manifest/control records, resolve retained/retired head versus successor protocol, and process every listing page under explicit bounds. Test invalid prefixes, head retirement, duplicate identities and libraries beyond the first page.

Refactoring acceptance is included in the corresponding tasks: CR.47 owns typed
migration capability, CR.48/CR.50 retain effective control decisions, and
CR.47/CR.49/CR.51 exercise mixed capabilities and intermediate migration stages.

## Verification log

This is chronological evidence. Earlier statements that a task is pending or open
describe that run; the checkboxes above are the current completion state. Historical
provider and milestone reports are collected in [cloud-acceptance-history.md](cloud-acceptance-history.md).

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


2026-10-16 — CR.10 implementation and focused verification:

Append progress now retains only locally verified checkpoint keys and a resumable historical
identity-audit round/key cursor. Ordinary discovery still rescans all checkpoint pages and author
roots, then reads unseen checkpoints and operation tails strictly beyond certified publication
frontiers. Audit/checkpoint requests retain the previously verified remote projection instead of
rebuilding it from zero. Old identities, including forks below the tail boundary, are verified in
separate slices. Native and browser share the same 64-record, 16-list-page and 8 MiB payload-stop
policy. A single object may exceed the byte target up to its existing object bound, and authenticated
archive nodes retain their separate path/cache bounds. New data/bootstrap is outside this historical
budget. No remote records are removed or receipt protections relaxed.

Audit progress is durable and monotonic under late completions. Consumption hints from remote
checkpoints are ignored and local hints are excluded from published checkpoints and publication
proof hashes. Local format 19 protects older readers; journal patches record new checkpoint keys
and cursor changes without copying the consumed-key set. Status and sync reports expose audit
round/cursor/completion and consumed checkpoint counts.

The 17 real-CLI append regressions passed (`target/cloud-reliability/cr10-cli-accepted.log`). At 16
and 256 seeded historical mutations, one complete audit plus three idle polls fetched each operation
and checkpoint exactly once across the whole sequence. A 160-record audit resumed across separate
CLI processes with at most 64 history GETs per slice. A later old-identity fork and corruption of an
already-consumed checkpoint failed without changing durable progress or pending work. Existing
causal gaps, paginated author discovery, immutable publication, lost responses, retained deleted
imports, shared bulk ACKs and restored-author regressions also passed. The old root-rescan test
allowed audit discovery in the same scan, while retaining its next-scan convergence assertion.
The 31 catalog replication tests passed, including monotonic audit merge, legacy wire compatibility
and bounded journal delta/replay/inverse checks (`target/cloud-reliability/cr10-catalog-final.log`).
All 238 catalog tests also passed (one explicit benchmark ignored; `target/cloud-reliability/cr10-catalog-all.log`).
A real-CLI late-checkpoint fixture reproduced stranded staged-operation gaps before the correction.
Audit now defers an unseen checkpoint to the next discovery pass, which merges it before staging
operation tails. The regression passes after the correction. A preliminary full CI run passed all
eight gates before this final race correction; final-source CI and browser acceptance are pending.
CR.10 remains unchecked until both finish.


Final-source browser acceptance passed in actual Chromium on OPFS and IndexedDB, using the native
signed fixture with a 162-operation compacted album history (`target/cloud-reliability/cr10-browser-pass.log`,
`cr10-browser-results.json`; scratch `C:/Users/pdykstra/AppData/Local/Temp/lightcraft-cr10-browser/browser-7afbe3eb/`).
Each browser stopped at sequence 63, reloaded, resumed the same audit round with 64 then 35 history
GETs, and completed at sequence 162. No operation/checkpoint was downloaded twice across those
resumed slices. Three subsequent automatic polls in each backend used 12 requests total (manifest
and discovery LISTs) and zero operation, checkpoint or receipt-node GETs. All 2,269 fixture requests
were signed; no browser page errors occurred. Rendered app states were inspected (`opfs-audit.png`,
`idb-audit.png`), including the paused cloud popup. The harness was corrected to observe the automatic
poll started by Resume rather than racing it with a second explicit Sync. The final-source optimized
WASM bundle built successfully (`cr10-web-final.log`). Final-source full CI remains the last gate.

Limitations: checkpoint/root/author listings and state merges remain bounded linear work. A completed
poll does not claim a completed history audit; the round/cursor/completion status is explicit. Requests
for new data/bootstrap and archive-witness fetches have their separate existing bounds. This acceptance
uses loopback storage and a small metadata fixture; it is not live-provider or 50,000-photo performance
evidence. Remote retention/reclamation policy is unchanged.


Final CR.10 gate: `cargo +1.95.0 xtask ci` passed all eight steps on the final source
(fmt, clippy, heif, workspace tests, parity, layers, assets, wasm;
`target/cloud-reliability/cr10-ci-verified.log`). This supersedes the earlier preliminary/stopped
runs. No source changes followed this passing run. CR.10 is complete and checked off;
CR.11–CR.21 remain unchecked and will be handled separately.


2026-10-16 — CR.11 implementation and focused verification (completion pending):
`Asset::same_manifest` defines the immutable original/rendition/supporting-document
contract independently of device connection names. `SetAsset` rejects changes to
bytes, sizes, keys, object versions or optional manifest members under an existing
identity. Changed renditions use a new asset identity. Manifests freeze at first
catalog commit; production import/revision preparation already completes them before
that commit. `Asset::rebound` validates and binds every blob, and is shared by
portable projection, incoming materialization and native/browser dependency checks.
This corrects browser publication leaving XMP bound to the portable connection.
The wire/catalog layout is unchanged.

Five catalog tests passed, including a descriptor-change matrix, rejected-operation
atomicity, new-identity replacements, idempotence, connection-only inverse/replay and
compaction (`target/cloud-reliability/cr11-catalog.log`). Focused engine tests passed
75 with one explicit scale benchmark ignored (`cr11-engine.log`), covering incoming
manifest rejection/rebinding and cache invalidation when a photo adopts a new
rendition manifest. The real-CLI XMP revision/fresh-join test passed in both protocols
using different local connection names and publication of a virtual copy back to
the creator (`cr11-cli-xmp.log`). The previous browser bundle reproduced the reported
failure in actual Chromium: publishing a fresh-device virtual copy failed with
"associated documents must use the original's connection" (`cr11-browser-before.log`).
Final-source browser acceptance and full repository CI remain pending; CR.11 is
still unchecked until they pass.


CR.11 follow-up before the final gate: connection-only rebinding now updates the
same asset's device-local working-copy connections atomically. Its inverse restores
them, and the catalog regression verifies a subsequent working-copy operation,
journal replay and compaction. This prevents a latent binding mismatch on future
local edits. The preliminary CI run was stopped before changing this source;
`cr11-ci-final.log` is the final-source gate.


CR.11 final acceptance: `cargo +1.95.0 xtask ci` passed all eight gates on the
final source (fmt, clippy, heif, workspace tests, parity, layers, assets, wasm;
`target/cloud-reliability/cr11-ci-final.log`). The final optimized WASM bundle also
built (`cr11-web.log`). Actual Chromium passed both protocols on both OPFS and
IndexedDB, with native and browser connection names deliberately different
(`cr11-browser-final.log`, `cr11-browser-results.json`). Every virtual-copy publication
verified original, smart preview, thumbnail and supporting XMP; reload retained the
copy and the native creator received it. All 175 fixture requests were signed and
there were no browser page errors. Rendered OPFS/head and IndexedDB/append app states
were inspected in scratch
`C:/Users/pdykstra/AppData/Local/Temp/lightcraft-cr11-browser-final/browser-2b18f6bc/`.
This is loopback-provider correctness acceptance, not a live-provider or scale claim.
No source changes followed the passing CI run. CR.11 is complete and checked off;
CR.12–CR.21 remain open.


2026-10-16 — CR.12 implementation, final gate pending: explicit portable Photo,
Content, Meta, Album, Stack, Filter, rule-group and global-reference types replace
whole catalog Photo/Album serialization and JSON subtraction/reconstruction.
Incoming materialization applies only typed shared fields to local seeds. Named
photo/album reference slots handle deferred delivery and retained tombstones;
arbitrary rule payload properties are no longer recursively rewritten. Valid
wire names, field omissions, flattened metadata defaults, sparse content fallback
and restricted empty selections retain their previous representation. Unknown
smart-filter fields and malformed global references now fail explicitly. File-path
rules and device-local sync-status filters, plus dependent smart albums, remain
local under the existing nonportable-smart-album policy.

Focused verification: Clippy passed with warnings denied (`cr12-clippy.log`);
46 synchronization tests passed, with one explicit scale benchmark ignored
(`cr12-engine-final.log`). New tests compare typed output with the frozen legacy
projection (including exact serialized photo bytes), cover sparse/default/optional
fields and album/stack wire shapes, reject private incoming fields atomically,
retain unresolved/negated references, bound nested rule decoding, and preserve
literal payload properties outside reference slots. Existing convergence, undo,
smart-album bootstrap and delivery-order regressions passed. Full repository CI
and browser compatibility acceptance remain pending; CR.12 is unchecked.


CR.12 final acceptance: all eight repository CI gates passed on the final source
(`target/cloud-reliability/cr12-ci.log`), followed by a successful optimized WASM
build (`cr12-web.log`). Actual Chromium passed conditional-head and append
publication on both OPFS and IndexedDB against the frozen CR.11 CLI binary as
producer and receiver (`cr12-browser.log`, `cr12-browser-results.json`). This checks
old payloads arriving at the typed codec and new typed payloads returning to the
previous codec, including all four media dependencies, virtual copies and reload.
All 175 fixture requests were signed; no browser page errors occurred. Rendered
OPFS/append and IndexedDB/head states were inspected under
`C:/Users/pdykstra/AppData/Local/Temp/lightcraft-cr12-browser/browser-e40f7089/`.
No source changes followed the passing CI run. CR.12 is complete and checked off.
This is fixture interoperability evidence; it does not assert live-provider or
large-library performance. CR.13–CR.21 remain open.


2026-10-16 — CR.13 implementation, final CI pending: `cloud_upload/transitions.rs`
extracts pure retained-content readiness, attempt/library/connection/asset phase
acceptance, one-batch destination/library/protocol/clock binding, dependency
waiting versus exact publication coverage, and receipt-finalization decisions.
The existing transfer/verification stages construct `VerifiedUpload` only after
fresh byte verification. The owner retains catalog apply, journal persistence,
rollback and reply ordering. Written receipt proofs are validated before they can
clear newer durable work. No wire or local catalog layout changes.

Clippy passed with warnings denied (`cr13-clippy.log`); 25 focused upload tests
passed (`cr13-engine-final.log`). New regressions run the transfer/verified/published/
synchronized phases after soft deletion, permanent deletion and relinking in both
protocols, replay the journal mid-flow, reject stale scope/attempt/asset failures,
retain newer receipt proofs and distinguish deliberate remote damage. The actual
owner event queue also accepts a verified phase after those three local changes,
persists and reopens the library with the frozen outbox intact and without
recreating the photo or changing its replacement source. Existing full-disk,
receipt-write failure/restart, preflight, shared-ACK and slider-group tests passed.
An intermediate focused build was stopped to adjust the test helper's migration
probe for already-append libraries; the final focused run above passed. Full
repository CI then passed all eight gates on the final source (`target/cloud-reliability/cr13-ci.log`),
including real CLI bulk request/byte assertions, deletion and process-restart recovery.
CR.13 is complete and checked off. CR.14–CR.21 remain open. This is fixture acceptance;
no fresh live-provider or large-library claim is made.


2026-10-17 — CR.14 implementation: current behavior and explicit remaining limits now
live in cloud-storage-implementation.md, normative synchronization rules in
cloud-sync-protocol.md, and browser authorization/storage rules in browser-cloud.md.
cloud-acceptance-history.md preserves the dated native, browser, B2 and migration
reports with their provider/account and fixture limitations. The original milestone
index and reliability checkbox list have distinct roles. Removed superseded current
claims about import-only publication, missing edit sync/compaction/external-editor
ingestion, future CatalogSync, completed CR.9/CR.10/CR.31 work and local format versions.
Links and current claims reviewed, including the historical proposal banner. Local
Markdown file and heading targets resolve. Full repository CI passed all eight gates
(`target/cloud-reliability/cr14-ci.log`); no executable source changed. CR.14 is
complete and checked off. CR.15–CR.21 remain open. This documentation consolidation
adds no provider, camera or scale acceptance claim.


2026-10-17 — CR.15 implementation, verification pending: native retention policy v3
persists photo/album owners and quality, with current blob dependencies derived from
the catalog rather than saved membership snapshots. Empty albums retain intent.
The central session boundary covers received changes, smart rules/metadata, source
revisions and undo/redo. Prior and new dependencies remain protected until catalog
append succeeds; reopen defers eviction until owner and library-wide policy resolve.
One offline worker handles live owners and library-wide copies; explicit reconnect
retries failed transfers. Missing preview renditions retain policy and expose a
recovery reason in cache/storage status and Settings → Cache. CR.15 remains unchecked.


2026-10-17 — CR.15 final verification: 41 focused asset/cache tests passed
(`target/cloud-reliability/cr15-tests-verified.log`). Six new regressions cover empty
albums, membership and smart-rule/metadata changes, original/preview revisions,
failed journal append/retry, undo, restart, missing preview recovery, and restoring
an evicted member while another download is held. The final-source full CI also
passes live photo/album pins through cache relocation with a zero-byte budget.
All eight repository CI gates passed (`target/cloud-reliability/cr15-ci.log`).
Native-only WASM helpers retain platform dead-code warnings; native strict Clippy
passed. No executable source changed after the final CI build started.

Actual desktop control-channel acceptance pinned an empty album, added an original
without a preview, inspected the explicit missing-preview reason in Settings →
Cache, changed the policy to original quality, and retained it through Clear cache.
With storage stopped, process restart resolved the live owner and exported 32×24
from verified offline bytes. The version-3 policy retained only owner/quality and
empty dependency arrays. All 17 signed-fixture requests verified. Warning/recovered/
offline screenshots were inspected in
`C:/Users/pdykstra/AppData/Local/Temp/lightcraft-cr15-native/native-6345d8c7/`;
results are `target/cloud-reliability/cr15-native-results.json`. Sampled Settings
updates were 40.6/43.8 ms with no queued/in-flight renders while CI was running;
this tiny procedural fixture is not a performance or live-provider benchmark.
Owned app/fixture processes were stopped. CR.15 is complete and checked off.
CR.16–CR.21 remain open.


2026-10-17 — CR.16: `cloud.adopt` and File ▸ Move Selected Photos to Cloud Storage
stage existing originals, merged supporting XMP and renditions on the durable import
worker, then relink the existing records. Live virtual-copy families retain separate
recipes, metadata, IDs, album membership and photo history. Current edits survive
preparation; no import defaults or stale sidecar settings are reapplied. Original files
remain on disk. Version-4 local manifests add an explicit adoption target; wire records
and receipt formats are unchanged. Current causal receipts bind publication through
exact mutation evidence rather than assuming request ID equals photo identity.

Durable selection-keyed requests resume partial batches and restart recovery. Owner
source/fingerprint guards reject intervening relinks; changed/unreadable originals have
per-photo retry actions without stopping healthy siblings. Undo retains immutable
publication resources while returning records to local files; later metadata on the
withdrawn tombstone cannot relink or delete that local record. Legacy initial-import
receipt constraints remain enforced. Focused evidence: 35 managed-import tests passed
(`target/cloud-reliability/cr16-tests5.log`), then all nine final adoption, publication
and materialization regressions passed (`cr16-tests-final2.log`), including partial retry.

Final rebuilt native control-channel acceptance passed both protocols. The rendered
confirmation button performed adoption; local catalog records (apart from source),
albums and original files were compared. Retry after restart reused one request and
asset, and a fresh catalog received both photos with their virtual-copy relationship.
Undo before upload completed publication/withdrawal, retained the local-file photo,
and left a fresh peer empty. All 149 signed fixture requests were independently verified
(63 head / 86 append). Screenshots were inspected. Evidence: `target/cloud-reliability/
cr16-native-results-conditional-head-v1.json`, `cr16-native-results-append-log-v1.json`,
`cr16-native-head-verified.log`, `cr16-native-append-verified.log`; temporary screenshots
in `C:/Users/pdykstra/AppData/Local/Temp/lightcraft-cr16-native/native-e88cf2f0/` and
`native-94f90dda/`. These procedural fixture checks do not establish live-provider or
large-library performance. Full final-source repository CI passed all eight gates
(fmt, strict Clippy, HEIF, workspace tests, parity, layers, assets and WASM):
`C:/Users/pdykstra/AppData/Local/Temp/lightcraft-reliability-target/cr16-ci-final.log`.

2026-10-17 — CR.17: browser persistent caching now has a verified-entry LRU index,
origin-wide Web Lock coordination, catalog/outbox headroom, live photo/album offline
intent and closed-library protection. Only indexed cloud blobs are disposable; imported
originals, unknown files, catalog/ACK/outbox data and pinned dependencies are excluded.
A cache write failure retains verified session bytes for viewing/export. Offline readiness
requires durable intent/catalog and verified persistent bytes. Renderer source misses use
the shared admission queue and receive bytes without depending on a successful disk write;
unchanged decoded sources are not transferred again on each edit. Explicit unpinning can
release space while another catalog save is blocked, conservatively retaining other owners.

All 42 web library tests passed, including generated eviction/protection cases, future/
malformed policy rejection and backed-up empty-album owner/quality intent. Explicit web
WASM Clippy passed. Full `cargo +1.95.0 xtask ci` passed all eight stages; final log:
`C:/Users/pdykstra/AppData/Local/Temp/lightcraft-reliability-target/cr17-ci.log`.
Final Chromium acceptance passed OPFS and IndexedDB with both protocols: 265 independently
SigV4-verified fixture requests. It covered failed cache writes with successful worker
viewing/export, pin repair without another original GET, empty-album membership changes,
zero-budget/clear protection, closed-library pins, eligible-cache eviction on catalog quota
failure, original/library sentinel preservation, source-transfer counts across edits,
unpinning during failed saves, offline export after reload, retained pending mutations and
native-peer convergence. Actual OPFS quota reduction and a fresh IndexedDB database under
an 8 KiB quota exercised browser quota enforcement; IndexedDB reported asynchronous
`QuotaExceededError` and retained unsaved albums. Controlled write failures provide the
repeatable recovery tests: Chromium caches admission for already-open IndexedDB buckets,
so reducing their quota is not a reliable way to force a later small write to fail.

Evidence: `target/cloud-reliability/cr17-browser-final7.log`, `cr17-browser-results.json`,
`cr17-web-tests.log`, `cr17-wasm-clippy.log`; inspected screenshots and PNG exports in
`C:/Users/pdykstra/AppData/Local/Temp/lightcraft-cr17-browser/browser-4dc625c2/`.
Earlier fixture attempts incorrectly expected dynamic IndexedDB quota changes to reject
already-admitted writes; those runs were not counted as passes. This is procedural fixture
acceptance, not fresh live-provider or large-library acceptance. Browser persistence can
still be denied or site data removed externally. Backup intent is codec-tested; fresh
backup/restore UI acceptance was not run for this task.
2026-10-17 — CR.18: deletion dialogs, the cloud popup and Cloud Synchronization
now state that published originals, earlier revisions, tombstones, XMP and metadata
history remain retained and billable. Native `cloud.reclaim` and the rendered retention
section provide reviewed orphan-media preview/apply. Complete bounded inventories,
local staging, every historical operation, exact checkpoint receipts and authenticated
archive leaf/context commitments protect references. Missing/corrupt/inconsistent
history fails before deletion. Apply requires the exact reviewed token and all writers
published/stopped, freezes local mutation/library switching, rechecks object identity,
and uses ETag-conditional deletion with absence readback and no unconditional fallback.
AWS default endpoints and loopback fixtures support apply; B2/custom endpoints are
preview-only. Published history and supporting XMP never expire through this tool.

All four focused retention tests passed, including rewritten operation/context forks,
wrong archive ranges and edit/cancellation guards (`target/cloud-reliability/
cr18-proof-unit.log`). Full repository CI passed all eight gates on final executable
source: `C:/Users/pdykstra/AppData/Local/Temp/lightcraft-reliability-target/
cr18-ci-proof.log`. Its two real-CLI retention regressions passed both protocols,
covering published deletion/emptying, changed previews, version/future exclusions,
HTTP 501/412 refusal, exact surviving bytes and unreadable metadata/history.

Final rebuilt desktop acceptance passed both protocols with 141 independently
SigV4-verified fixture requests (66 head / 75 append). Actual preview/confirmation/apply
controls removed only the reviewed orphan; every other remote object was unchanged,
the source file survived, and a fresh peer still opened the published photo. Final
retention and deletion-warning screenshots were inspected. Evidence:
`target/cloud-reliability/cr18-native-head-proof.log`, `cr18-native-append-proof.log`,
`cr18-native-results-conditional-head-v1.json`, `cr18-native-results-append-log-v1.json`;
screenshots in `C:/Users/pdykstra/AppData/Local/Temp/lightcraft-cr18-native/native-29a9967f/`
and `native-1f95eeb6/`. This verifies the procedural fixture workflow, not fresh live
AWS/B2 acceptance. Bounds and stopped-writer requirements are documented in
[cloud-retention.md](cloud-retention.md).

2026-10-17 — CR.19: `cloud.overview.inputLimits`, managed-import/adoption and
synchronization dialogs now expose shared byte/pixel boundaries. Native original input
is bounded to 512 MiB before file allocation and 128 MP / 65,535 per edge before full
development; RAW admission counts sensor borders before crop. Managed descriptors use
the same 128 MP boundary. Browser originals retain the 512 MiB transfer/admission limit
and use a 32 MP decode budget. The 4 GiB streamed wire bound is explicitly separate.
Render, probe, merge, DNG conversion, denoise and original-reader paths share guards.
Custom/resident readers are checked on return. Some vendor RAW header probes unpack
CFA samples; these limits do not promise allocation-free probing or bounded render latency.

Oversized shared photos use smart previews for ordinary browser rendering even when
the original is cached. Full-quality export reports an actionable original-limit error;
explicit preview export stays at preview resolution. Original-quality offline selections
also protect the required preview, including whole-library policy. Missing required
previews keep readiness false without relaxing protection of the original. A focused
regression covers photo/all-original/all-preview owners, and the seven live-pin tests pass
(`target/cloud-reliability/cr19-pin-tests-final2.log`). Existing missing-Preview repair
diagnostics were preserved after an initial CI assertion caught their changed wording.
All 42 browser library tests passed (`cr19-web-tests-final.log`).

The SHA-256-pinned CC0 Hasselblad X2D 100C 16-bit `.3fr` is 213,311,488 bytes,
11,904 × 8,842 sensor pixels and a 11,656 × 8,742 developed crop. The corpus manifest
and [input-limit contract](cloud-input-limits.md) identify its source/license/hash; no
media is committed. Before these changes, a 1,600 × 1,200 CLI render took 3.031 seconds
with a sampled peak process working set of 647,430,144 bytes (one process/input, not a
capacity or speed guarantee). Native control-channel acceptance then exercised the real
adoption confirmation, preserved original hash, IDs, virtual-copy family, edits, metadata,
album and prior named history, restart/retry identity and fresh-peer export with both
protocols. All 143 fixture requests were independently SigV4 verified (70 head / 73 append).
Evidence: `target/cloud-reliability/cr19-native-head-verified.log`,
`cr19-native-append-verified.log`, `cr19-native-results-conditional-head-v1.json` and
`cr19-native-results-append-log-v1.json`. Screenshots/exports were inspected.

Final Chromium acceptance passed both protocols with 177 independently verified fixture
requests: ordinary preview did not fetch the 213 MB original, explicitly cached originals
still respected the 32 MP decode budget, explicit preview export succeeded, browser edits
converged on desktop, and original/all-original pins retained both bytes through zero
budget/cache clearing and offline preview export after reload. No page errors occurred;
the final limit dialog and rendered export were inspected. Evidence:
`target/cloud-reliability/cr19-browser-offline-final2.log`, `cr19-browser-results.json`;
screenshots/exports in `C:/Users/pdykstra/AppData/Local/Temp/lightcraft-cr19-browser/
browser-2d068077/`. Sparse 513 MiB input and a tiny JPEG declaring 200 MP fail admission
with quantitative recovery errors, before payload decoding. Full final-source repository
CI passed all eight gates (fmt, strict Clippy, HEIF, workspace tests, parity, layers,
assets and WASM): `C:/Users/pdykstra/AppData/Local/Temp/lightcraft-reliability-target/
cr19-ci-final2.log`. This is procedural-fixture and one representative real-file acceptance,
not fresh live-provider, every compression variant, camera-colour or large-library parity.
CR.19 is complete. CR.20–CR.21 remain open.

2026-10-17 — CR.20: `lightcraft-cli cloud-signer` is the maintained personal signer.
One canonical browser origin, dedicated prefix and separate random bearer token authorize
scoped object reads, exact-version reads, bounded metadata PUTs and append discovery.
SigV4 query URLs bind method/key/version, exact Content-Length, SHA-256 checksum and
conditional-head preconditions. Content-addressed keys must match the digest; media PUT,
DELETE, bucket-wide listing and contradictory requests are refused. Credentials remain
server-side; URLs/session tokens are not logged or persisted by the service. HTTP headers,
JSON bodies, request read deadlines and concurrent connections are bounded; remote serving
requires an HTTPS reverse proxy over its loopback listener. Browser validation now requires
body binding on every PUT and rejects case-duplicate authorized headers.

The [deployment guide and capability matrix](browser-signer.md) covers CLI build/config,
private environment/profile credentials, Caddy/systemd examples, an AWS permission template,
exact-origin bucket/signer CORS, browser setup and token/credential rotation. It explicitly
separates personal token authorization from multi-user account management and documents
native-only import/adoption, external-editor/DNG and reclamation workflows. Existing custom
signers must return the exact hash/checksum header for conditional PUTs as well as append
writes. No catalog/wire migration or data deletion is required.

All five signer unit tests, the real CLI-process launch/auth/scope integration test and
43 browser tests passed; strict workspace Clippy passed. Evidence:
`target/cloud-reliability/cr20-unit.log`, `cr20-cli-test.log`, `cr20-web-tests.log`,
`cr20-clippy.log`, `cr20-web-build.log`. Full final-source repository CI passed all eight
gates: `C:/Users/pdykstra/AppData/Local/Temp/lightcraft-reliability-target/cr20-ci.log`.

Chromium acceptance used the maintained Rust signer (not the fixture signer) for both
protocols. It verified missing/wrong token, wrong-origin/out-of-prefix/delete/media-write
refusal, short URL lifetime, independently checked signatures, exact checksum/condition/
length enforcement, browser join/preview/export, browser metadata publication and desktop
convergence. Of 121 storage requests, 115 were independently signature-verified and six
intentional body/condition/length tampering requests were refused. No page errors occurred.
Configuration screenshots were inspected. Evidence: `target/cloud-reliability/cr20-browser.log`,
`cr20-browser-results.json`; screenshots/exports in
`C:/Users/pdykstra/AppData/Local/Temp/lightcraft-cr20-browser/browser-a1e93e42/`.
This verifies the local deployment workflow. DNS, TLS termination, service-manager operation,
actual account policy and fresh live AWS/B2/custom-provider conformance were not run and
remain deployment-specific verification. CR.20 is complete. CR.21 remains open.


2026-10-17 — CR.21: Producer-classified synchronization failures now survive native
and browser transport, protocol and owner boundaries. HTTP/service codes and typed errors,
never message text, choose recovery. Connectivity and contention receive five automatic
retries after 2/4/8/16/30 seconds; a sixth failure halts polling. Authentication,
integrity, invalid schema/history/library identity, unsupported configuration and local
resource failures halt immediately. Native URL/TLS configuration failures require repair;
browser Fetch cannot distinguish offline from TLS/CORS rejection and uses bounded
connectivity retries with that limitation stated in its diagnostic. Permanent write errors
are not hidden by speculative readback, while uncertain writes still reconcile exact bytes.
Mismatching immutable readback is an integrity failure.

Retry deadlines use monotonic time. Health exposes failure kind, optional HTTP status,
recovery action, retry attempt/limit, exhaustion and an optional wall-clock countdown.
The popup, icon tooltip and Info report retry/recovery status. Countdown and attempt formats
are translated in all supported format catalogs. Successful polling clears the error and
budget; upload publication cannot do so. Explicit Resume/Sync now and browser
reauthorization reset the budget without discarding failure evidence. Cancelled ordinary
attempts cannot halt a newly resumed scheduler. Failed or cancelled migration stays halted
for explicit cutover recovery. Runtime budgets reset on catalog reopen; durable pending
mutation IDs, import receipts and originals remain retained. No catalog/wire migration or
data deletion is required.

Focused checks passed: nine cloud-status/policy tests, 18 native replication worker tests,
12 S3 tests, 18 cloud UI tests and all 43 browser unit tests. These include bounded exhaustion,
manual/success reset, absent wall time, publication/error isolation, permanent error policy,
producer provenance, migration cancellation and rendered countdown/recovery headlines.
Strict workspace Clippy passed. Evidence: `target/cloud-reliability/cr21-health-test.log`,
`cr21-worker-final.log`, `cr21-s3-tests.log`, `cr21-ui-tests.log`, `cr21-web-tests.log`,
`cr21-clippy.log`, `cr21-native-build.log`, `cr21-web-build.log`.

Native acceptance passed both protocols: injected HTTP 503 retried automatically and
published retained edits without Resume; authentication stopped polling with no repeated
requests; pending work survived restart; explicit repair/Resume converged with a fresh peer.
All 141 storage requests were independently signature-verified. Chromium, using the
maintained Rust signer, passed the same automatic recovery and authentication halt for both
protocols, retained pending edits across reload, and converged with native peers after
reauthorization. The extended browser run also forced Fetch rejection while offline and recovered automatically
after reconnecting, without Resume. All 184 storage requests were independently
signature-verified; HTTP 503 faults were injected in the browser route. No page errors occurred. Countdown,
authentication and recovered UI screenshots were inspected.
Evidence: `target/cloud-reliability/cr21-native-{head,append}.log`,
`cr21-native-results-{conditional-head-v1,append-log-v1}.json`, `cr21-browser.log`,
`cr21-browser-offline2.log`, `cr21-browser-results.json`; screenshots in
`C:/Users/pdykstra/AppData/Local/Temp/lightcraft-cr21-native/native-834ab6e6/`,
`native-be7b8016/`, and
`C:/Users/pdykstra/AppData/Local/Temp/lightcraft-cr21-browser/browser-0459328d/`.
These are procedural loopback/provider-fault checks, not fresh AWS/B2 acceptance or a
large-library latency guarantee. Live provider/account/TLS deployment verification remains
separate from repository CI and this reliability checklist.

Full final-source `cargo +1.95.0 xtask ci` passed all eight gates: fmt, strict Clippy,
HEIF, workspace tests, parity, layers, assets and WASM. Log:
`C:/Users/pdykstra/AppData/Local/Temp/lightcraft-reliability-target/cr21-ci.log`.
CR.21 is complete and checked off. All CR.1–CR.41 checklist items are complete.

2026-10-17 — CR.42: library control envelope and maintenance-gated upgrades. Pure
`cloud_control` tests cover two simultaneous upgraders, a commit racing maintenance,
crashed-upgrader takeover (stale owner can neither activate nor cancel), a lost activation
response, cancellation/retirement and malformed or newer envelopes. Five real-CLI tests
(`apps/lightcraft-cli/tests/cloud_upgrade.rs`, signed loopback S3 fixture) cover a commit
losing its CAS to maintenance (receive-only, `publicationBlocked` in the report and
`cloud.syncStatus`, exact pending mutation retained), a losing simultaneous upgrader that
changes nothing, takeover of an expired lease plus rebase to epoch 2 with a lost activation
response, a held lease refusing other upgraders followed by cancellation to epoch 1, append
retirement with an offline device rejoining (no head write, identical pending mutation,
published afterwards) and append control records pausing or refusing publication. Existing
append, head, join and migration CLI suites pass unchanged except the migration dialog test,
which now needs no peer confirmation. A headless snapshot of the migration dialog was
inspected. All eight CI stages passed (`target/agent-upgrade/ci.log`). The browser upgrade
runner and append control check compile for wasm32 but were not executed in a browser; no
live AWS/B2 run was made (CR.45). Append polls make one additional scoped LIST of `control/`.
CR.43–CR.45 remain open.

2026-10-17 — CR.42 merged onto CR.21 (`cloud-support`). Integration fixes: the maintained
browser signer (CR.20) now authorizes `catalog/epochs/<n>/` change/checkpoint writes (n ≥ 2,
canonical) and the append `control/` listing every append poll performs; the orphan-retention
audit (CR.18) accepts version-4 heads, epoch-namespaced records and canonical append control
records instead of refusing every upgraded library. Upgrade transitions treat CR.21's
`uncertain_write()` outcomes (including transient HTTP) as "reread and confirm"; the browser
upgrade runner returns typed `CloudFailure`s. Regressions:
`signer_binds_exact_hash_condition_scope_and_lifetime`,
`signer_rejects_media_writes_deletes_unscoped_lists_and_contradictory_fields` and
`upgraded_heads_epoch_records_and_append_control_are_audited_not_refused`. The earlier
`target/agent-upgrade/ci.log` was removed with its worktree; post-merge full CI:
`target/merge-upgrade/ci.log`.

2026-10-17 — CR.42 follow-up: typed upgrade failures. Native and browser upgrade runners no
longer report every failure as invalid history. Seed failures keep their CR.21 kind; control
blocks map through `From<Blocked> for CloudFailure` (newer format or retired protocol →
unsupported, maintenance or competing control → contention, malformed control → invalid);
ownership takeover and a busy head are contention, a non-reproducing checkpoint is integrity,
an unconfirmed activation is connectivity, and storage without conditional writes stays
unsupported. Ordinary native sync carries refused control envelopes as `AssetError::Blocked`
instead of an "invalid asset" string. Every refused upgrade (append, rebase, cancel; native and
browser) waits for an explicit retry rather than scheduling an ordinary poll as if it were the
upgrade. Regressions: `control_blocks_keep_their_kind` and the held-lease assertions on
`lastError.kind`/`retryable` in `held_lease_refuses_other_upgraders_and_cancellation_restores_the_same_epoch`.

2026-10-17 — CR.46: sync UX review. `cloud.syncStatus` conflicts now carry the entity name and up to
eight choices (current first, newest next) with a short preview, origin and `moreChoices`; the deleted
photo list (`deletedPhotos`, at most 200, plus `deletedPhotoCount`) is built once per catalog revision
in the same cache. Cloud Synchronization lists up to 16 conflicts with Keep buttons and a scrolling
Recover list; Info offers the same choices for the selected photo's conflicting fields. `publicationBlocked`
reaches `cloud.overview` and its health, the browser host records it from completed scans (polling
it every 60 s like native), and the popup headline reads "Publishing paused by the cloud library" with
the reason. Popup counts use translated labels instead of English plurals; the last remote check
reads as seconds, minutes, hours or days. Regressions:
`any_field_conflict_lists_named_choices_and_keeping_another_value_resolves_it_everywhere`,
`deleted_photo_list_is_computed_once_per_revision_and_capped_with_a_full_count`,
`cloud_popup_says_publishing_is_blocked_and_counts_field_conflicts` and
`cloud_sync_dialog_keeps_a_chosen_conflict_value_and_bounds_the_recover_list`. Headless renders of the
popup and dialog were inspected; the browser build compiles but was not run in a browser.

2026-10-16 — CR.47: migration capability is separate from ownership. The shared
`MigrationSpec` registry resolves plan plus actual frozen source/target schemas to
an implemented converter and activation reader/writer requirements. This build
supports only schema-1 identity rebase and append migration. Begin, takeover
(including same-installation resume and cancellation), activation and retirement
refuse unsupported conversions. Native rebase validates the actual schemas before
building outputs; identical unknown schema numbers no longer authorize identity
translation. Browser append takeover uses the same shared control checks. The
persisted control envelope is unchanged; readable future maintenance records remain
valid and retained, with a typed unsupported/update-required failure.

All 11 control tests and six real-CLI upgrade tests passed. The new signed-fixture
regression seeds expired rebase and append migrations targeting `SCHEMA + 1`, then
tries ordinary acquisition, `writersStopped:true` and cancellation. Every refusal
performs only GETs, retains exact remote objects/control revision, preserves the
catalog and pending mutations, and schedules no automatic poll retry. Existing
supported takeover/rebase, lost activation, retirement and offline rejoin still pass.
Focused logs: `target/cloud-reliability/cr47-control-tests.log` and
`target/cloud-reliability/cr47-upgrade-tests.log`.

The rebuilt desktop app was driven through its control channel against an owned
loopback fixture with EXAMPLE credentials. The cloud popup visibly reports the
update-required reason and retains one pending change; the screenshot was inspected.
All eight storage requests were independently signature-verified, with no upgrade
writes. Evidence: `target/cloud-reliability/cr47-native-results.json` and
`C:/Users/pdykstra/AppData/Local/Temp/lightcraft-cr47-acceptance/cr47-4dec4d81/unsupported-upgrade.png`.
This is local fixture acceptance; browser execution and live AWS/B2 upgrade acceptance
were not performed (CR.45 remains open).

Full final-source `cargo +1.95.0 xtask ci` passed all eight gates: fmt, strict Clippy,
HEIF, workspace tests, parity, layers, assets and WASM (including the browser callers).
Log: `target/cloud-reliability/cr47-ci.log`. CR.47 is complete; CR.48–CR.55 and the
previously deferred CR.43–CR.45 remain unchecked.

2026-10-17 — CR.56: failure advice matches the cause. `CloudFailure` carries an optional `recovery`
(serialized only when set; older failures without it still deserialize) that `action()` prefers over the
kind's general advice. Control blocks map newer formats to "Update LightCraft", a retired head to
"Migrate Cloud Library… to rejoin" and maintenance to "another device is upgrading". HTTP 301/307/400/404/413
are configuration (unsupported) with bucket/prefix/region/endpoint advice, not invalid history. Catalog
limits are a typed `CatalogError::Capacity` (new `capacity` failure kind; not retried) and reach the status
through `AssetError::Capacity`; catalog I/O/corruption is a local resource and a newer catalog is
unsupported. A head library that finds an append manifest reports `AssetError::Moved` with rejoin advice.
Advice names Retry Syncing; Info's Sync now reads Retry Syncing after an error; paused/setup/rejoin errors
name the button and keep the command id for agents. Regressions:
`recovery_advice_names_the_real_fix_not_the_kinds_general_advice`,
`wrong_bucket_or_region_is_configuration_not_invalid_history` and
`catalog_failures_keep_their_kind_through_assets_and_the_engine`.

2026-10-17 — CR.57: polling while idle. egui draws only on input or a requested repaint, and
automatic polling, retries and the 60 s maintenance poll ran only inside a drawn frame, so an idle
desktop window or browser tab stopped receiving changes and its "Retrying in N s" countdown froze.
`Session::cloud_poll_due_in` reports the next automatic deadline (None while paused, halted, preparing
migration or running, when the activity row already drives frames); the desktop UI and the browser host
call `request_repaint_after` for it, and the cloud popup refreshes each second while open. The browser
no longer halts automatic polling with a local-resource error when eight download jobs fill its queue;
it polls 5 s later. A library save failure (`NotSaved`, already retried by the session) postpones the
poll. Native `cloud.reconnect` resets the retry budget and polls at once unless the user paused or a
migration is being prepared. The bounded retry policy itself (five retries over about 60 s, then halt)
is unchanged. Regression: `idle_hosts_learn_when_polling_is_due_and_reconnect_restarts_a_halted_scheduler`.
A minimized desktop window may still receive no frames from the platform; that and browser background-tab
throttling are not addressed.

2026-10-17 — CR.58: Local background media revisions. Managed background completion retains its originating typed operation. XMP save, DNG conversion, external-editor copy and reload commit with cloud sync off; explicit cloud enrollment remains blocked. The managed-media subsystem extraction remains CR.69.

The local-only engine regression covers all four operations and restart. Presented native control-channel acceptance covers asynchronous XMP, external copy and reload, asserting zero extra network requests and unchanged cloud objects. The local-only Info screenshot was inspected. Evidence: `target/cloud-cr58-engine-recheck.log`, `target/cloud-cr58-native-acceptance.log`, `target/cloud-cr58-native/toggle/off-info.png`.

Final-source `cargo +1.95.0 xtask ci` passed fmt, strict Clippy, HEIF, workspace
tests, parity, layers, assets and WASM: `target/cloud-review-cr58-cr62-ci-final.log`.
These are disposable loopback/procedural fixtures, not live AWS/B2 acceptance.

2026-10-17 — CR.59: Original-owner conversion certification. Cloud-to-local conversion recertifies the final received state against the unchanged original owner and its retained append publication state. Repeated scans retain exact acknowledgements, recover already-published pending IDs from verified receipts, and compare semantic fingerprints independent of transport witnesses. Unpublished mutations remain pending.

The real-CLI signed fixture passes both protocols after another device compacts receipts, including a remote write whose local acknowledgement was lost. Conversion clears the exact retained pending ID, receives the newer rating, and performs only GET/HEAD with unchanged remote objects. The fingerprint regression distinguishes publication position from proof transport. Evidence: `target/cloud-cr59-compaction-ack.log` and `target/cloud-cr59-stability.log`.

Final-source `cargo +1.95.0 xtask ci` passed fmt, strict Clippy, HEIF, workspace
tests, parity, layers, assets and WASM: `target/cloud-review-cr58-cr62-ci-final.log`.
These are disposable loopback/procedural fixtures, not live AWS/B2 acceptance.

2026-10-17 — CR.62: Stable absolute local-original destinations. Native library binding and the permanent Local Originals destination are resolved to absolute paths. Conversion persists the reviewed destination and verifies that binding again before worker startup; existing symlink/junction and blob validation remain in force.

The real-CLI regression converts `./photos`, verifies absolute destinations in preview and the durable mode record, checks the original bytes, and reopens from another working directory without network requests. Evidence: `target/cloud-cr59-cr62-cli.log` and the final workspace CI run.

Final-source `cargo +1.95.0 xtask ci` passed fmt, strict Clippy, HEIF, workspace
tests, parity, layers, assets and WASM: `target/cloud-review-cr58-cr62-ci-final.log`.
These are disposable loopback/procedural fixtures, not live AWS/B2 acceptance.

2026-10-17 — CR.61: Incremental append discovery. Native and browser append discovery share stage-and-drain planning. Causally ready operations are applied and their superseded payloads released during discovery; integrity checks and the 64 MiB unresolved-dependency staging bound remain enforced.

A shared-planner regression catches up through 300 sequential 256 KiB replacements (about 75 MiB cumulative payload), with a second author arriving before its dependency. Ready staging returns to zero, both frontiers advance and no gaps remain. Native workspace tests and the browser WASM build cover both adapters; browser runtime and live-provider execution were not repeated. Evidence: `target/cloud-cr61-planner.log`.

Final-source `cargo +1.95.0 xtask ci` passed fmt, strict Clippy, HEIF, workspace
tests, parity, layers, assets and WASM: `target/cloud-review-cr58-cr62-ci-final.log`.
These are disposable loopback/procedural fixtures, not live AWS/B2 acceptance.

2026-10-17 — CR.60: Permanent-local editing and causal rejoin. Permanent-local edits use the ordinary catalog independently of the bounded cloud outbox. A device-local projection baseline is journaled atomically with the first edit, while shared causal state and existing pending IDs remain frozen. Re-enable coalesces final differences against that context before receiving remote changes, durably prepares the new mutations and removes the baseline before enabling remote work. Cloud capacity limits can refuse rejoin with local edits retained and sync still off. Unpublished intermediate media revisions become Superseded, retain protected staging, and leave the upload queue; referencing one again reactivates it. Catalog format 21 records the baseline.

All 44 focused local/revision tests passed, including 10,001 local edits, unchanged original cloud outbox, restart, undo/redo, original pending IDs, remote concurrency, baseline/edit atomicity and rejoin limit refusal. Real-CLI rejoin passes both protocols with a new local photo and concurrent ratings (target/cloud-cr60-cli-rejoin-tests.log). Native rejoin automatically uploads final revisions and the added photo, with zero network requests while local-only; its screenshot was inspected (target/cloud-cr60-native-rejoin.log, target/cloud-cr60-native/local-rejoin/rejoined.png). The original conversion native check passed a retry and three further repetitions after one unexplained control connection reset; command/exit diagnostics were added, but the initial reset cause was not established. Inspection sample: frame 1.43 ms, logic 30.78 ms, last render 4.72 ms; no large-library performance claim.

Final-source `cargo +1.95.0 xtask ci` passed fmt, strict Clippy, HEIF, workspace
tests, parity, layers, assets and WASM: `target/cloud-review-cr60-ci.log`.
These are disposable loopback/procedural fixtures, not live AWS/B2 acceptance.

2026-10-17 — CR.49: Durable successor exposure and safe cancellation. A version-2 maintenance envelope records Preparing before successor publication and durably latches Publishing through the conditional head before the first output PUT. Desktop and browser share the transition. Only Preparing can cancel back to the old authority; Publishing and v1 unknown exposure require finishing the same migration. Failures, uncertain responses, renewal and takeover never clear exposure. Activation and retirement require the latch. Existing successor objects are never erased. Healthy initial envelopes remain version 1. Typed newer-envelope and finish-upgrade advice survives head decoding and migration health. The no-CAS writersStopped path remains cooperative and is still CR.43; this is not a hard append barrier or a schema converter.

Thirteen shared control tests and ten real-CLI upgrade tests pass, including both orders of cancellation/output-authorization CAS, takeover, v1 unknown exposure, failure before manifest, after manifest and after the complete checkpoint seed but before retirement, interruption of rebase output, a lost exposure-CAS response, unchanged remote objects on refusal, retained pending IDs, retry completion and typed update recovery. Native control-channel acceptance refuses cancellation after seed publication, preserves every remote object and finishes on retry; refusal and completion screenshots were inspected. Evidence: target/cloud-cr49-control.log, target/cloud-cr49-upgrade-final.log, target/cloud-cr49-native-acceptance.log and target/cloud-cr49-native/upgrade-exposure/. Sample inspection: frame 0.93 ms, logic 31.00 ms, last render 7.93 ms; no large-library performance claim. All eight final-source CI gates pass (target/cloud-review-cr49-ci.log). Browser WASM compilation is covered; fresh browser runtime and live AWS/B2 acceptance remain CR.45.

2026-10-17 — CR.53: Safe cache relocation cleanup. Cache relocation carries its intended destination and copied blob sizes through owner completion. Reopen must select that exact root and adopt the retention settings, offline owners and keep-all intent before old-copy cleanup is authorized. Fallback, damaged policy or changed settings restores the old selection and retains both copies. Cleanup holds the original resolver and its byte-lease gates, verifies replacement SHA-256 and size under the destination gate, and defers in-use files. A library-owned deletion fence stops cleanup before another move, reopen or process-lock release; retired source-cache maintenance and policy cannot affect a later reuse of the same folder. Without an exclusive library lock old copies are retained. Cleanup retries leases for 30 seconds and may leave extra old copies after a long lease, failed verification, shutdown or interruption; it never discards old retention settings while other files remain. No remote or catalog format changes.

All 26 cache-focused tests pass, covering outstanding old byte leases through reopen, removal after release, exact replacement hash/size, destination fallback, damaged/changed retention settings, durable selection rollback, immediate reuse of the old root in the same session and after dropping/reopening the library, and stale maintenance/policy against a current lease. Native control-channel acceptance moves a pinned original and preview, checks original bytes and offline ownership, and verifies the selected folder in Cache settings; the screenshot was inspected. Evidence: target/cloud-cr53-cache-tests-final.log, target/cloud-cr53-native-acceptance.log, target/cloud-cr53-native/cache-relocation/. Sample inspection: frame 1.31 ms, logic 32.94 ms, last render 5.55 ms; no large-library performance claim. All eight final-source CI gates pass (target/cloud-review-cr53-ci.log). These checks use disposable local fixtures, not live AWS/B2 data.
