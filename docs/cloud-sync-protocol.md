# Direct bucket synchronization

Last updated: 2026-10-20

This is the current normative metadata protocol and owner-durability contract.
[Current behavior and limits](cloud-storage-implementation.md), the
[browser authorization/storage contract](browser-cloud.md), and
[dated acceptance](cloud-acceptance-history.md) have separate roles. Exact receipt
archive and capacity rules are maintained in [cloud-history-compaction-design.md](cloud-history-compaction-design.md).
The [reliability checklist](cloud-reliability-tasks.md) tracks implementation and verification.

Native immutable append/receipt bodies may be reused from a bounded, destination/key-bound
local cache after byte and schema verification. This is transport evidence only: causal
validation, current authority, historical-key discovery and durable acknowledgement remain
required. Fresh integrity verification bypasses durable bodies throughout that pass. Cached
reuse cannot establish that a remote object still exists or has unchanged remote bytes;
corruption/disappearance may be detected later at fresh verification. See the
[cache policy and limits](cloud-storage-implementation.md#native-immutable-history-cache).

Configure the same bucket and library prefix on each computer. In File → Cloud Synchronization,
the first computer leaves Library ID blank; subsequent computers enter the first computer's
Library ID (also shown in Info). Connection names and credentials belong to each computer.
Use separate local catalog directories. Each persistent installation/catalog claims its own current author in both protocols;
copied catalogs keep historical pending IDs and rotate the current writer before editing. Native cloud mode automatically adopts added local photos into managed storage. Folder-browsed Local records stay on this device. Existing catalogs open without a cloud connection.

`cloud.syncEnable {connection, library?, protocol?}` enables a durable offline mutation journal.
On native hosts it also enrolls all added library photos in automatic durable staging;
folder-browsed Local records are excluded. Identical original bytes share SHA-256
objects, while distinct photo identities, virtual copies and concurrent edit choices
remain distinct. Adopting a local original reprojects its existing album/stack references.

### Turning synchronization off on this device

`cloud.syncDisablePreview {}` returns the permanent destination and an estimate from
the current catalog. `cloud.syncDisable {confirm:true, wait?:false}` starts or retries a
native conversion; `cloud.transitionStatus {}` reports stage, completed/total file and
byte counts, current file, destination, blocking state and an actionable error.
`cloud.syncDisable {cancel:true}` waits for an active worker to stop before unblocking.
CLI clients can request `wait:true`; desktop calls must remain asynchronous.

The stages are draining → reading → downloading → checking → complete. Any failure
retains cloud mode and enters a blocking retry/cancel state. A separate, atomically
written, device-local `cloud-mode.json` records intent before workers stop. Reopening
an interrupted conversion requires Retry or Cancel; corrupt/future records fail closed
instead of silently reconnecting. Existing verified files are rechecked and reused.

The receive-only reader validates both supported protocols without publishing any
objects. It merges cloud-only content with this catalog's pending changes, downloads
all live managed originals, previews, thumbnails and associated XMP, including retained
content-conflict alternatives, into `<library>/Local Originals/blobs/<sha256>`.
Deleted entities, unrelated bucket contents and historical garbage are outside this
scope. Disk-space and write-permission checks precede media acquisition. Streamed
copies are synced, verified by exact length and SHA-256 and published without replacing
existing originals. The destination must support hard links; otherwise conversion
fails with cloud mode retained. Permanent copies are independent of upload staging
and disposable cache; cache relocation, trimming and output guards protect them.

After copying, another verified scan must show the same cloud state. New content
extends the plan and repeats verification, with a bounded retry limit for continuously
changing libraries. The resulting local library reflects the final observed head or
append frontiers. This is not a global freeze: writes published after that observation
remain on other devices until this device explicitly reconnects. An incomplete append
history, changed local catalog/storage binding, corrupt file or failed durable save
prevents success. The merged catalog is persisted before the atomic local-only mode
record is committed. Cloud originals are never deleted by this operation.

Local-only mode retains causal identities, conflict history and pending edits, while
blocking automatic adoption, upload, polling and implicit source downloads. Explicit
`cloud.syncEnable` reconnects the same library and resumes merge/upload; pause/resume
is separate and cannot reconnect a disabled library. Editing and navigation are blocked
during conversion, including commands/control clients; Quit is safe and restart resumes
through explicit recovery. Browser Pause remains available, but browser conversion
to permanent local files is disabled with desktop guidance: browser cache persistence
does not provide this durability guarantee.

### Permanent-local edits and explicit rejoin (CR.60)

While synchronization is off, the ordinary catalog is the durable local overlay. The
first local edit atomically journals a device-local projection baseline with that edit;
later edits do not append cloud mutations or reserve cloud author sequences. The shared
causal state and existing pending mutation IDs/payloads remain frozen. The baseline is
bounded to 64 MiB and the existing entity/field bounds; it is local catalog data, excluded
from cloud publication. Catalog format 21 records this baseline and older builds refuse
the newer local catalog.

Explicit re-enable claims the current writer, compares the final local projection with
the saved baseline, and prepares bounded mutations using the frozen causal context
before receiving newer cloud state. Concurrent remote edits therefore remain alternatives
rather than becoming observed edits that local rejoin silently replaces. The prepared
mutations and baseline removal share one catalog transaction; persistence precedes the
mode-file switch and resumption of remote work. A retained baseline forces local-only
mode on restart, even if a restored mode file says otherwise. Incoming materialization
refuses an unmerged baseline. Rejoin still enforces cloud mutation/outbox capacity;
refusal leaves sync off, local edits saved and local editing available.

Unpublished intermediate managed revisions replaced during the disconnected period are
marked `superseded`. They retain immutable manifests and protected staged bytes, are
excluded from cloud queue counts and upload selection, and cannot become remotely
acknowledged through a stale worker. A current or pending content reference reactivates
the corresponding upload. Previously frozen pending imports are never superseded or
rewritten. New file-backed photos retain their edits and ordinary automatic enrollment
runs after rejoin. Superseded local history is not a cloud backup guarantee.

### Background synchronization

`cloud.sync {wait?, checkpoint?}` reconciles and publishes it. Native GUI synchronization
runs on a worker and polls every five seconds while enabled. Typed transient failures use
bounded automatic backoff; permanent failures require explicit recovery (see Cloud subsystem
health below). CLI/MCP wait for one attempt by default. Activity cancellation preserves pending
work. `cloud.syncStatus` reports the cursor, pending work and retained conflict revisions.
Managed uploads require synchronization enabled for their connection. Select the protocol
in Cloud Synchronization (or `cloud.syncEnable`) before uploading; staging an import while
disabled remains safe and uploading returns an actionable setup error.

## Storage destination binding

`cloud.configure` pins a used connection to its existing bucket, region, prefix
and endpoint in both protocols, including managed assets staged before enablement.
It rejects retargeting before changing saved settings or the active resolver. The
setup wizard runs the same check before saving typed credentials. Credentials and
profile selection can rotate for the same destination. Prefix/endpoint trailing
slashes have one normalized spelling for configuration, loading, probing, receipts
and live object-key construction; loading older aliases does not rewrite their file.

Changing storage requires a separate local catalog and verified transfer of
originals plus history. The append protocol migration changes the publication
protocol within a destination; it does not move a library to another bucket.

Browser configuration pins storage origin, prefix and the exact optional
`bucketRoot`. Adding or removing a root after enablement is also a destination
change. Configure the complete bucket root before joining when append migration
will be needed. Authorization endpoints and ephemeral tokens can rotate without
changing that object namespace. Rejected edits leave both provider and persisted
configuration intact.

## Retained smart-album references

Album identities survive deletion separately from live album objects. Fresh catalogs
reserve mappings for album tombstones and references whose target has not arrived;
reservation advances the durable local ID allocator. Later delivery or explicit
restoration uses the same identity without creating a placeholder live album.

A missing or folder target makes the saved smart criterion unavailable and nonmatching.
This propagates through smart-album dependencies, negative operators and nested any/none
groups: missing data cannot broaden a restrictive album. The saved reference remains
intact and existing rule diagnostics identify missing targets. Repair or restoration
makes the criterion usable again. Smart albums with filesystem folder criteria or any photo selection that cannot be
shared on this connection stay device-local, along with smart albums depending on
them. Their complete local criteria survive enablement and self-synchronization.
Converting a previously shared album to this policy withdraws its shared copy with
a retained tombstone; the local album stays intact. Returning to portable criteria
republishes that device's complete local album under the same identity. `albums.list`
reports `syncScope: deviceLocal|shared` for smart albums.

Selection restrictions distinguish legacy absent/empty `only` (unrestricted) from
`onlyRestricted:true` with an empty/absent allowed list (match nothing). Nonempty
portable selections carry the marker so undelivered/deleted targets cannot broaden
them after local mapping. A later target delivery resolves the retained wire IDs.
Filter chips clear the marker; legacy `only:[]` command patches still clear the
restriction unless they explicitly set the marker. Empty similarity results remain
constrained rather than showing the whole library.

Upgrade all devices before using these semantics. Current local snapshots use format 19 (15 introduced the smart-selection barrier),
new mutations version 2, and new synchronization/checkpoint states version 3. Older
builds reject these required versions before applying an empty allowed set. New
readers accept legacy mutation/state versions and retain their exact identities and
digests. Receipt maintenance preserves the newer checkpoint barrier. Batch, append
manifest and outer checkpoint wrappers keep their existing formats. The head is version 4
and carries the library control envelope (see [Library control and format upgrades](#library-control-and-format-upgrades-2026-10-17)),
which future format changes use instead of "upgrade every device first".

## Object layout and publication

All keys are scoped beneath the configured library prefix. Original/rendition layouts are
documented in [cloud-storage-implementation.md](cloud-storage-implementation.md). Metadata uses:

| Key | Contents |
|---|---|
| `catalog/head.json` | Small version-4 head: library, increasing generation, tip, optional checkpoint and library control (epoch, schema, reader/writer minimums, maintenance state); the single CAS authority for commits and upgrades |
| `catalog/epochs/<n>/{changes,checkpoints}/…` | Immutable batches/checkpoints of conditional-head epoch `n ≥ 2`; epoch 1 keeps the `catalog/changes`/`catalog/checkpoints` layout |
| `sync-v2/<library>/control/<revision:020>-<sha256>.json` | Immutable append-log library control records (cooperative barrier) |
| `catalog/changes/<sha256>.json` | Immutable version-3 batch: parent and version-2 mutations (legacy version 1 remains readable) |
| `catalog/checkpoints/<sha256>.json` | Version-1 wrapper containing version-3 portable state and authenticated receipt forests; obsolete import-receipt map stays empty |
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
Ordinary maintenance preserves remote history; explicit `cloud.deleteLibrary` is the
separate whole-library erasure exception (see [cloud deletion](cloud-retention.md#delete-an-entire-cloud-library)).
Operators may expire covered change
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
An import with missing/incompatible staging or connection settings is reported in
`cloud.upload`'s `skipped` results and retains a device-local `blocked` reason and retry mode
in its upload intent (local format 17). This preparation outcome is saved even when no
worker starts or an otherwise successful batch includes skipped imports. Healthy selected
items continue and durable data remains retained; causal publication dependencies still apply.
Preparation failures never revoke a previously verified synchronization acknowledgement.
Deliberate verification retries retain `verify:true` so the retry cannot silently skip them.

The cloud popup and photo details show specific blocked reasons and import-specific retry
actions. Blocked imports count as issues, rather than ordinary waiting uploads. Explicit
import retry reruns preparation; resume, connection/setup changes and library reopen reset
automatic attempt eligibility. Unrelated upload batches do not repeatedly retry blocked work;
use the displayed import-specific retry action (or explicit `imports` selection).
Restoring a manifest alone requires an explicit retry or resume while the app remains open.
Preparation checks are bounded to 32 imports per invocation; the status overview retains
the total blocked count with at most 32 detailed records. Catalog upload state is unchanged
until an actual media attempt reports its verified outcome.

Automatic single-import jobs can reach `objectsVerified` while sibling imports still need
transfers. This means publication pending, not failed. Exact append publication proof and
fully verified object keys acknowledge every covered upload intent in one catalog transaction.
Obsolete worker phases cannot regress an already acknowledged intent. Shared ACKs also
journal a device-local receipt-finalization queue (local format 16) with the exact mutation
IDs/digests and protocol/library binding; head proofs include their acknowledged generation.
A separate local worker writes this evidence into every covered sidecar, then durably clears
that import's queue entry. Upload workers also clear their own entries after successfully
writing final proof. Local write or catalogue-clear failure keeps staging protected.

Finalization does not upload or download originals and can finish while cloud publication is
user-paused. Restart retries queued work; local failures retain per-import reasons and retry
with a 30-second backoff. Explicit resume clears the backoff. Local-copy removal finalizes
covered receipts before staging promotion/removal, so it needs no manual verification upload.
Receipt/asset/global-photo/library/protocol/destination mismatches and unfinished multipart
records remain protected and require repair. Old synchronized imports lacking a queued exact
proof retain the conservative legacy recovery policy; generation alone cannot invent evidence.

Sync-enabled conditional-head imports also publish normal causal mutations rather than
adding one synthetic legacy author per import. Their version-4 device-local receipts bind
the library, exact mutation proofs and conditional-head generation. Version-1/2 standalone
and version-3 append receipts remain readable for conservative local recovery. The standalone
legacy publisher and native/browser synthetic-author bridge have been removed. Uploads without
enabled synchronization retain staged data and require explicit setup. `checkpoint:true`
archives historical receipt proofs with bounded Merkle roots while retaining full live-head,
tombstone and pending contexts. The implemented contract and remaining author/entity limits
are recorded in [cloud-history-compaction-design.md](cloud-history-compaction-design.md).

Archive certification uses an incremental proof session. It validates the incoming
document structure once and builds the remote receipt forest once, plus one forest
for each local target. The main catalog and retained append publication progress
share the same immutable-node cache. Missing paths produce a deduplicated frontier;
supplying a node resumes only the paths waiting for that hash. Nodes are hash-checked
once within the session, with exact library/author/range checks on every path.
Prefix commitments and leaf mutation/context commitments remain mandatory. A failed
path poisons the session, so ignoring an error cannot permit later application.
Used path nodes are returned as evidence even when reconstructed locally: the later
owner and publication-progress merges have independent forests. Unused generated
nodes stay out of that evidence; the existing witness budget still applies.

Only completed proof planning permits the final document merge, once per target.
The owner still merges against its current catalog before acknowledging work.
Native and browser conditional-head readers retain one bounded witness cache through
checkpoint loading, replay, hydration and final certification; migration retains it
too. Fetches remain bounded individual GETs, processed by frontier rather than a
whole-state retry per object. Append audits retain their existing bounded cache
policy and can refetch after eviction. This changes no wire formats or proof limits
and does not remove the separate large-library/materialization backlog.

Album moves are projected from the desired graph and detached before reattachment. For a
cross-entity parent cycle, the album with the greatest global identity in that cycle is shown
at the root. Proposed edges remain in the causal registers. Stacks resolve disputed ownership in increasing global stack identity order using only
shared photo identities. Deleted and unresolved shared photo targets cannot claim ownership;
a projection with fewer than two live shared members dissolves. Device-local members are
attached only after those shared decisions. A dissolved or deleted shared stack can retain
a group of at least two local-only members, with its local collapsed state. An unsynchronized
mixed stack relinquishes members owned by shared winners and retains its other members when
at least two remain. Local singletons remain unstacked. Changed stacks are removed before
installing the final membership map; local overlays never change shared ownership or validity. `cloud.syncStatus` reports
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
For each author, list the suffix strictly after its last contiguous remotely verified sequence,
then fetch unknown records. Previously verified identities are checked by a separate
resumable historical discovery cursor. Follow every continuation page. Restart author
root discovery on later scans so a newly inserted earlier author is found. Ordinary Sync
now requests bounded cached/known-key historical discovery; scheduled discovery starts
at most every five minutes and continues interrupted slices. A different hash at an old
sequence must be fetched and causally checked. An exact certified receipt or consumed
checkpoint key can skip reconstructing accepted effects, without claiming current remote
byte integrity.

`cloud.verifyHistory` performs a separate read-only fresh verification. It never publishes,
materializes changes or acknowledges pending mutations. Persist its destination-bound
weekly deadline, independent append cursor and whole-slice reservation before remote I/O.
Each slice admits at most 128 HTTP requests/128 MiB of bounded responses; nested receipts
and listings share admission. Limit reservations to four per persisted 24-hour window and
continue incomplete scheduled slices at least 15 minutes apart. Restart, cancellation and
failure do not refund allowance. Save successful append keys before subsequent requests.
Fresh reads bypass durable body caches, and verification failures cannot be cleared by
ordinary cached synchronization. Validate required known-key gaps and archive branch
children as well as listed objects. See [cloud-storage-implementation.md](cloud-storage-implementation.md#incremental-sync-and-remote-verification)
for adapter scope, deferred completion, corruption-detection tradeoffs and head-reader limits.

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
recent timestamp nor a few devices' acknowledgements proves safe GC. The separate
[native orphan retention tool](cloud-retention.md) retains every published historical
reference and requires explicit writer quiescence; it does not expire protocol history.

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
to the library, not silently changed after a 501. For an existing CAS library, migration puts
the head into maintenance (a CAS that every conforming writer's next commit loses), seeds the
append namespace from that frozen history, retains pending IDs and local entity mappings, then
retires the head to the append successor. Legacy records stay intact. Other devices detect the
retired head and rejoin without rewriting their pending mutations.
The earlier B2 tests did not successfully publish a shared head. Their staged imports can
be recovered through explicit migration without reimporting the originals. If the catalog
does not yet have a mutation journal, enable the default conditional-head journal with its
existing connection first. With no published head (or storage rejecting conditional writes)
there is no barrier to hold, so migration then requires `writersStopped:true` after stopping
every other device. This explicitly rebinds legacy upload receipts; selecting append mode alone
does not silently change those receipts.

### Protocol acceptance requirements

Required acceptance includes simultaneous writers to the same field; independent fields and
album additions; same-object upload races; lost PUT/multipart/acknowledgement responses;
restart with frozen intent; late author creation during root pagination; a missing earlier
sequence appearing after a page cursor; out-of-order cross-author dependencies; duplicate IDs
with different content; restored backup with pending historical authors; incomparable
checkpoints; no publication of unsent checkpoint effects; offline delete-versus-edit recovery;
quota failures; expired listing/upload URLs; malformed/out-of-prefix responses; and native/
browser/live-B2 convergence. Verify that append mode never writes a shared head or sends the
unsupported conditional-write headers, while CAS-mode regressions continue to pass. Full CI
and actual UI inspection are separate verification gates recorded in [../tasks.md](../tasks.md).

### Setup, cutover and retention

For a new B2 library, configure native storage, then enable synchronization **before** managed
import: `cloud.syncEnable {"connection":"personal","protocol":"append-log-v1"}`. Join other
catalogs with the same prefix, library ID and protocol. The shared setup dialog offers
Append-only (Backblaze B2). Status reports author frontiers, gaps, pending work and conflicts;
there is no append generation or shared-head cursor. Sync now is incremental, with bounded
known-key/cached historical discovery. Its discovery cursor and fresh verification cursor
are separate; neither is publication authority. Automatic polling remains 60 seconds during
activity and 15 minutes after more than 60 seconds of inactivity, measured from completion.
Historical discovery's persisted five-minute deadline does not trigger fresh body downloads.
All checkpoint pages are discovered; unseen immutable checkpoints are hash-verified, causally certified
and merged once per device before their exact keys are saved with remote progress. Their device-local
consumption set and audit cursor are never adopted from remote checkpoints, transmitted in checkpoints
or included in publication-proof identity. Fresh verification rechecks consumed checkpoints. No listing-page
snapshot guarantee is assumed. New keys inserted before an audit cursor are found in a later round.
Staged dependencies are capped at 64 MiB, and up to 1,000 gap IDs are shown. Pending dependencies
still exist in the retained log and are rediscovered on the next scan.

For an existing CAS catalog, choose File → Migrate Cloud Library… (also available in Cloud
Synchronization and Info). Opening the dialog calls `cloud.prepareMigration`: this device's
automatic uploads and synchronization pause, and active catalog synchronization is cancelled.
Wait for running uploads to finish or cancel them in Activity. Other up-to-date devices need
no manual stop: the head's maintenance state refuses their commits while they keep browsing,
editing and receiving. The dialog's checkbox is needed only when there is no head to hold the
barrier (see above). Cancel keeps preparation paused in this session; explicit Sync now
resumes it. Preparation is not persisted across app restarts.

`cloud.upgradeLibrary {"plan":"appendLog","wait":false}` (alias `cloud.migrateAppend`) performs
the same cutover. Desktop and browser return a cancellable Activity task; native CLI/MCP wait
by default. Inspect `cloud.syncStatus` (`preparingMigration`, `migrating`, reports) for progress
and completion. Migration verifies committed CAS history on a worker, publishes/readbacks an
immutable seed, retires the head, then atomically saves mode/progress on the catalog owner.
Unsent pending IDs and mappings, including edits made while the worker runs, remain intact. A
failed native journal save rolls back the local cutover. Cancellation can leave the head in
maintenance with a verified seed; retry the upgrade (the same installation resumes at once)
or cancel it with `cloud.upgradeLibrary {"cancel":true}` only before successor publication
has been durably authorized. Once successor output may exist, finish the same upgrade.
Every other catalog
runs the same command once after retirement to rejoin; that rejoin writes nothing remotely.
Failed legacy upload receipts are explicitly rebound during this migration and retain their
staged bytes and multipart identities. This version rejects CAS synchronization when it finds
an append manifest for the same library. Builds released before the library control envelope
cannot see maintenance or retirement: keep those stopped. Legacy heads, changes and
checkpoints remain.

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

Dated provider and fixture runs are in [cloud-acceptance-history.md](cloud-acceptance-history.md).
The current [reliability checklist](cloud-reliability-tasks.md) records subsequent changes and
final-source CI. Neither an old acceptance run nor a completed feature implies current live-provider acceptance.

### Publication completion ordering

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
Receipt finalization after another worker acknowledges publication is durable (CR.27);
stage decisions and bulk request/byte invariants are verified in CR.13/CR.31.


### Writer identity and catalog restores

Writer ownership belongs to shared synchronization rather than the append protocol.
Native setup/open and the local mutation boundary protect both modes. Browser setup
and host installation claim both modes before editing; a failed claim blocks new
local mutations while allowing verified incoming reconciliation.

Native installation storage keeps `cloud-installation-id` and small per-catalog
`cloud-writer-sequences/<owner>.json` records outside catalog backups. A reservation
is durably saved before a newly authored mutation enters the live catalog/journal,
including explicit conflict resolutions and tombstone recovery. If an older backup
returns to the same path, or a crash leaves a reservation ahead of the catalog, a
new author starts before editing. Existing pending IDs, original payloads and
historical author clocks remain intact. Ordinary reopen keeps the current author;
there is no author-per-launch or author-per-import churn. Missing installation
records trigger a safe new epoch; malformed/future records fail closed and remain
available for recovery. Reservation write failure leaves the proposed catalog and
outbox changes unapplied. Installation identity creation publishes complete bytes
atomically, including concurrent process initialization.

Preserve installation storage when restoring catalog folders. Restoring an entire
installation image including those records requires new installation storage
(`LIGHTCRAFT_CONFIG_DIR`) so that the new current author cannot reuse the image's
writing epoch. The browser's supported Restore operation creates a fresh library
directory and reloads before accepting edits; it has no in-place overwrite route.

Conditional-head publication orders retained pending mutations by causal readiness,
including previous authors, before batching or media dependency checks. It cannot
publish sequence 1 of a new author before historical revisions in its context.
Missing retained causal history produces a recovery error with the outbox retained.
Append discovery/publication continues to enforce contiguous author prefixes and
causal readiness. Retired author clocks remain retained; author-slot retirement
is still unsupported.


### Cloud subsystem health

`cloud.syncStatus.health` and `cloud.overview.health.sync` report the actual polling
scheduler: disabled, unverified, checking, healthy, userPaused, migrationPaused,
halted or retrying. They include the subsystem's retained error and last successful
remote scan (time, task and polling/publication source). Resume allows a new check;
it clears the error only after that check succeeds. Upload publication can record
a verified scan but cannot restart halted polling or clear its failure. An unrelated
media/cleanup success clears only the same resource's error. Health resets when
opening a different catalog; obsolete workers cannot poison the new library.

The overview reports publication waiting, retained append gaps, structural and field
conflicts, blocked/failed imports and pending/failed local receipt jobs separately.
Bounded diagnostic details do not hide additional failed resources. Activity reports
remain history; the newest activity is not the source of cloud health. The popup and
icon show attention for structural-only conflicts and stopped polling. Up to date
requires a successful remote scan, no retained error, no pending publication/receipt
work and no unresolved conflicts. The displayed scan age explains the freshness of
that claim; after reopen the UI checks again before displaying it. Browser media
download failures are resource-specific and its host reports the same scheduler
health model.

Failure producers retain typed provenance through native and browser synchronization;
error message text never selects retry policy. Connectivity and contention failures retry
after 2, 4, 8, 16 and 30 seconds, then halt on a sixth consecutive failure. HTTP
408/425/429/500/502/503/504 are transient; 409/412 are contention. Authentication (including
401/403), integrity, invalid schema/history/library identity, local resources and unsupported
configuration halt immediately. Browser Fetch rejection cannot distinguish offline, TLS or
CORS failures, so it receives the same bounded connectivity policy and diagnostic guidance.
Native explicit TLS/URL configuration failures require repair. Failed migration never resumes
ordinary polling automatically; explicitly retry or cancel the cutover.

Health includes `lastError.kind`, optional `httpStatus`, `retryable`, `action` and `retry`
(`attempt`, `maxAttempts`, `exhausted`, `scheduled`, `nextAtUnixMs`, `secondsUntilRetry`).
The popup and Info show a countdown or recovery action. Retry deadlines use monotonic time;
wall time is display-only and may be unavailable. Resume/Sync now and browser reauthorization
reset the budget but retain failure evidence until a successful polling scan. Upload publication
does not clear that evidence or reset the budget. Pause/cancellation and obsolete owner results
cannot halt a newly resumed scheduler. Budgets are session-local; durable pending mutation IDs,
receipts and originals stay retained across restart. Retries never allocate replacement mutations.


### Logical undo and redo

Undo history records the metadata keys and individual album memberships changed
by each action. Before applying an inverse it combines saved values for those
fields with current values elsewhere. A title undo therefore preserves a received
caption; membership undo preserves unrelated received additions and removals.
Local-only album members are handled by the same local mask without entering the
shared projection. Nested inverses are prepared in application order with a bounded
depth. Metadata field construction is exhaustive so new model fields require an
explicit undo policy. Catalog operation/schema formats remain unchanged.

Revision guards retain the fields actually authored by that action, obtained from
its newly allocated mutation range. Merged undo entries retain each touched field's
latest local guard, rather than trusting the entire current entity again. Undo and
redo create fresh revisions and carry their logical field masks forward. A later
competing revision to a touched field still refuses the operation atomically, with
the history entry available for explicit conflict-aware recovery. Independent
remote fields do not become accidental targets of a whole-metadata inverse.


### Native upload stages and transfer evidence

A native upload task prepares each selected import's media once, reconciles one
ready metadata batch through the per-library owner gate, then finalizes covered
receipts after the owner has journaled exact proof. Local receipt-write failures
retain that durable queue and retry independently of media transfer. Abort is a
separate stage. Failed transfers are reported per import rather than silently
repeated in additional append publication passes. A batch shares one immutable
synchronization snapshot instead of cloning the entire state per item.

Successful full hash/size checks supply bounded verification evidence to publication
checks and sibling tasks through the live library coordinator. Its key includes
connection revision, destination, object key/version, hash and size. A separate
short-held lock tracks active producers; publication waits for their durable media
readiness instead of downloading the same original during each attempt. No network,
staging I/O or owner reply runs under this readiness lock. The publication gate
continues to cover metadata through owner durability.

Evidence is never reconstructed from disk markers. A new library/connection epoch,
completed outbox without active producers, or explicit verification retires it;
reopen and deliberate verification check bytes afresh. Overflow falls back to actual
verification. Dependency reads enter the task's transferred-byte counter. A late
ready import supplies exact checked proof for a durable finalization job even when
another worker already removed its mutation from the current outbox.

Sidecar writes under the staging lock preserve compatible exact proof and the
newest generation supplied by another finalizer. An obsolete ordinary phase cannot
erase its synchronized marker; deliberate later verification can still fail while
preserving historical publication evidence and keeping staging protected. Per-import
completion and fresh-media signals scope recovery correctly; abort/local finalization
cannot clear a verification failure. Typed maintenance purposes prevent user
connection names from colliding with receipt-worker routing/reservations.


### Typed transport outcomes

Native and browser transports share `cloud_outcome::Outcome`: an unchanged certified
head scan, a conditional-head result, or append progress with exact acknowledgements.
An unchanged hint retains its portable evidence; the owner compares its current
catalog before deciding that no write is needed. Conditional-head results explicitly
separate an empty remote head from a published head. Published positions require a
SHA-256 cursor and nonzero generation. Empty-head results cannot carry ACKs; append
results have one authoritative portable state inside progress and no head fields.

Every variant also retains a typed publication capability: allowed, a specific
`cloud_control::Blocked` reason, or an intentional receive-only snapshot. Clean clients
and unchanged scans retain the observed maintenance, newer-writer or retirement refusal.
Recertifying portable state preserves this capability alongside the position and ACKs.

Owner application preserves newer head positions, merges append evidence, and
checks requested ACKs against the result. Browser received state reaches storage
before an ACK receipt retires pending mutations. Its head receipt uses the retained
current position after reconciliation; empty-head state flushes without a fabricated
ACK. Existing version-1/version-2 browser receipt files remain readable; their legacy
on-disk encoding does not define the transport result type. Catalog/wire formats
are unchanged by this refactoring.

`cloud.sync` completion reports include `outcome` (`unchanged`, `conditionalHead`,
or `append`). Only a published head includes `cursor` and `generation`; an empty
head reports `head: "empty"`, and append reports `frontiers`/`gaps`. Poll status still
exposes the saved catalog's protocol-specific state. Append request-cost and owner-application bounds are specified below;
remaining state-sized work is recorded in the current implementation limits.

Completion reports include `publicationAllowed`. A refusal retains the compatible
`publicationBlocked` message and adds normalized `publicationFailure` provenance
(`kind`, `recovery`, `action` and retryability). Poll status and overview retain that
failure separately from polling's `lastError`: a readable blocked scan still succeeds
and follows the regular activity-aware polling cadence, rather than spending failure
retries. Capability is runtime evidence; an unchanged scan does not append a catalog
operation merely to refresh its displayed refusal.


### Shared structural projection

Folder-parent resolution and shared stack ownership live in
`cloud_sync::shared_projection`. They take only shared synchronization state,
returning global identities without consulting a catalog, local IDs or overlays.
Cycles sever the greatest global album identity's proposed edge while retaining
all causal proposals. Missing/deleted parent targets become roots; malformed
parent values and live nonfolder parents remain actionable errors.

Stack validity and ownership are computed before adding local members. The
catalog adapter allocates local identities, installs the resolved graph and then
preserves private content separately. Generated tests compare the actual shared
projection across different local IDs, mixed local groups, concurrent proposals,
tombstones, reverse insertion/delivery order, restart and repeated application.
Separate properties verify preservation of private photos, filters and groups.
No wire-format change or large-library performance claim accompanies extraction.


### Native conditional-head history recovery

Native readers stream backwards through immutable batches into an operation-local
verified byte spool, then revalidate one batch at a time for oldest-first replay without
a second GET. A fixed 4,096-slot reference ring uses at most 16,826,368 bytes and a
circular body arena uses at most 512 MiB. Decoded history batches never accumulate.
Constant-space cycle detection and exact parent/hash/size/schema/scope checks remain
enforced. Scratch failures report temporary-disk recovery and preserve data. Normal
completion/error/cancellation releases both files; Windows delete-on-close and Unix
unlink-on-open also reclaim files after abrupt process exit.

Ordinary writers and caught-up idle polls automatically checkpoint at 1,024 tail
batches. Histories beyond the retained record or byte window recover through verified
oldest-prefix checkpoints. Each prefix is installed through conditional head CAS
before any local pending edit is applied or acknowledged. Completed prefixes survive
restart and cancellation; retry reads the new boundary. Further windows rescan the
remaining chain instead of requiring it all to fit in memory or scratch storage.
An acknowledged baseline is reused only without pending local effects and after its
cursor is found in the certified ancestry. Migration's read-only path uses successive
windows without changing the head.

Spool proof lasts for one read, never grants authority and does not survive a verification
budget slice. Separate recovery/CAS/publication-confirmation reads may fetch records
again; fresh remote verification always obtains current bytes. Unchanged small head polls
retain their existing no-write path. Browser replay still has separate traversal/replay
GETs below. CR.79 records exact native request/byte savings and memory evidence; append
checkpoint consumption and audit bounds follow below.

### Native remote baseline and pending overlay

An optional disposable local baseline contains certified pure remote state at an exact
already-durable destination/library/cursor/generation. It never contains the device's
unpublished overlay. On absence, corruption, retargeting or position mismatch, pending
work still forces full verified replay. Current authority and ancestry are read before
advancing the baseline, and exact remote receipts remain necessary to acknowledge edits.
The last observed remote state seeds publication confirmation; optimistic candidate effects
cannot seed that read, including after lost replies or competing CAS. Baseline cache writes
do not change the outbox and do not replace the atomic catalog state/cursor/ACK operation.
The saved checkpoint anchor must match current authority before incremental reuse. New
checkpoints trigger reconstruction so checkpoint-only recovery effects are received; a
changed anchor at the same causal position is refused. Fresh verification and checkpoint
maintenance retain uncached remote reconstruction.

### Browser bounded history replay

Browser head replay now uses the same 4,096-record window, 1,024-record automatic
checkpoint interval and constant-space cycle detection as native replay. Its
reference deque enforces a 4 KiB serialized reference limit and retains at most
4,096 references; batches are fetched/verified again one at a time oldest-first.
A longer tail checkpoints verified prefixes through CAS before publishing local
mutations. Migration reads all windows without changing the head. Hash, size,
parent, library and checkpoint-boundary checks remain mandatory in both passes.
Neither path retains a decoded batch tail. Request minimization is a separate
tradeoff: fresh windows need traversal and replay GETs, and very long histories
can need additional passes. Local materialization and append request bounds are specified below.


The checked-in Windows regression `apps/lightcraft-cli/tests/cloud_replay_memory.rs`
uses `scripts/cloud-replay-memory.ps1` (PowerShell 7) to measure only the reader
child, excluding the provider fixture's retained source objects. It compares 100
and 1,500 superseding 256 KiB caption batches, checks small final state, resident
and commit growth, and exactly one payload GET per historical batch in a read. The measurement is Windows-only; Windows without PowerShell 7 reports a skip.
Shared/native bounded-window tests remain portable. These are catch-up working-set checks, not
owner-thread or slider-latency benchmarks.


### Creating and joining libraries

`cloud.syncEnable` without `library` creates a library and authors its immutable
initialization once. Supplying an existing library ID joins it. A join adopts shared
library settings and identity from the verified destination, without publishing
another initialization of those fields. Local photo/album work may still be queued
with its original mutation IDs; synchronization reads existing remote history before
publication. A missing head/append manifest is an actionable join error, even if
there is queued local work. The local `requireExisting` binding survives restart;
it is excluded from portable checkpoints. Existing frozen bootstrap mutations retain
their IDs and contexts, including those authored before this distinction existed.
Read-only joins create no remote operations and consume no shared author clock slot.

Native and browser append workers apply a ready operation to their worker-local
projection and validate combined capacity before its immutable PUT. They return no
acknowledgement if PUT fails; an uncertain successful PUT is recovered by exact
content verification. This prevents a locally predictable rejected operation from
poisoning the remote log. It does not replace validation of concurrently discovered
remote operations or raise field/author limits.


### Local materialization and journal deltas

The catalog rebuilds a nonserialized reverse identity index once after loading and
maintains it with identity operations. Received metadata edits decode changed photos;
photo arrival/content/deletion also refresh dependent copies, selections, covers and
memberships. Folder and overlapping-stack changes keep deterministic global graph
resolution, followed by device-local overlays. Receipt/cursor-only progress does not
materialize unchanged documents.

Local catalog format 18 introduced `patchSyncState` (current format: 19). After writer reservation, shared native
and browser session code replaces full synchronization-state updates with typed
field/register, receipt, archive and outbox deltas. Append publication snapshots use
nested portable deltas too. Each edited value records its expected predecessor;
failed preconditions or full-state validation preserve the base. Inverses participate
in atomic batch rollback. Initial enable/disable still uses `setSyncState`. Remote
mutation/checkpoint formats and capacity limits are unchanged. Older local formats
upgrade before any new journal append; older builds refuse format 18.

Schedulers capture the exact catalog revision and connection with their saved
worker snapshot. A transport `Unchanged` result can skip full document comparison
only while that revision and the library/provider bindings are still current;
any owner edit/progress uses the full evidence comparison and reconciliation path.
Native workers receive their large result snapshots back for destruction after
owner persistence, retaining the publication permit through durable acknowledgement.

Native `cloud.sync` results include `ownerApplyMs`: owner result comparison,
reconciliation, application and persistence time. It excludes network-worker time
and startup snapshot/ownership work. The scale fixture measures nonwaiting startup
dispatch wall time separately. These are not end-to-end UI latency measurements. Full logical merges, state validation, catalog
container clones and snapshots can still scale with retained state. See CR.9
acceptance for exact fixture sizes, request counts, journal growth and timings;
append checkpoint consumption and audit budgets follow below.


### CR.10: append discovery and historical audit budgets

Local catalog format 19 retains consumed checkpoint keys and a monotonic audit round/key cursor.
The reader keeps its durable verified remote state during audits instead of reconstructing it from
zero. Ordinary discovery still rescans author roots and every checkpoint listing page, but downloads
only new checkpoints and operation tails beyond certified frontiers. Gaps never advance those
frontiers. Audit slices share the same policy on native and browser transports: at most 64 history
records, 16 audit listing pages and a stop after 8 MiB of record payload. A single valid object may cross that byte target
(up to the existing 64 MiB checkpoint limit); Merkle witness fetches have their separate existing
node/path/cache bounds. Bootstrap and newly arriving data are independent of this historical budget.
Objects already fetched by discovery in the same scan are reused by the audit. A checkpoint first
seen during an audit waits for the next ordinary discovery pass, before operation-tail staging;
it cannot strand already downloaded records by changing their causal base midway through a scan.

`cloud.syncStatus.appendAudit` and completed sync reports expose round, cursor and completion;
`consumedCheckpoints` exposes the local consumption count. Audit cursors advance only with successful
verified owner persistence and resume after restart. Late completions merge consumed keys and keep
newer audit positions. A failed audit retains the prior durable cursor and pending work. An idle
poll after an audit completes performs no covered-operation, checkpoint or archive GETs, though
manifest reads and LIST discovery remain. Checkpoint listings, state validation/merging and discovery
of many authors are still bounded linear work; this is not a general large-library latency claim.


### Explicit metadata schema boundary

`crates/engine/src/cloud_sync/portable.rs` and its filter codec explicitly define
shared photo, immutable content, descriptive metadata, album, stack and global
reference representations. Projection never serializes a complete catalog photo
or album and removes private fields afterward. Incoming materialization applies
named, typed shared fields to local records; catalog IDs, paths, local flags,
working-copy bindings, upload intents and cache state have no incoming field slots.
Existing wire names, flattened `meta.*` registers, optional omissions, sparse
legacy content defaults and explicit empty-selection semantics are retained.
Valid existing wire payloads are unchanged. Previously ignored unknown smart-filter fields and malformed global-reference strings now produce actionable errors rather than receiving an accidental local interpretation.

Photo and album references occupy schema-defined slots, including nested smart
rules. Missing photo targets remain unavailable, and album targets reserve local
identity mappings without creating live albums. Arbitrary rule payload properties
are not recursively rewritten. Smart albums containing filesystem path rules or
per-device synchronization-status criteria, together with their dependent smart
albums, remain device-local under the existing nonportable-rule policy.


### Library control and format upgrades (2026-10-17)

"Old software detects an unsupported format" prevents misreading data; it does not
coordinate changing a format while other writers exist. Every library therefore carries a
small, stable **control envelope** (`crates/engine/src/cloud_control.rs`):

| Field | Meaning |
|---|---|
| `controlVersion` | Envelope format. A newer one is refused ("update LightCraft") before any other field is decoded |
| `epoch` | Active generation of the library's metadata, not an edit sequence |
| `schema`, `minimumReader`, `minimumWriter` | Separate read and write compatibility: a build may display a library it must not publish to, because deserializing is not proof that writing keeps newer fields |
| `state` | `active`, `upgrading` (with `migration`) or `retired` (with `successor`) |
| `activeMetadataRoot` | Epoch namespace: `catalog` for epoch 1, `catalog/epochs/<n>` later |
| `revision` | Increments on every control transition |
| `migration` | Stable `id`, `plan` (`rebase` or `appendLog`), target epoch/schema, exact frozen `source` (head tip and generation), `owner`, fencing `token` (it stops a stale owner, not a malicious writer), `leaseUntilMs`, and format-2 `exposure` (`preparing` or `publishing`; absent v1 means unknown) |

This build reads and writes schema 1 (the current wire set). Version 2/3 heads, written
before the envelope existed, read as Active epoch 1 schema 1 and become version 4 on the
next conditional write; older builds refuse version 4, so they fail closed rather than
overwriting it. Pending mutations are never rewritten: translating a mutation between
schemas is refused in this build (`translate_mutation`), and a future translator must keep
the original identity/digest and record a durable original→translated provenance mapping.

Maintenance readability does not grant upgrade capability. A shared typed converter
registry resolves the plan plus the enclosing control's frozen source schema and the
migration's target schema. This build implements only schema-1 → schema-1 rebase and
append migration. New migration acquisition and every takeover (including the same
installation resuming, cancellation, and the browser append runner) refuse unsupported
conversions before writing control markers or migration outputs. Activation derives
reader/writer requirements from the selected converter. A readable maintenance record
targeting a future schema stays unchanged and reports an unsupported upgrade with an
instruction to update LightCraft; it is not evidence of malformed history. Structural
validation deliberately continues to accept that record so newer builds can recover it.

**Conditional head: one authority.** The control lives inside `catalog/head.json`, so
ordinary commits and maintenance compete at the same compare-and-swap. Every publisher
checks the control of the head it just reread, immediately before the CAS that carries that
control forward unchanged (including recovery-prefix and automatic checkpoints). A commit
prepared before maintenance loses its `If-Match` and rereads; it then stops. While publication
is blocked (maintenance, retirement, newer writer schema) synchronization still receives and
certifies remote state, acknowledges only mutations already present remotely, keeps the rest
pending, and reports `publicationBlocked` with an actionable reason.

Transitions are empty marker batches plus a head CAS, so tip and generation always advance
together (cursors and the unchanged-poll path stay consistent). They never use the ordinary
"conflict means reread and retry" rule as proof: after each attempt the upgrader rereads the
head and accepts only exactly the control it wrote (new revision and, while upgrading, its
token). That resolves lost responses, including a lost activation response.

```text
Active(epoch e, root R)
  → Upgrading(e, frozen source S, migration M, owner O, token T)      begin / takeover
  → Active(e+1, root R')       rebase: only token T; checkpoint in R' read back first
  → Retired(successor)         appendLog: only token T; seed published first
  → Active(e)                  cancel only while exposure=preparing: only token T
```

Whole-library erasure adds a terminal `Deleted` control state with no migration or
successor. It rejects reads, publication and upgrades. After reviewed typed consent,
the native eraser installs this barrier before deleting data. A conditional-head barrier
uses the observed ETag and replaces personal-history references with inert placeholders;
this terminal transition deliberately does not append a batch or keep a readable tip.
Append erasure publishes an immutable Deleted control record at the next revision.
Old envelopes/history are removed, retaining only the terminal barriers. Other devices
must be stopped; append control is cooperative. Native local deletion-intent markers
keep the initiating device from uploading again, including after interruption/restart.

The lease decides only when another device may *attempt* a takeover; wall clocks never
authorize a commit. A takeover keeps the migration id, plan and frozen source, issues a new
token (so a resumed stale owner can no longer activate or cancel) and never lets ordinary
writers resume. The same installation may resume its own migration immediately. Cancellation
also requires a durable `preparing` exposure marker.

Control format 2 adds `migration.exposure`. New acquisitions start in `preparing`. Before
the first successor PUT, native and browser runners CAS the same head to `publishing`,
confirm the exact control by rereading, and only then write output. This is a one-way latch:
failed or uncertain PUTs, renewal and takeover retain it. A racing cancellation and output
authorization cannot both win the head CAS. `publishing` means output **may** exist; its
absence on a later LIST does not authorize returning the old library to Active. Retry/finish
the same migration using its frozen source and immutable output identities; never blindly
delete a possibly used successor. Older v1 maintenance records have unknown exposure and
also require completion. Healthy initial envelopes remain format 1; maintenance transitions
promote to format 2, which older clients refuse before decoding new fields. Newer-envelope
head errors retain typed update/recovery advice through native and browser transports.

`cloud.upgradeLibrary {plan?: "appendLog"|"rebase", cancel?, writersStopped?, wait?}`:

* `rebase` writes the verified frozen state as a checkpoint in the next epoch's namespace
  beside the old one (no original or rendition is copied), reads it back and compares it
  with the source, then activates. Retries reuse the same content-addressed objects.
* `appendLog` (default; alias `cloud.migrateAppend`) seeds the append namespace and retires
  the head. Run on a device whose head is already retired, it only rejoins: read-only
  discovery, local protocol switch, pending mutations unchanged. The browser supports
  `appendLog` and `cancel`; `rebase` runs in the desktop app or CLI.
* `cancel` returns a `preparing` library to its unchanged epoch. `publishing` and legacy
  unknown exposure are refused without changing local or remote data, with an instruction
  to finish the same upgrade. Stopping a local worker only halts that attempt; it does not
  reverse the remote maintenance state.

**Rejoining.** A device that was offline sees the new epoch on its next read: its cursor is
not in the new chain, so it rebootstraps from the new epoch's checkpoint and then publishes
its preserved pending mutations with their original identities and digests. After epoch
`e+1` accepts edits, rollback is no longer "point back at epoch `e`"; those edits would also
have to be preserved or migrated. The old epoch's objects remain for recovery.

**Append log: cooperative barrier.** Append mode has no mutable object or CAS. Its control
is a set of immutable records `sync-v2/<library>/control/<revision:020>-<sha256>.json` with
the same envelope. Each scan reads the records at the greatest revision before any write;
none means Active epoch 1. A newer reader requirement or invalid record refuses the scan; a
newer writer requirement, maintenance, retirement or two different records at one revision
pause publication (manifest creation, operation PUTs and checkpoints) while receiving
continues. This is cooperative: a write already in flight can still land, and no producer of
these records ships in this build. A hard append barrier needs an election-capable control
adapter (B2 `b2_update_bucket` `ifRevisionIs` bucket info, or a small commit coordinator) and
late-write discovery in a retired namespace; both are tracked in
[cloud-reliability-tasks.md](cloud-reliability-tasks.md). S3/B2 Object Lock is not a mutex: it
retains object versions but does not stop new versions or objects.

**Compatibility boundary.** No build can be made to obey a protocol it never implemented.
Head-mode builds before version 4 fail closed on a version-4 head. Append-mode builds before
the control records cannot see them; keep such prerelease builds stopped, and restrict old
write credentials when it matters. An application-level lock cannot stop a client holding
unrestricted bucket credentials.

Evidence: `cloud_control` unit tests (two simultaneous upgraders, commit racing maintenance,
crashed upgrader takeover, lost activation, cancellation/retirement, malformed and newer
envelopes) and `apps/lightcraft-cli/tests/cloud_upgrade.rs` against the signed loopback S3
fixture (a commit losing its CAS to maintenance stays pending; takeover of an expired lease
and rebase to epoch 2 with a lost activation response; a held lease refuses other upgraders;
cancellation; append retirement and offline rejoin with identical pending mutations; append
control records blocking and refusing publication). Live AWS/B2 runs were not repeated.


### B2 head/intake adapter components (2026-10-18; integration pending)

These components are not selected by ordinary native/browser synchronization or
upgrade commands yet. CR.43 remains open. They add the following supported records
without changing semantic schema 1 or ordinary AWS head emission:

- Head v5 optionally carries `legacyAppend`, a content-addressed reference under the
  original `catalog/legacy/<sha256>.json` root (maximum 64 MiB). Its v1 intake binds
  library identity, exact original append manifest hash, and bounded append discovery
  state. `Head::next` preserves the reference; heads without it still emit v4.
- Control v3 is restricted to the original epoch-1 append root, retired toward
  `conditional-head-v1` epoch 2, or permanently deleted. Its constructor validates
  the exact original manifest. Ordinary maintenance emits v2. Older max-v2 control
  readers refuse this fence before interpreting its fields.
- A fixed `catalog/b2-authority.json` descriptor binds the complete configured
  bucket/endpoint/region/prefix. Only the native bucket-info revision CAS commits
  an immutable `catalog/authority/<sha256>.json` Head. An absent pointer after the
  descriptor appears is an interrupted upgrade, not an empty writable authority.
  Initialization validates an exposed maintenance head, frozen checkpoint/markers,
  supported source control/manifest, native permissions and absent or exactly retired
  raw head. Subsequent control transitions retain fencing/exposure/source evidence.
- Legacy recovery reads only original epoch-1 append operations/checkpoints and
  immutable archive proofs. Its result is uncommitted input: a future sync caller
  must CAS both the causal checkpoint and updated intake before returning an outcome
  or acknowledging anything. Scan-only progress can retain the causal head position;
  historical audit rounds are explicitly requested and keep the existing budgets.
- Deleted heads contain no checkpoint, legacy reference or personal-history tip.
  Their exact nonpersonal zero-reference shape is validated separately; normal
  readers continue to return Deleted. The native adapter rejects all revival writes.
- Native B2 429/503 responses establish a bounded host cooldown and retain an integer
  Retry-After delay through typed failure serialization and retry scheduling. They
  stop immediate CAS retry loops; a scheduled attempt rereads authority first.

The ordinary upgrade runner, shared authority selection, browser signer RPC,
discovery/rejoin, ongoing recovery admission and full-erasure barriers still need
integration and workflow acceptance before this protocol is usable from the app.
