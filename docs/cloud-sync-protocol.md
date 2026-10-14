# Direct bucket synchronization

Last updated: 2026-10-14

Configure the same bucket and library prefix on each computer. In File → Cloud Synchronization,
the first computer leaves Library ID blank; subsequent computers enter the first computer's
Library ID (also shown in Info). Connection names and credentials belong to each computer.
Use separate local catalog directories. A catalog's saved device identity must not be cloned
to another independently editing device. Existing local photos remain local until imported
into managed storage. Existing catalogs open without a cloud connection.

`cloud.syncEnable {connection, library?, protocol?}` enables a durable offline mutation journal.
`cloud.sync {wait?, checkpoint?}` reconciles and publishes it. Native GUI synchronization
runs on a worker and polls every five seconds while enabled. A failure pauses polling until
an explicit Sync now retry. CLI/MCP wait by default. Activity cancellation preserves pending
work. `cloud.syncStatus` reports the cursor, pending work and retained conflict revisions.
Managed uploads require synchronization enabled for their connection. Select the protocol
in Cloud Synchronization (or `cloud.syncEnable`) before uploading; staging an import while
disabled remains safe and uploading returns an actionable setup error.

## Object layout and publication

All keys are scoped beneath the configured library prefix. Original/rendition layouts are
documented in [cloud-storage-implementation.md](cloud-storage-implementation.md). Metadata uses:

| Key | Contents |
|---|---|
| `catalog/head.json` | Small version-3 head: library, increasing generation, tip and optional checkpoint; archive-aware writer barrier |
| `catalog/changes/<sha256>.json` | Immutable version-3 batch: parent and version-1 mutations |
| `catalog/checkpoints/<sha256>.json` | Version-1 wrapper containing version-2 portable state and authenticated receipt forests; obsolete import-receipt map stays empty |
| `sync-v2/<library>/receipts/<sha256>.json` | Immutable bounded Merkle receipt node shared by both transports |

Readers reject version-1 heads, version-1/2 import batches, nonempty legacy import receipt
maps and synthetic `baseline_`/`legacy_` replica identities. The maintainer confirms those
pre-release libraries contain only disposable test data. Use a fresh catalog and storage
prefix for them; rejection preserves their bytes. Current imports and binary revisions use
normal causal mutations and exact receipts. Existing supported causal-head libraries can
still migrate to append mode; obsolete standalone import libraries cannot.

Writers verify newly referenced originals, proxies, thumbnails and associated XMP before
publishing a change batch. Immutable writes use `If-None-Match: *`; head replacement uses
the observed ETag with `If-Match`. ETags are concurrency tokens, never content hashes.
SHA-256 and declared length verify batches, checkpoints and dependencies. A failed precondition
fetches the competing history and retries with the same mutation IDs. An uncertain immutable
write is read back; an uncertain head write is reconciled by reading the head. Unsupported
conditions produce an actionable error and retain pending work. There is no unconditional fallback.

Head polling downloads only batches since the durable cursor when that cursor remains in
the tail. An older cursor falls back to a verified checkpoint plus its tail. Browsing reads
the local catalog throughout. `checkpoint:true` publishes a checkpoint before conditionally
advancing the head. A checkpoint boundary is inclusive: readers do not fetch the covered batch.
No maintenance command currently removes remote objects. Operators may expire covered change
objects only after retaining a referenced checkpoint and its tail. Originals, revisions,
tombstones and deduplication receipts are retained; this is not a remote garbage collector.
Rebootstrap merges pending offline mutations instead of replacing local state.

## Durability and reconciliation

Catalog format 12 persists global entity mappings, device/library identities, causal revisions,
received receipts, pending immutable mutations and the acknowledged cursor. One CRC-framed
journal batch contains a local change and its outbox intent. Incoming changes, acknowledgements
and cursor are also one batch; success is reported only after saving it. Failed acknowledgement
saves restore the in-memory pending state. Retry after a crash recognizes the original IDs.
Compaction retains pending work. Incoming application does not generate an outgoing echo.
Results from another open library or changed connection are rejected. Slider interactions finish
as a single local edit before incoming state is applied.

### Upload readiness and structural conflicts

Receiving remote changes proceeds while unrelated uploads are failed, deleted, waiting, or
bound to another connection. Missing media delays only the local causal publication prefix.
It is a normal waiting condition; integrity and transport errors still produce actionable errors.
A deleted managed photo's frozen staged import is published before its tombstone, including
after local permanent deletion. This completes its causal sequence without restoring the photo.
An orphan with missing/incompatible staging is reported in `cloud.upload`'s `skipped` results;
healthy selected items continue and durable data remains retained.

Automatic single-import jobs can reach `objectsVerified` while sibling imports still need
transfers. This means publication pending, not failed. Exact append publication proof and
fully verified object keys acknowledge every covered upload intent in one catalog transaction.
A stale non-error worker phase cannot regress an already acknowledged intent. A sibling's on-disk
receipt may retain its waiting marker until explicit verification/retry; staging cleanup remains
conservative until that receipt also has exact remote proof.

Sync-enabled conditional-head imports also publish normal causal mutations rather than
adding one synthetic legacy author per import. Their version-4 device-local receipts bind
the library, exact mutation proofs and conditional-head generation. Version-1/2 standalone
and version-3 append receipts remain readable for conservative local recovery. The standalone
legacy publisher and native/browser synthetic-author bridge have been removed. Uploads without
enabled synchronization retain staged data and require explicit setup. `checkpoint:true`
archives historical receipt proofs with bounded Merkle roots while retaining full live-head,
tombstone and pending contexts. The implemented contract and remaining author/entity limits
are recorded in [cloud-history-compaction-design.md](cloud-history-compaction-design.md).

Album moves are projected from the desired graph and detached before reattachment. For a
cross-entity parent cycle, the album with the greatest global identity in that cycle is shown
at the root. Proposed edges remain in the causal registers. Stacks resolve disputed ownership
in increasing global stack identity order, retain photo ordering, and dissolve projections
with fewer than two members; existing device-local stack ownership remains local. Changed
stacks are removed before installing the final membership map. `cloud.syncStatus` reports
`structuralConflicts` with both proposed and projected values. The Cloud Synchronization
dialog shows these choices and offers Keep current structure. Its
`cloud.acceptStructure {entity}` command creates a causal resolution using the displayed
valid parent or membership, including an empty membership for a dissolved stack. An explicit
valid folder move or stack membership edit can also resolve a choice. A projection alone
does not discard remote data. Ordinary and structural conflict summaries are cached until
the catalog revision or open library changes.

Verification of these corrections is recorded in
[cloud-reliability-tasks.md](cloud-reliability-tasks.md). Earlier acceptance below predates them.

Independent fields merge. Album membership is an individual boolean register per photo, so
concurrent additions of different photos survive. Global photo references also cover virtual
copies, stack members, covers and smart filters. Paths, credentials, cache budgets, pins,
working files, transfer state, selection and hardware preferences are excluded. Mixed albums
and stacks retain their device-local members. Quick Collection has a stable library identity.

Each field retains causally maximal revisions. Concurrent values select a deterministic display
winner by sequence/device identity and retain alternatives. Distinct concurrent develop recipes
appear as named Cloud conflict versions. Open Versions to inspect an alternative; Keep current
edit explicitly resolves the displayed recipe. `cloud.resolveConflict {entity,field,revision}`
can choose another retained revision. A local edit or explicit resolution archives competing recipes into synchronized named versions before retiring their field heads, preserving them on checkpoint bootstrap. Concurrent history/version arrays retain distinct entries;
identical entries coalesce, while mutation receipts retain their separate identities.

A deletion wins concurrent editing. Its content and offline edits remain in the tombstone;
returning devices cannot silently recreate a permanently removed photo. File → Cloud
Synchronization lists retained deleted photos with Recover actions, including photos no longer
in the local Recently Deleted view. `cloud.restoreDeleted {entity}` creates an explicit causal
restore using retained content and edits. Remote original objects are not deleted.

Undo/redo creates new mutations. It rejects a step when a field that step would change has
received another revision; independent remote fields remain intact. Inspect the retained
versions/conflicts and make an explicit edit to resolve a rejected step.

Bounds are enforced before live catalog changes: 2 MiB per mutation/batch, 64 MiB per state or
checkpoint, 4,096 devices and tail records, and 100,000 entity/receipt records. Larger migrations
split into entity-preserving mutations; an oversized individual entity returns an error.
Bounds exceeding the supported library size retain local work and require maintenance or a
future protocol revision. No malformed remote input intentionally panics.

Backblaze B2 live reads/exports passed, but the supplied endpoint rejected both conditional
headers with HTTP 501. Conditional-head publication remains unavailable there; explicitly selected append synchronization is supported below.
The independently signed loopback S3 fixture verifies supported conditional behavior; live
AWS interoperability has not been verified.

## B2 append-only synchronization

Implemented and verified 2026-10-14. Select `append-log-v1` explicitly for B2; existing libraries default to
`conditional-head-v1`. The optional managed metadata backend remains deferred.

The explicitly selected `append-log-v1` protocol supports B2. Its authoritative data is
the union of published immutable mutations, reconciled by the existing causal model. It has
no shared writable head, global generation counter, leader, or lock object. Native clients
talk directly to the bucket; the browser still needs the existing stateless authorization
service, extended to authorize scoped listing and immutable uploads.

Compatibility is based on the live header-isolation checks in
`target/cloud-acceptance/b2-diagnosis-results.json`: ordinary PUT/GET succeeded, conditional
GET worked, and conditional PUT returned 501 for matching, stale and absent-object conditions
with both URL styles. A separate read-only probe verified prefix filtering, delimiter-based
discovery, continuation pagination and `start-after` on this bucket
(`target/cloud-acceptance/b2-list-capabilities.json`). Backblaze documents these in
[ListObjectsV2](https://www.backblaze.com/apidocs/s3-list-objects-v2). The probes establish these
operations, not a transactional snapshot across listing pages or a performance guarantee.

### Keys and identities

Keep originals/renditions at their existing content-addressed keys. Put metadata in a separate
namespace so older clients cannot overwrite or misunderstand the new protocol:

```text
<prefix>/sync-v2/<library-id>/manifest/<sha256>.json
<prefix>/sync-v2/<library-id>/ops/<replica-id>/<sequence:020>-<sha256>.json
<prefix>/sync-v2/<library-id>/checkpoints/<creator-id>/<sha256>.json
```

The manifest is immutable and identifies the exact library, protocol and schema. Joining uses
an explicit library ID; a new library gets a random ID. Clients pin the manifest's identity
and reject incompatible descriptors. Its initial fields are deterministic for a given library
and protocol, so simultaneous initialization of that same library produces identical bytes.
Protocol upgrades use a new namespace and an explicit
migration; an ordinary edit cannot change library protocol.

Each operation object's body is one canonical `Mutation` from
`crates/catalog/src/replication.rs`. Its author and sequence must match the key, its library
must match the manifest, and its canonical bytes must hash to the key's SHA-256. Twenty-digit
sequence numbers sort numerically within a replica. Freeze the exact bytes/key in durable
publication intent before sending; a retry must not allocate a new mutation ID or change bytes.
Start with one mutation per object; finalized sliders already form one mutation. A later batch
format can amortize requests without changing mutation identities or discovery guarantees.

Every independently editing installation/catalog must have a distinct replica identity.
Native catalog locks and browser Web Locks serialize a local writer. Restoring a backup on
another installation must allocate a new current author before accepting new edits, while
retaining old pending mutations with their original IDs. The installation identity stays outside exported library backups. Catalog format 12 stores
a bounded set of historical local authors alongside the current author. Opening a restored
or copied catalog under another owner rotates the current author and preserves old pending
IDs and their causal contexts. Native ownership includes the canonical catalog directory;
browser ownership includes its active library directory.

Two bodies with the same author/sequence have different hash-suffixed keys, so even a cloned
device cannot overwrite the other's operation. The existing mutation-receipt collision check
must then pause that author's synchronization and preserve both records/local work. It must
never choose one by timestamp. Entire cloned browser profiles or manually copied device
ownership data remain an identity-recovery case, not an automatically safe second writer.

### Publication and acknowledgement

1. Atomically save the local catalog change, mutation and frozen publication intent, as today.
2. Upload newly referenced originals, previews, thumbnails and XMP, and verify their SHA-256
   and length before publishing metadata. Existing verified dependencies can be reused.
3. Publish operations in each author's sequence order. Read the exact operation key first;
   reuse it only if its bytes match. Otherwise PUT to that hash-addressed key without a
   conditional header, then GET and verify the exact committed bytes.
4. Save an acknowledgement proof containing exact mutation IDs/digests and their verified
   object references. Only then remove those IDs from the local outbox or mark staged media
   eligible for promotion/removal. Metadata is committed once its complete immutable object
   is remotely verified; there is no second shared-head publication step.

An uncertain PUT or multipart completion is resolved by reading its exact hash-addressed key.
If it is absent, retry identical bytes; a mismatching existing object is an integrity failure.
B2 may retain an additional identical object version on a retry. That affects storage usage,
but readers apply the mutation once. ETags and version IDs are useful diagnostics/references;
SHA-256 remains the integrity identity.

The narrow immutable upload APIs
validate the content-addressed key and expected hash/size. They use ordinary PUT or ordinary
multipart completion in append mode. Do not add a generic unconditional fallback to the current
CAS publisher: `catalog/head.json`, arbitrary paths and mutable documents cannot use this API.
A read-before-write is a reuse check, not a lock. Safety comes from every conforming writer
being able to write only identical bytes to a particular content-addressed key.
Authorized clients are assumed to follow this rule. Hashes detect corruption and conflicting
identities; they do not authenticate the author or prevent a bucket administrator from changing
objects. A conflicting object is an error, not permission to overwrite it or discard local data.

Cloud import into an append-mode library requires a durable shared library identity before
publication. Route initial imports and binary revisions through the causal mutation outbox;
the older standalone legacy-import/head publisher must not run against B2. Local import,
editing and export still work while publication is unavailable.

### Discovery, gaps and durable progress

Use `ListObjectsV2` with the exact library `ops/` prefix and delimiter `/` to discover authors.
For each author, list the suffix beginning at its last contiguous remotely verified sequence
(inclusive), then fetch unknown records. Follow every continuation page. Restart discovery
from the root on later scans so an author inserted alphabetically before an earlier page
cursor is still found. Run a complete identity audit at startup, explicit Sync now and
periodically to detect an older duplicate/fork below an incremental boundary.

Maintain separate concepts:

| Value | Meaning |
|---|---|
| Causal knowledge | Mutations known locally, including durable unpublished local edits |
| Remote publication receipts | Exact remotely verified IDs and digests |
| Discovery frontier per author | Largest contiguous remotely verified prefix, or a prefix covered by a validated checkpoint |
| Pending gaps/dependencies | Records discovered but not ready to apply |

Neither the maximum key returned by LIST nor `SyncState.clock[author]` is a discovery frontier.
For example, seeing sequence 10 while 9 is absent cannot advance the durable frontier past 8.
A failed/timed-out page cannot certify its remainder. Continuation tokens are transient page
cursors, not permanent synchronization cursors; the next scan resumes from durable frontiers.

Stage out-of-order records, and apply only after their required causal predecessors are
available through received records, local durable mutations or validated checkpoint coverage.
The existing `SyncState::receive`/`merge` remains responsible for register reconciliation,
deduplication, recipe versions and deletion/tombstone policies. Do not assume its vector clock
proves that every predecessor's object was downloaded. Persist applied changes, exact receipts
and discovery progress together before advancing progress. Invalid records and identity
collisions retain pending work and leave the existing local library usable.

This protocol provides eventual convergence, not a globally atomic view of all devices at the
end of a scan. Writes arriving during pagination are found on subsequent passes. Known peers
can be polled frequently while active; new-author discovery and full audits can run less often.
Explicit Sync now performs discovery too. Status should show pending publication, missing
records and last successful scan; it must not invent a global generation or claim every other
device has acknowledged the user's edits.

### Checkpoints and retention

Publish immutable checkpoints containing `Portable` state, authenticated deduplication coverage,
tombstones/conflict versions, and a certified contiguous coverage vector. Build them from
verified published state, never directly from the local catalog's `portable()` while it has
unpublished changes: that representation excludes the outbox field but still contains effects
of its pending mutations. A checkpoint must not bypass dependency upload or acknowledge
unpublished work.

There is no authoritative latest checkpoint. Validate candidate objects and merge compatible
checkpoints using `SyncState::merge`; incomparable vectors may both contribute changes. Replay
the uncovered tails of every discovered author afterward. Coverage requires receipts for the
complete claimed prefixes and a causally closed state; a maximum clock alone is insufficient.
Preserve the local outbox when bootstrapping or merging a snapshot.

The first release does not remove remote operation records, checkpoints, receipt archive nodes,
tombstones, originals or revisions. Disable lifecycle expiry for these namespaces. Checkpoint
maintenance archives exact historical mutation/context hashes in immutable Merkle forests,
restoring capacity consumed by historical receipts. Full current-head and pending contexts stay
live. Retired authors remain in the clock; 4,096 authors and the active document/pending/64 MiB
state bounds still apply. Missing or conflicting archive proofs retain local work and stop the
merge. Receipt compaction and remote garbage collection are separate operations. Neither a
recent timestamp nor a few devices' acknowledgements proves safe GC.

### Capacity status and recovery

An unchanged certified conditional-head result reports `unchanged:true` and skips
owner-side projection/materialization, catalog operations and new journal records.
The guard compares every durable portable component plus cursor and generation;
pending work, nonempty acknowledgements, migration and append progress use the
normal transaction path. Browser no-op completion also bypasses ACK-file writes
and the ACK flush stage. Native polling still retries any pre-existing unsaved
local work; an unchanged head does not discard or strand that work.

Existing-catalog enablement and each local edit keep one entity's changed fields in
one bounded group: at most **4,000 changes and 1 MiB of serialized change-array JSON**.
Album attributes and managed `member:<id>` entries share this budget; local-only
members are excluded. This is a deliberate product limit, rather than a streaming
album representation. Original and preview file bytes are outside this metadata budget.
An oversized group reports its identity/name, actual count
and bytes, limits and recovery. Split existing large albums before enabling sync;
split large membership edits into smaller commands, or shorten oversized metadata.
Failure installs no partial synchronization state, identities or outbox into the
catalog. Media is never deleted as recovery.

This limit is per enablement/edit group, not a lifetime 4,000-member album limit.
Smaller edits may accumulate more fields up to the 10,000-field state limit; removing
a synchronized member retains a false membership field. Receiving an existing
remote checkpoint uses the state bounds and does not re-encode it as an enablement
group. Both native and browser enablement use the same atomic engine path.

`cloud.syncStatus` and `cloud.overview` report `utilization` and `capacityWarnings`.
The byte count is the exact serialized local `SyncState`, including the outbox,
measured without allocating another full state buffer and cached by catalog revision
and library identity. It is metadata usage, not provider quota or media/cache usage.
Limits are 64 MiB, 100,000 documents, 100,000 full receipts, 10,000 pending mutations,
4,096 author identities, 10,000 fields per entity and 128 revisions per field.
Warnings start at 80% of each limit and become critical at 95%.

The Cloud Synchronization dialog shows counts, warning-specific recovery guidance,
and **Archive receipt history**, which runs `cloud.sync {"checkpoint":true,"wait":false}`.
The cloud popup and Info panel show byte usage and the first warning. Checkpoints
can archive eligible historical receipts; finish uploads and pending synchronization
first. They cannot remove current documents, tombstones, field/conflict heads or
retired author clocks. Removing album members retains their membership fields.
Pending capacity needs successful synchronization; author capacity has no supported
safe slot-retirement mechanism. Limit failures preserve the existing catalog/outbox
and identify the applicable recovery or unsupported limit.

### Native/browser boundaries and compatibility

Local synchronization persistence selects head cursor/generation or append frontiers and
verified publication receipts explicitly.
Keep mutation identities, portable registers and the existing conflict UI. A new exact-ID
acknowledgement path must not pass a LIST fingerprint into the old head cursor/generation API.
Native results still apply on the owning session and reject a changed library/connection.

The browser's acknowledgement receipt must be versioned for append-mode proofs. Incoming
state/progress and the proof must finish flushing to OPFS/IndexedDB before IDs are discarded,
retaining later edits. Reload must validate the proof against the saved transaction/progress
and exact mutation digests. Keep the existing cache verification, quota and no-eviction rules.

Native S3 needs bounded bucket-root ListObjectsV2 URL/signature construction and XML parsing;
the existing object-key URL helper is insufficient. Returned keys must stay inside the exact
configured namespace. Bound pages, keys, XML depth/body size, tokens, device counts and queued
records; repeated continuation tokens or missing required pagination fields are errors.

Extend browser authorization with typed `list` and `putImmutable` requests. A list request
includes exact prefix, delimiter, `startAfter`, continuation token and bounded `maxKeys`.
Configure/validate the bucket root in addition to the storage origin; validate the authorized
URL's root, method and exact signed query. The signer enforces that prefix on every page and
cannot grant an unrestricted bucket list. `putImmutable` binds the key, hash and size and never
authorizes a mutable head. Existing expiry refresh, cancellation, HTTPS, token handling and
bucket GET/PUT CORS behavior remain. Credentials require scoped listing permission as well
as object read/write; Backblaze application keys need the corresponding `listFiles` capability.

Preserve `conditional-head-v1` for existing S3 libraries. Mode is explicitly chosen and pinned
to the library, not silently changed after a 501. For an existing CAS library, migration needs
a coordinated pause/upgrade of all writers: verify the legacy remote history, publish a new
append-mode seed checkpoint containing only remotely committed state, retain pending IDs and
local entity mappings, then switch devices to the new namespace. Legacy records stay intact.
Without that cutover, old clients could keep writing a history the new readers do not discover.
The earlier B2 tests did not successfully publish a shared head. Their staged imports can
be recovered through explicit migration without reimporting the originals. If the catalog
does not yet have a mutation journal, enable the default conditional-head journal with its
existing connection first, then migrate with every old writer stopped. This explicitly rebinds
legacy upload receipts; selecting append mode alone does not silently change those receipts.

### Implementation order and acceptance

1. Add explicit protocol configuration, immutable-write/readback and bounded listing adapters.
   Verify ordinary small/multipart uploads and prefix/pagination behavior on live B2.
2. Add append records, exact-ID publication proofs, durable progress and author-rotation
   recovery. Preserve the current CAS mode and all local staging/ownership protections.
3. Add gap-aware discovery, causal application and checkpoint bootstrap; initially keep remote
   history and current core bounds. Reuse existing field, conflict, deletion and undo tests.
4. Extend the browser signer contract, Fetch adapter and durable acknowledgement receipt;
   inspect the shared setup/status/retry workflows on OPFS and IndexedDB.

Required acceptance includes simultaneous writers to the same field; independent fields and
album additions; same-object upload races; lost PUT/multipart/acknowledgement responses;
restart with frozen intent; late author creation during root pagination; a missing earlier
sequence appearing after a page cursor; out-of-order cross-author dependencies; duplicate IDs
with different content; restored backup with pending historical authors; incomparable
checkpoints; no publication of unsent checkpoint effects; offline delete-versus-edit recovery;
quota failures; expired listing/upload URLs; malformed/out-of-prefix responses; and native/
browser/live-B2 convergence. Verify that append mode never writes a shared head or sends the
unsupported conditional-write headers, while CAS-mode regressions continue to pass. Full CI
and actual UI inspection are separate verification gates recorded in `tasks.md`.

### Setup, cutover and retention

For a new B2 library, configure native storage, then enable synchronization **before** managed
import: `cloud.syncEnable {"connection":"personal","protocol":"append-log-v1"}`. Join other
catalogs with the same prefix, library ID and protocol. The shared setup dialog offers
Append-only (Backblaze B2). Status reports author frontiers, gaps, pending work and conflicts;
there is no append generation or shared-head cursor. Sync now performs a full identity audit;
automatic polling uses inclusive author tails and audits again every five minutes and on startup.
The next automatic scan starts five seconds after completion, leaving time for explicit retry
even when a remote scan lasts longer than the interval.
All checkpoint pages are verified and merged; no listing-page snapshot guarantee is assumed.
Staged dependencies are capped at 64 MiB, and up to 1,000 gap IDs are shown. Pending dependencies
still exist in the retained log and are rediscovered on the next scan.

For an existing CAS catalog, choose File → Migrate Cloud Library… (also available in Cloud
Synchronization and Info). Opening the dialog calls `cloud.prepareMigration`: this device's
automatic uploads and synchronization pause, and active catalog synchronization is cancelled.
Wait for running uploads to finish or cancel them in Activity. Stop synchronization on every
other device, upgrade every client, and confirm the dialog's peer checkbox before migration.
Cancel keeps preparation paused in this session; explicit Sync now resumes it. Preparation is
not persisted across app restarts, so keep paused devices closed until cutover is ready.

`cloud.migrateAppend {"writersStopped":true,"wait":false}` performs the same cutover. Desktop
and browser return a cancellable Activity task; native CLI/MCP wait by default. Inspect
`cloud.syncStatus` (`preparingMigration`, `migrating`, reports) for progress and completion.
Migration verifies committed CAS history on a worker, publishes/readbacks an immutable seed,
then atomically saves mode/progress on the catalog owner. Unsent pending IDs and mappings,
including edits made while the worker runs, remain intact. A failed native journal save rolls
back the local cutover. Cancellation can leave a verified remote marker/seed behind; retry
migration rather than reverting protocols or deleting remote objects. Apply the migration to
every local catalog before resuming the fleet. This confirmation is operator coordination;
LightCraft cannot remotely stop or upgrade other devices.
Failed legacy upload receipts are explicitly rebound during this migration and retain their
staged bytes and multipart identities. This version rejects CAS synchronization when it finds
an append manifest for the same library. It cannot control older binaries: keep those stopped.
Never resume the two protocols concurrently. Legacy heads, changes and checkpoints remain.

`AppendProgress.remote` caches only verified remote state, separate from the local pending
projection. Both belong to the existing combined 64 MiB local synchronization-state budget.
Checkpoint maintenance removes historical full receipts from live state after immutable archive
verification; it does not raise the budget or drop current/pending contexts. There is no automatic
remote GC or arbitrary log expiration policy. Retain operations, checkpoints, receipt archive
nodes, original revisions and tombstones until a separate retention protocol defines safety.

Native uploads verify all assets of a bulk import before publishing its shared mutation. Small
and multipart writes use ordinary S3 requests in append mode and always verify bytes before
metadata publication. A Windows/B2 TLS reset reproduced with 64 KiB buffered plaintext writes;
draining each 16 KiB fragment succeeded with identical signatures and bytes. The pure-Rust
transport now drains at that boundary. No conditional operation is retried unconditionally.

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
and `tasks.md`. Acceptance objects use owned prefixes; no existing library objects are removed.

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


### Publication completion ordering (2026-10-14)

Native uploads and ordinary synchronization share one metadata publication gate per
open library. A worker holds it from remote reconciliation through durable owner
application (or rejection); byte transfers and staging receipt writes use separate
locks. Blocking CLI commands pump both owner queues while waiting. Cancellation
and library replacement release waiting work without applying results to another
catalog. The browser already permits one metadata synchronization job.

A certified head completion retains its worker's starting acknowledged generation.
Finishing an older valid scan after a newer completion preserves the newer cursor;
a scan below its own starting generation is rollback and fails, including scans
with pending mutations or checkpoint maintenance. Equal-generation cursor changes
remain errors. Append acknowledgement merges verified remote effects and contiguous
frontiers, pruning gaps covered by either result. Receipt/manifest collisions fail.
An obsolete completion missing witnesses for newer archived history retains the
newer state and outbox and schedules a fresh certified scan without halting polling.

Upload phase events identify their attempt. Ordinary older failures cannot undo
shared acknowledgement. Explicit verification started on an already synchronized
import can invalidate it when it confirms missing or corrupt remote bytes and
cannot repair them; connectivity or old publication failures do not revoke proof.
Receipt finalization after another worker acknowledges publication remains CR.27;
transfer/publication stage extraction and request-cost assertions remain CR.31.
