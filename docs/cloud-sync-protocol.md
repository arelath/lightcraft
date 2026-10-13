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

## Object layout and publication

All keys are scoped beneath the configured library prefix. Original/rendition layouts are
documented in [cloud-storage-implementation.md](cloud-storage-implementation.md). Metadata uses:

| Key | Contents |
|---|---|
| `catalog/head.json` | Small version-2 head: library, increasing generation, tip and optional checkpoint |
| `catalog/changes/<sha256>.json` | Immutable version-3 batch: parent and version-1 mutations |
| `catalog/checkpoints/<sha256>.json` | Immutable version-1 portable state and legacy-import receipts |

Readers also accept version-1 heads and version-1/2 initial import/revision batches. Legacy
initial import metadata forms a baseline; subsequent mutation edits supersede that baseline.
Binary revision imports update immutable content and filename, preserving later metadata edits.

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

Publish immutable checkpoints containing `Portable` state, complete deduplication receipts,
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

The first release does not remove remote operation records, checkpoints, tombstones, originals
or revisions. Disable lifecycle expiry for these namespaces. Checkpoints accelerate replay;
they do not currently compact the core's retained receipts or remove its bounds (100,000
records, 64 MiB state, 4,096 causal authors). Reaching a bound reports an actionable error and
retains local data. Receipt compaction and remote garbage collection need a separate design
proving new/offline-device bootstrap, immutable checkpoint availability and collision/deletion
preservation. Neither a recent timestamp nor a few devices' acknowledgements proves safe GC.

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

For an existing CAS catalog, stop every writer and upgrade every client, then invoke
`cloud.migrateAppend {"writersStopped":true}` on each catalog. Native CLI/MCP waits; the browser
returns a background task. Migration verifies the committed CAS state, publishes/readbacks an
immutable seed, then saves the local mode/progress without discarding pending IDs or mappings.
Failed legacy upload receipts are explicitly rebound during this migration and retain their
staged bytes and multipart identities. This version rejects CAS synchronization when it finds
an append manifest for the same library. It cannot control older binaries: keep those stopped.
Never resume the two protocols concurrently. Legacy heads, changes and checkpoints remain.

`AppendProgress.remote` caches only verified remote state, separate from the local pending
projection. Both belong to the existing combined 64 MiB local synchronization-state budget;
checkpointing does not remove receipts or raise this capacity. There is no automatic remote
GC or arbitrary log expiration policy. Retain operations, checkpoints, original revisions,
tombstones and exact receipts until a future coordinated retention protocol defines safety.

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
