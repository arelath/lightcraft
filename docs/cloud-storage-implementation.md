# Cloud storage implementation

Last updated: 2026-10-20. This document describes current behavior and operational limits. The normative wire and
durability contract is [cloud-sync-protocol.md](cloud-sync-protocol.md); its receipt-history
contract is [cloud-history-compaction-design.md](cloud-history-compaction-design.md).
[browser-cloud.md](browser-cloud.md) defines the browser adapter contract.
[cloud-acceptance-history.md](cloud-acceptance-history.md) preserves dated acceptance;
[cloud-reliability-tasks.md](cloud-reliability-tasks.md) is the authoritative remaining-work
checklist. [../tasks.md](../tasks.md) indexes the original milestones. The initial
[proposal](cloud-storage.md) is historical, not the implemented specification.

B2 account download caps can reject even an absent library manifest in an empty bucket.
LightCraft recognizes the bounded S3 `AccessDenied` response for B2's download bandwidth/
Class B cap and reports a provider limit, retaining HTTP 403 and pending work. Setup gives
Caps & Alerts guidance; synchronization stops automatic retries until explicitly retried.
The same recognition runs before the browser refreshes an authorized URL. Other 403
responses retain their authentication classification. Provider caps are separate from
library receipt/history limits and never route to LightCraft's Storage tools.

## Native traffic diagnostics

`cloud.traffic {limit?: 0..256, operation?: id}` is a read-only native query for the
**current process's observation period**, starting at `startedUnixMs`. Query the running
app through the control channel, or put it after sync/upload commands in the same CLI
invocation. A new CLI process starts a new observation period. `cloud.overview.traffic`
provides the compact process totals; these are not account-wide usage or billing totals.

Accounting covers S3 metadata/listing/upload requests, full media downloads (including
upload verification), and native B2 authorization/bucket-control calls. Each completed
transport attempt emits one structured `lightcraft_cloud_requests` debug record with
method, operation kind, status, recognized B2 cap refusal, elapsed time, body bytes,
logical purpose and operation ancestry. Connect/cancel/source/read failures are included;
HTTP failures retain their status. Attempts can fail before reaching the provider.
`attemptInOperation` is the HTTP-call ordinal, not an inferred retry number.

The diagnostic reports per-operation and destination totals, per-import scopes within
upload batches, local cache/evidence and shared-proof hits, and up to 24 recent hourly
buckets. `backgroundPollAttempts` and `backgroundPollBodyBytesRead` isolate automatically
scheduled synchronization, including its audit traffic. Hour bins are relative to process
start; `observedMs` identifies partial bins. Parent operation totals include child work;
do not sum parent and child totals together. Global totals count each attempt once.
Snapshots expose active attempts separately; body-byte totals finalize when responses
are released. They do not replace live transfer progress in Activity.
Elapsed-time totals sum request lifetimes, including streamed body consumption; parallel
request durations do not represent job wall time.

Byte counts describe body bytes read by the application and outbound chunks confirmed
by the transport. Headers, TLS, framing and unread/prefetched response bytes are excluded.
Partial failures can therefore give a **lower bound**, never a provider invoice estimate.
Successful fully consumed fixture transfers are checked against backend payload counts.
The combined B2 download/Class B cap message still does not identify which cap was hit.

Request records contain hashed destination, resource, library and import identities,
not credentials, headers, signed URLs or raw object paths. Current connection names and
the public cloud-library ID have a separate identity lookup in the response. Callers
without a known library/operation remain explicitly unscoped; they still contribute to
process/destination totals. Recent detail is bounded to 256 requests and 64 operations;
active operations are protected from detail eviction. If all operation slots are active,
`untrackedOperations` records omitted new detail while global traffic totals remain.
Destination detail is bounded to 32 identities plus an overflow bucket. The current
connection-name lookup is limited to 32 bindings, with an explicit truncation flag. No cache,
publication proof, upload verification or polling policy changes are implied by these
diagnostics. Native immutable-body reuse is described below; further traffic reductions
remain tracked by CR.83–CR.86.

## Native immutable history cache

Native filesystem libraries reuse verified append operation/checkpoint and receipt-archive
bodies in `<library>/sync-record-cache-v1/`. The disposable cache is independent of the
movable original-media cache; moving media storage does not move history bodies. No cache
binding is serialized into a connection, upload receipt or cloud record. In-memory catalogs retain their existing adapters. Browser append-body reuse is described below. Native conditional-head batch replay uses the operation-local spool described below; browser replay remains CR.86.

Entries bind the canonical destination (connection ID, bucket, region, prefix and endpoint),
exact immutable key including its content identity, verified actual byte length and body
SHA-256. Credentials and profile selection do not affect that binding. Reads check the
binding/header/length/hash and the original content-addressed key and schema before reuse.
Causal merge, receipt certification, fork detection and acknowledgement run again against
the current planner. Cache bytes never establish current authority or remote existence.

Published bodies are limited to 4,096 entries and 256 MiB per library, including envelope
bytes; admission also reserves the temporary file's bytes. Each body retains its protocol
limit (at most 64 MiB). Recent use guides eviction, with file modification order as the
restart baseline. Initial directory discovery stops after 8,192 entries. Unknown files and
interrupted temporary writes are never reused or deleted and consume admission capacity;
an unusable or overfull cache falls back to remote reads. Cache loss or disk-write failure
does not change synchronization state. Atomic durable publication prevents partial bodies
from becoming valid entries. A cancellable OS file lock and durable pre-mutation generation
serialize independent cache instances and invalidate stale indexes, including old workers
winding down after a library switch. The fixed eight-byte lock record is outside the body
budget. Cancellation is checked during reads/writes and both lock waits.

Historical listings and resumable discovery remain active. A different hash at an old
sequence still goes through ordinary content/causal validation and fork rejection. Exact
acknowledged operation receipts and consumed checkpoint identities form a durable known-key
index: historical discovery can skip reconstructing those accepted effects entirely, even
if the disposable body cache was evicted. These identity checks are not fresh remote-byte
proof. Unknown/gapped records still need their verified bodies. Explicit maintenance and
receive-only audits bypass cached bodies, including nested receipt reads.

## Native version-bound media verification receipts

Persistent native libraries may reuse a completed full SHA-256/length verification
after recovery. A disposable receipt records the canonical destination, exact key,
expected hash/size, requested historical version (if any), actual provider response
version from the completed byte/checksum proof, verification method and successful result. Only AWS's default endpoint
and recognized canonical B2 endpoints qualify; unknown compatible providers retain
full readback. Empty, null, invalid or absent version IDs never qualify.

Every recovered proof requires a new HEAD for the applicable object (including its
versionId when pinned). Both actual version and size must match the completed byte
proof. HEAD is not new content proof: changed/missing versions, damaged/missing
receipts or unsupported metadata fall back to full SHA-256 readback; a missing
object is unverified. Authentication, cap, cancellation and network failures
propagate rather than silently downgrading verification. Fresh verification skips
receipts. In-memory shared readiness and exact durable publication acknowledgements
retain their existing independent roles.

Receipts share the disposable sync-record-cache-v1 envelope, atomic publication,
OS locking and 4,096-entry / 256 MiB budget; each receipt is at most 16 KiB. Cache
failure or eviction only loses this optimization. No catalog or cloud wire schema changes. Multipart receipts may include the optional
checksum-selection/evidence fields described below. Successful single PUT and validated multipart
completion responses expose their version IDs, but those headers alone do not
create a content proof. Uncertain completion still requires complete readback.

This reduces repeated verification download bytes after restart, not Class B
transaction counts: HEAD is still a provider request. Native
in-memory libraries and versionless providers retain full verification. Qualified new-upload checksum admission is described below. Provider contracts: [AWS version IDs](https://docs.aws.amazon.com/AmazonS3/latest/userguide/versioning-workflows.html),
[B2 GET](https://www.backblaze.com/apidocs/s3-get-object),
[B2 HEAD](https://www.backblaze.com/apidocs/s3-head-object),
[B2 PUT](https://www.backblaze.com/apidocs/s3-put-object) and
[B2 multipart completion](https://www.backblaze.com/apidocs/s3-complete-multipart-upload).
Validation uses local signed counterparts; live provider acceptance remains open.

## Shared native quota admission

S3 metadata, streamed media and native B2 control check one process-wide admission
registry before starting each HTTP attempt. A recognized B2 cap refusal suspends
the same provider/credential scope across buckets and library prefixes. The native
JSON cap codes and the existing exact S3 download/Class-B diagnostic are recognized
from bounded error bodies. Ordinary permission errors do not suspend an account.
The registry retains a salted credential digest, never keys, bearer tokens or signed
URLs. Canonical B2 regions share one service identity; AWS and unknown compatible
services remain separate. Different keys belonging to one account cannot reliably
be associated through S3 alone; a changed key may need its own first refusal.

HTTP 429/503 Retry-After seconds share a monotonic cooldown across all three paths.
Delays retain the existing bounded policy (1 second–15 minutes, 2-second default).
An explicit Retry Syncing/Sync now/upload retry clears a cap, but cannot bypass an
active cooldown. Automatic starts never clear it. A restart permits another probe.
There are at most 128 suspended scopes; overflow fails closed rather than evicting
an active restriction. Overflow suspension requires restart to recover.

Already in-flight attempts may complete. Offline-copy workers stop on a cap or
throttle. Preview failure propagates that refusal instead of fetching the original;
already cached media remains available. Upload workers stop before untouched items
change phase, and automatic continuation stops until explicit retry. Per-item errors
still allow unrelated transfers, and uncertain-write receipts retain reconciliation.
No quota state changes publication proof, deletion admission or durable ACK ordering.

Provider references checked 2026-10-20: [B2 integration checklist](https://www.backblaze.com/docs/cloud-storage-integration-checklist)
and [native transaction-cap response](https://www.backblaze.com/apidocs/b2-create-bucket).
Validation uses signed loopback storage and synthetic cap/cooldown responses; it does
not consume live B2 allowance or establish live AWS/B2 acceptance.

## Qualified native upload checksums

AWS's default endpoint supports a conservative SHA-256 admission path. A single PUT
sends the expected checksum in its signed checksum header, hashes the exact streamed
source bytes again, and requires the matching provider checksum and a non-null version.
Multipart uploads explicitly select SHA256/COMPOSITE at initiation. Selection and
per-part provider evidence persist with the resumable upload; missing old fields default
to no checksum admission. Every source chunk is hashed again, including skipped/resumed
parts, and the whole source SHA-256 must match before completion. Ordered part hashes
produce the expected binary-digest composite plus part count. Completion must return
the exact checksum, COMPOSITE type, bucket/key and immutable version. HTTP-200 Error
documents are failures. ETags remain concurrency tokens; user-authored metadata is ignored.

An opaque completed-upload proof is scoped to destination/key/hash/size/version and
cannot become publication readiness until a separate HEAD confirms that exact current
version and size. It then persists through the same bounded media receipt cache. Missing
or version-mismatched evidence falls back to full readback; contradictory checksums/types
fail integrity validation. Uncertain responses, explicit fresh verification, older uploads,
unconfirmed selection and missing part/completion proof retain full readback. Successful
validated multipart completion removes its durable unfinished-upload entry, avoiding a
redundant abort; uncertain completions retain their original reconciliation/cleanup path.

B2 and custom compatible endpoints do not opt in based on S3 compatibility or echoed
metadata. Their new uploads still receive full readback; B2's version-bound recovery
receipts remain useful. AWS buckets without usable version IDs also retain readback.
No live AWS bucket is available for acceptance. This path is verified against a signed
local counterpart that independently checks payloads, selection, part checksums and
the final composite. Full media downloads remain the compatibility and audit path.

Provider contract references, checked 2026-10-20: [AWS PutObject](https://docs.aws.amazon.com/AmazonS3/latest/API/API_PutObject.html),
[CreateMultipartUpload](https://docs.aws.amazon.com/AmazonS3/latest/API/API_CreateMultipartUpload.html),
[UploadPart](https://docs.aws.amazon.com/AmazonS3/latest/API/API_UploadPart.html),
[CompleteMultipartUpload](https://docs.aws.amazon.com/AmazonS3/latest/API/API_CompleteMultipartUpload.html),
[checksum selection/type rules](https://docs.aws.amazon.com/AmazonS3/latest/userguide/checking-object-integrity-upload.html),
and the [SHA-256 composite example](https://docs.aws.amazon.com/AmazonS3/latest/userguide/tutorial-s3-mpu-additional-checksums.html).
The current [B2 PUT](https://www.backblaze.com/apidocs/s3-put-object) and
[B2 completion](https://www.backblaze.com/apidocs/s3-complete-multipart-upload) documents
do not establish an equivalent validated SHA-256 guarantee; support is deliberately not inferred.

## Native conditional-history replay spool

Within one native conditional-head read, backward traversal saves the exact verified
batch bytes and forward replay reuses them without another object GET. Replay rechecks
scope, size, SHA-256, schema and parent continuity. This ephemeral proof belongs to the
observed history; it does not grant current authority, establish later remote existence,
or bypass fresh authority/CAS confirmation. Another read or explicit verification fetches
remote bytes again. The reader still refuses pending local effects as a baseline; certified pure remote state is supplied separately as described below.

Two private temporary files retain at most 512 MiB of bodies and 16,826,368 bytes of
references (4,096 slots). A bounded offset deque tracks complete retained batches. The
circular body arena keeps the oldest complete replay prefix even when the byte limit is
reached before the record limit. Only one raw and one decoded batch are resident; bodies
remain limited to 2 MiB. Cancellation is checked during chunked I/O. Scratch damage or
storage failure reports temporary-disk recovery and preserves catalog/pending work.
Windows deletes exclusive scratch files when their handles close, including process exit;
Unix unlinks them on creation. Other hosts remove owned paths on normal drop.

Longer histories still traverse the chain to certify its boundary. Recovery installs
verified oldest-prefix checkpoints through the existing conditional authority before
continuing; read-only callers walk successive windows. An acknowledged cursor with no
pending edits can seed a byte/record-limited prefix without repeating its initial scan.
Separate recovery walks, CAS retries and publication confirmation can reread records.
The spool is neither a durable cache nor progress across verification budget slices.
Browser conditional-head replay still downloads traversal/replay bodies separately (CR.86).

## Native acknowledged remote baselines

Ordinary native conditional-head synchronization separates a certified remote snapshot
from this device's pending overlay. A successful outcome may cache its pure remote
`Portable` in `sync-record-cache-v1/`, bound to the complete destination, library, durable
catalog cursor and generation. These derived entries share the existing 4,096-entry /
256 MiB cache budget and have a 64 MiB body limit. Loading checks the envelope hash,
length, binding and state schema. Missing, evicted, damaged, oversized or mismatched
entries fall back to full verified replay. There is no catalog or remote format change.

A cached entry never acknowledges pending work or establishes current authority. Its
exact position must already be durable in the catalog before use; a cache write ahead
of a failed catalog save is unreachable from the older durable cursor. Incoming state,
exact receipt acknowledgements and cursor still commit in one owner-thread journal
operation. Credential rotation does not change destination identity; retargeting does.

Each read checks current authority and the baseline cursor's ancestry, advances only by
new verified remote batches, then checks original pending IDs independently. Publication
confirmation starts from the last observed remote snapshot, captured before optimistic
pending effects are applied, so conflicts and uncertain writes cannot promote an
unconfirmed candidate into baseline proof. The exact checkpoint reference is retained with
the baseline: a changed anchor triggers full reconstruction, including checkpoint-only
legacy recovery edits, and a changed anchor at the same causal position is refused.
Expired cursors/checkpoint boundaries retain
verified full replay and prefix recovery. Explicit checkpoint maintenance and fresh remote
verification bypass the cached baseline. Corruption of old remote bytes may therefore be
detected later by fresh verification, as with other known-history reuse.

## Native B2 authority reuse

Each conditional synchronization call creates a new, runtime-only observation scope.
Protocol validation, incremental reads and publication checks share one verified native
bucket revision and its exact immutable descriptor/head/intake/manifest bodies. The
mutable bucket observation is never persisted. Every write path invalidates it on
success, conflict, cancellation or uncertainty, including validation failures; publication
confirmation and the next operation require a fresh bucket observation. A stale revision
can only lose the provider CAS, not overwrite a concurrent writer.

Immutable bodies are bound to the full destination/key/hash/size and revalidated against
the owning schema on reuse. They share the existing 4,096-entry / 256 MiB disposable cache;
a pass also retains at most 32 bodies / 8 MiB. A staged head may reuse bytes only after
its upload has completed exact readback. These byte proofs never prove publication of
the bucket pointer. Fresh verification bypasses persisted bodies and starts a new scope.
Known record lengths reserve bounded verification budget rather than the maximum intake
size. Cache loss/corruption falls back to remote reads; remote damage to known immutable
objects can be detected later by fresh verification.

Unchanged upgraded heads return promptly when legacy discovery is not due. The existing
persisted five-minute discovery deadline governs background legacy scans; manual Sync now,
explicit audits and checkpoint maintenance request discovery. An incomplete remote scan
continues on ordinary polls without waiting for another deadline or restarting its round.
A due scan still discovers
new keys behind known author frontiers and rejects forks before publishing local work.
Per-blob deletion/maintenance admission outside synchronization retains fresh authority
reads. Deleted and newer-writer controls retain their typed recovery through protocol validation.
Connection settings, credentials and remote/catalog formats are unchanged.

## Incremental sync and remote verification

Ordinary **Sync now** (`cloud.sync {wait?:false}`) reconciles incremental author tails,
new checkpoints and ready local changes. It also requests a bounded historical discovery
slice; exact known identities require no old-body download. Background historical discovery
starts at most every five minutes, with its next deadline persisted before network work;
an unfinished discovery cursor continues on subsequent polls. Upload publication does not
start an extra historical round per photo. Activity-aware polling remains **60 seconds**
while active and **15 minutes** after more than 60 seconds of inactivity.

**More → Verify remote history** (`cloud.verifyHistory {wait?:false}`) is a separate,
read-only fresh-byte check. It publishes no mutations, changes no catalog/outbox state and
can run while this device is paused. Native CLI waits by default; desktop/browser report
an Activity task and `cloud.syncStatus.reports`. A report with `verification:true` and
`complete:false` is a slice, not a completed scan; `deferred:true` means no request was
admitted because its durable allowance was spent. The UI reports completion or partial
progress once, even if the popup was closed, and discards results after a library change.
Observing these local task reports makes no provider requests. The compatibility parameter
`cloud.sync {audit:true}` performs synchronization followed by such a verification slice;
maintenance (`checkpoint:true`) must be requested separately.

Fresh verification uses a separate destination/library-bound durable schedule and append
cursor. Its automatic deadline starts one week after first use and advances one week after
a completed verification. Incomplete automatic slices continue no sooner than 15 minutes later.
A corrupt/unsupported schedule refuses network startup rather than silently losing
budget evidence. Credentials/profile rotation does not reset the destination's allowance.
Native records live in `<library>/remote-verification-v1/`; browser records live under
`cloud/remote-verification-v1/` in OPFS/IndexedDB. An OS lock/Web Lock serializes independent
owners. Both adapters durably reserve an entire slice before any verification request.

Each slice admits at most **128 HTTP requests and 128 MiB of bounded response bodies**,
including listings, nested receipt reads and B2 authority calls. Browser-managed CORS
preflight and transport overhead are excluded. Browser counts its own
signer/control requests too; a signer service's internal provider calls are outside browser
visibility. At most **four slices per persisted 24-hour reservation window** are admitted.
Failures, cancellation, restart and unused slice capacity do not refund that reservation;
a backwards wall clock cannot replenish it. Within a slice, admission reserves each
response's maximum supported size before I/O and refunds unused bytes only after success.
The append walker also targets 64 records/8 MiB between records and 16 listing pages;
an individual checkpoint can exceed that target but cannot exceed the outer byte budget.
Saved progress advances between complete records; one record and its nested proof must
fit a slice's allowance.
These are verification allowances, not a limit on necessary incremental discovery, uploads,
media viewing, or the provider account's total traffic.

Append verification saves each successful exact key before the next request and resumes
across failures/restarts independently of cached discovery. It freshly reads checkpoints,
operations and receipt archives, including receipt branch children. Listing gaps cannot
hide missing consumed checkpoints, acknowledged full-receipt operation keys, or required
archive roots. Newly introduced old keys still undergo causal/fork checks. A detected remote
history failure stays latched across ordinary cached sync; restore the remote objects and
complete fresh verification to clear it. Successful cached reads cannot clear that diagnosis.
Conditional-head verification uses the bounded read-only checkpoint/tail reader. Native
traversal/replay reuses each verified batch within that read, halving its history GETs;
the spool does not survive a request/byte-budget slice or restart. A head history too large
for one slice still cannot complete verification until checkpoint maintenance reduces its
tail. Cross-slice head verification progress remains future work. Native incremental
baseline separation is implemented; CR.86 covers browser replay. Append verification
already resumes at its saved key.

Cached/known-key discovery intentionally delays detection of corruption or disappearance
at an already accepted remote key until a fresh check. Weekly verification is not an atomic
snapshot of remote existence: each object is checked when its slice reads it. Previously
compacted operation bodies have no complete local key inventory; archive certificates and
the retained remote listing provide their integrity evidence. All newly discovered content
is verified before acceptance; no cached identity grants authority or acknowledges pending
work.

Browser append bodies use one atomic `cloud/append-metadata-v1.json` snapshot, separate
from pinned media. It binds the physical bucket root/origin, connection ID, prefix and exact
key; login tokens and signer URLs are not cache identity. At most 128 bodies of at most
1 MiB fit in an 8 MiB serialized snapshot (plus its atomic replacement). Oversized records,
cache damage or failed writes fall back to remote verification. Original content/key/schema
checks run again on reuse; fresh verification bypasses the snapshot. Auxiliary storage
admission preserves catalog headroom and pinned media. A concurrent stale cache writer can
lose reusable entries, but cannot accumulate unindexed body files. Browser conditional-head
cache/replay work remains CR.86.

Checkpoint maintenance now archives exact historical receipt proofs in bounded Merkle forests. Full current-head, tombstone and pending contexts remain live; retired author clocks and published remote objects remain retained. See [cloud-history-compaction-design.md](cloud-history-compaction-design.md) for the implemented native/browser contract and remaining capacity limits.

`cloud.syncStatus` / `cloud.overview` expose exact metadata bytes and count utilization,
with warnings at 80% and critical warnings at 95%. Cloud Synchronization shows
limit-specific recovery and an Archive receipt history action; Info and the cloud popup
show compact usage. See [capacity status and recovery](cloud-sync-protocol.md#capacity-status-and-recovery).

Enabling synchronization for an existing catalog and each local entity edit require
at most 4,000 changed fields and a 1 MiB serialized change array per entity. Large
managed albums must be split before enablement; oversized metadata must be shortened.
Setup fails atomically with the entity, usage, limits and recovery in its error.
Smaller edits can accumulate more members subject to the retained 10,000-field bound.

Unchanged conditional-head polls return `unchanged:true`, preserving catalog revision,
cached views and journal bytes when there is no earlier unsaved work. Browser owner
completion skips both the catalog and ACK-file transaction; acknowledgements and
changed metadata/cursor/generation continue through durable receive. Network discovery
uses durable consumed-checkpoint hints and bounded resumable historical audits
in append mode. LIST discovery and full state validation still have linear costs.

## Cloud status popup

The cloud icon opens a compact library card with photo/album totals and “Storage space
used”: original bytes recorded remotely (received catalog originals or locally verified
uploads), deduplicated by remote object identity. The tooltip states that previews, sidecars, synchronization
metadata and other libraries are excluded, as are locally staged originals. The last
successful remote check appears in the idle state. Uploads show active photos out of active plus
waiting photos alongside current transfer bytes, a progress bar, and changes awaiting
publication. The photo count describes started transfer/publication work, not completed
photos; merely admitting an item to a multi-photo task leaves it waiting until its worker starts.

Check-circle, sync-arrows and warning-triangle footer icons distinguish the three main
states. Pause, conflict-branch and cloud-off icons identify attention causes; navigation
rows have trailing chevrons. All icons are original vector drawings.

Problems appear together under Needs Attention, with publication-blocked changes, a
conflicting photo/field preview and Choose, and synchronization failures with Details and
an automatic-retry countdown. Choose and Details open Cloud Synchronization, where the
full cause, recovery advice, retained choices and maintenance actions remain available.
Per-import and upload retry actions remain directly accessible. The footer has one main
action: Pause, Resume, or Retry. Navigation rows close the popup when opening their
associated view or dialog. Cloud synchronization is the single popup settings action;
connection configuration remains available from File. Desktop import is offered in the idle state; browser original
uploads remain unsupported.

## Current limits and remaining work

- Enabling native cloud synchronization automatically queues every file-backed or built-in demo photo
  added to the library, and later imports, for verified staging and upload. Browsed
  Local-folder records are enrolled only after being added to the library. Adoption
  relinks existing photo and virtual-copy records after verified staging; IDs, edits,
  metadata, album memberships and photo history remain. Original files are retained.
  Demo photos have no on-disk original: background preparation materializes full-resolution
  linear Rec.2020 originals as 32-bit floating-point DNGs, retaining scene tone mapping,
  highlight headroom and separate edits. Their actual encoded size replaces the demo estimate.
  Generated adoption uses manifest version 5; the current local catalog format is 21
  (media-kind conversion first required format 20). Older builds refuse the newer catalog
  rather than discarding local rejoin or media-kind operations.
  Existing cloud objects are byte-verified before any upload; matching originals are reused.
  The queue is rediscovered on reopen, bounded to 1,000 candidate IDs per scan, and
  stages one original family at a time without a library-size limit. Missing or unsupported
  originals remain local with recovery errors; healthy photos continue. Pause stops new
  automatic staging, and Resume retries preparation failures. Explicit `cloud.adopt`
  remains available for individual recovery. One-shot CLI/MCP commands do not run the
  desktop frame pump; use explicit `cloud.adopt` followed by `cloud.upload` there.
  Native photo/album offline intent follows current members and source revisions.
- Browser managed import/upload and adoption are unsupported; ordinary file import
  remains device-local. Its verified persistent cache evicts only unpinned indexed cloud
  blobs, reserves
  catalog headroom, retains live photo/album offline intent across reload/backup, and
  distinguishes temporary session bytes from durably saved offline copies. Browser
  storage remains subject to browser eviction/site-data removal; see [quota recovery](browser-cloud.md#verified-cache-offline-selections-and-quota-recovery). The maintained [personal signer deployment and capability matrix](browser-signer.md)
  use `lightcraft-cli cloud-signer`, exact-origin CORS and private server credentials.
- Ordinary photo deletion retains complete objects, revisions, tombstones and receipt archives.
  Desktop Cloud Synchronization also offers a separate [whole-library erase](cloud-retention.md#delete-an-entire-cloud-library),
  requiring a fresh storage review, a stopped-device acknowledgement and the exact typed
  phrase `DELETE CLOUD LIBRARY`. It removes historical versions and incomplete uploads,
  retaining only deletion barriers, the bucket and local device copies.
  Native [retention tooling](cloud-retention.md) previews/apply-checks unreferenced,
  unversioned orphan media; AWS S3 apply requires a reviewed token and stopped writers.
  Custom endpoints/B2 remain preview-only. Multipart cleanup only retires incomplete
  uploads. Never expire history or supporting objects independently of this protocol.
- [Original limits](cloud-input-limits.md) are reported by `cloud.overview.inputLimits`:
  native input is 512 MiB / 128 MP; browser original decoding is 32 MP with 512 MiB
  aggregate source admission. Shared descriptors accept 128 MP; larger browser photos
  use published smart previews. The 4 GiB blob wire bound does not imply decode capacity.
- Synchronization retries connectivity, HTTP 408/425/429/500/502/503/504 and contention
  failures after 2, 4, 8, 16 and 30 seconds. A sixth failure halts polling. Authentication,
  integrity, invalid history/schema/identity, unsupported configuration (including HTTP
  301/307/400/404/413), synchronization capacity limits and local resource
  failures halt immediately with a recovery action specific to the cause (update LightCraft,
  rejoin a moved library, check bucket/prefix/region, archive receipt history). Explicit Resume/Sync now resets the
  retry budget; successful polling clears the error. Migration requires explicit recovery.
  Idle desktop windows and browser tabs wake for each scheduled poll or retry; Reconnect
  restarts a halted scheduler unless syncing is paused.
  Resource-specific errors and last successful remote scan remain separate from user pause
  and pending publication. Pending edits, staged originals and exact mutation IDs survive
  failures; the runtime retry budget resets on catalog reopen.
- Upgrade ownership is separate from conversion capability: only schema-1 identity
  rebase and append migrations are executable in this build. A readable maintenance
  envelope targeting a future schema is retained unchanged and requires an application
  update before takeover or cancellation. Activation compatibility comes from the
  converter registry. Other upgrade and cache review findings remain explicitly open
  in CR.43–CR.45 and CR.48–CR.55.
- Checkpoints archive exact old receipts while retaining current/pending contexts.
  Author clocks and remote history remain retained. State validation, snapshot cloning,
  author/checkpoint listing and some structural reconciliation still scale with state;
  the measured CR.9/CR.10 bounds are not a general large-library latency guarantee.
- Conditional-head publication requires working conditional writes; the tested B2
  endpoint rejected those headers. Explicit append mode passed dated B2 acceptance.
  Live AWS, a fresh post-change B2 run and arbitrary providers remain unverified.
  The optional managed metadata backend is deferred.

The checklist above describes this revision; use the [task checkboxes](cloud-reliability-tasks.md)
for later completion state. Verification details and provider/account caveats belong
to the dated archive, not to current feature claims.

## Starting with an empty bucket

The bucket must already exist. Connection checking only lists/reads and does not create
or write objects. An empty correctly configured storage folder is a valid result:
setup says there is no library yet and offers New Library. Enable & Synchronize creates
the remote library metadata during its first synchronization. Saving a connection alone
does not enable synchronization or move existing local photos into cloud storage.
A successful read-only check does not establish PUT or erase/version permissions.

## Adopting existing photos

File ▸ Move Selected Photos to Cloud Storage stages verified originals, supporting XMP,
editable previews and thumbnails on a worker, then atomically relinks existing records.
No import defaults or stale sidecar settings are reapplied. Edits made during preparation
remain authoritative. Selecting a live virtual copy adopts its matching live parent family;
independent duplicate records stay independent. Existing source files are never removed.

`cloud.adopt` accepts at most 1,000 selected IDs and uses the same 512 MiB input and
128 MP rendition limits as managed import. Missing, changed or unsupported originals
produce per-photo retry actions; healthy siblings can finish. Reload a changed original
before starting a new request. Retrying the same selection resumes its durable request;
`requestId` resumes without source selection, and `newRequest:true` starts another attempt.
Prepared version-4 manifests also work with `cloud.recoverImports` after restart, even
when the source file is unavailable. These device-local records never enter cloud metadata.

Undo restores the local source and Library/browse membership. It retains immutable assets
and upload intents so an already-frozen publication can finish before its withdrawal.
Redo reattaches the same asset. The old browse baseline is retained for Undo and is inert
while the photo is managed; it is excluded from portable synchronization data.

## Managed revisions and owner-thread I/O

DNG conversion, associated XMP saves and external-editor return ingestion prepare
immutable revisions on workers and retain photo identity, virtual copies and earlier
assets for undo/recovery. Conversion verifies decoded samples. Staging manifest v2
binds a revision to its target and base asset so recovery relinks rather than duplicates.
External working files and merge outputs remain durable local files outside eviction.
The supporting-XMP section below specifies recipe and foreign-property handling.

Native UI dispatch uses cached sources. Metadata, foreground exports, external edits
and control renders acquire missing originals on workers, hold leases and resume on
the owner only while the library/source binding remains current. Cancellation leaves
recoverable local state. Output guards protect managed originals, cache paths and
bound working files, including directory-link aliases.

## Current architecture

The catalog remains a local journal and snapshot, currently format 19. The
[journal version table](../crates/catalog/src/journal.rs) specifies upgrades and
older-reader rejection; local format numbers are independent of remote wire versions.
Photo IDs identify recipes and library records, asset IDs identify originals, and
blob IDs are SHA-256 digests of immutable bytes. Virtual copies share an asset record.
Changing bytes, object keys or versions, rendition membership, or supporting documents
requires a new asset identity. The complete manifest is frozen at its first catalog
commit, including manually attached assets; prepare renditions before committing.
Only device-local connection IDs can be rebound. `Asset::same_manifest` and
`Asset::rebound` define that comparison/binding for catalog edits, publication and
materialization, including originals, previews, thumbnails and XMP. Working-copy
bindings follow atomically, including undo/replay. Conflicting existing identities
are rejected atomically. Shared checkpoints exclude device-local paths, upload
intents, working-copy bindings, cache state and preferences.

`crates/catalog/src/assets.rs` describes originals and optional smart-preview/thumbnail blobs, with connection IDs, object keys and optional object versions. Device cache paths, transfer state and connection settings are excluded from the catalog. Existing SipHash duplicate-detection and cache hashes retain their semantics.

`crates/engine/src/assets.rs` separates `BlobStore` (transport), `TransferManager` (verified acquisition and availability), and `CatalogSync` (the conditional metadata-publication interface). Catalog synchronization uses the durable mutation protocols described in [cloud-sync-protocol.md](cloud-sync-protocol.md).

The resolver streams to an atomic-write temporary file, checks the declared size and SHA-256, then publishes it. Failed, cancelled, interrupted or corrupt transfers never become valid cached files. Reopening verifies cached content before use. Per-blob locks deduplicate simultaneous downloads; leases protect files during use. Errors distinguish confirmed missing objects, rejected credentials and temporary failures.

`Library.cache_dir` is independent of catalog storage and `Library.on_disk`, which continues to control filesystem original copying/moving and native catalog-directory behavior. Native hosts can use `open_library_in_with_cache` to attach a separate device cache to another catalog store. Thumbnails, smart previews, denoise products and face caches use the cache capability. Native filesystem libraries default to their library directory, with verified blobs in `assets-cache/`.

Managed acquisition precedes existing image, embedded-preview and denoise pair loaders. Original-byte exports, metadata inspection and merge readers acquire verified originals through detached readers scoped to the selected assets. Render-source factories do not initiate network requests; normal desktop render workers acquire sources before decoding. `cloud.fetch` starts a separate cancellable background download. Availability changes wake the UI and invalidate failed render requests and cached source state. Managed exports require originals even when a decoded proxy is cached. Explicit ``allowSmartPreview: true`` permits proxy export and caps the output to its cropped native size; original-file and DNG exports always require originals.

## Remote previews and thumbnails

The key helpers in `assets/keys.rs` define immutable renditions beneath the configured library prefix:

| Object | Relative key |
|---|---|
| Original | `originals/<source-sha256>` |
| Editable proxy | `previews/<source-sha256>/v1/<payload-sha256>.lcsp` |
| Display-ready thumbnail | `thumbs/<source-sha256>/v1/<recipe-hash>/<render-parameters-hash>/<edge>/<payload-sha256>.png` |

The original identity is SHA-256; existing recipe/cache hashes remain descriptor hashes, separate from byte-integrity verification. The rendering descriptor covers crop policy, dimensions, source white-balance facts, embedded lens facts and pipeline/camera-profile versions. Thumbnail payloads are oriented sRGB8 PNGs, at most 1,024 pixels per edge, one megapixel and 16 MiB encoded. A thumbnail for another recipe, rendering context, source or version is ignored. Virtual copies can therefore share an original without displaying another copy's edits. Existing `.lcsp` references and arbitrary original object keys remain readable.

Quick preview jobs try the existing local render cache, a matching remote thumbnail, the cached camera preview and a small develop render. Downloads and decoding run on preview workers. Matching thumbnails can finish grid browsing without downloading the original; edits that invalidate the thumbnail render from an available smart preview. A failed final thumbnail falls back to the normal render path. Editable proxies remain separate from display-ready thumbnails and retain the explicit export policy above.

Adding/replacing rendition references invalidates resolver/UI generations once per catalog operation batch, clears decoded sources and rejects results captured against older rendition identities. Managed variant/loupe/grid keys also follow availability changes. Local-file cache keys retain their previous behavior.

Decoded proxies are bypassed when an original becomes cached, including direct/headless renders. Source-result identities include the resolver generation, so late proxy results cannot repopulate a newer source cache. A concurrent verifier of an already cached original cannot cause substitution of a proxy: workers wait with cancellation while UI callbacks return Pending.

## Durable managed import

`cloud.import` prepares photos on workers, then commits the asset, photo and local upload intent in one journal batch on the owning session. Each import has a fallibly generated random identity, persisted in `Managed/imports/<id>.json` before staging. Original bytes, a generated editable `.lcsp`, and a recipe-matched PNG thumbnail live at `Managed/blobs/<sha256>`, separate from the disposable cache. Original staging is immutable, SHA-256 checked, atomically written and limited to 512 MiB per imported photo. Malformed or unsupported inputs return errors and preserve staged data for recovery.

The import completes locally without storage connectivity or credentials. It reports **Saved locally / upload pending**. The desktop subsequently schedules background upload; one-shot CLI/MCP callers explicitly run `cloud.upload`. Cancelling, abandoning a worker result, switching libraries or failing a catalog save preserves the manifest and original. `cloud.recoverImports` explicitly scans validated manifests for the selected connection, regenerates missing renditions from the staged original and commits unrecorded imports. Already journaled identities are skipped, preventing duplicate recovery. It never rereads arbitrary source paths from a manifest. A failed journal append keeps the in-memory pending save; repair the error and retry the save before recovery in another process.

Resolver acquisition prefers durable staged bytes and verifies them before decoding or exporting. Cache trimming and remove-local-copy cannot remove them; output guards prevent overwriting the immutable blob namespace. Invalid overlapping cache/managed directory mappings disable trimming and staging until repaired. `Managed/working` holds durable external-edit revisions and is excluded from cache maintenance. The managed revision workflow is described below.

## Durable upload and initial remote publication

Enable Cloud Synchronization for the selected connection before uploading, explicitly
choosing conditional-head or append mode. Imports may be staged while disabled; upload
returns a setup error and keeps their originals. All current publications use normal
causal mutations, including conditional-head imports. Version-4 head receipts and
version-3 append receipts retain exact publication intents/proofs. The obsolete standalone
import publisher and synthetic-author bridge are removed. Pre-release remote import-only
records and catalogs with synthetic import replicas are rejected; create a fresh catalog
and prefix for that disposable test data. Other local catalogs still upgrade normally.

Upload receipts live in `Managed/uploads/<import-id>.json`. They contain a versioned immutable import snapshot, verified object keys, multipart upload identities, acknowledged part ETags with SHA-256 and length, publication generation and a bounded retry error. Version 2 also binds the receipt to its bucket, region, library prefix and endpoint, excluding credential-profile selection. Retries refuse to send recorded multipart IDs to a different storage destination. Version-1 receipts remain readable; a legacy incomplete session needs explicit abort using its original connection settings before retry can bind it. Receipts are atomically saved before sending multipart parts and after each acknowledged part. Corrupt, oversized and future receipts are preserved and return an actionable error. Receipts and connection settings contain no credentials.

The queue keeps at most four upload groups running, shares the resolver's background transfer admission queue, and processes independent imports after a failure. Desktop reopening, a completed import, connection configuration and reconnect request one retry of queued work; failed tasks do not spin in a network retry loop. The activity stack shows bytes and cancellation. Info shows saved locally, uploading, failed with Retry upload, publication pending, and Original synchronized. File → Cloud Storage configures a connection; File → Import to Cloud Storage selects it and opens the ordinary asynchronous file picker.

Originals and required proxies/thumbnails are immutable conditional writes. Every remote key is downloaded and SHA-256/length verified before publication, including distinct keys with the same content digest. A missing object can be uploaded; authorization or integrity failure cannot be treated as absence. An accepted PUT with a lost response is reconciled by reading and verifying its bytes. ETags are only concurrency tokens.

Originals larger than 8 MiB use 8 MiB multipart parts. A persisted upload ID and acknowledged parts survive process restart. A lost part response causes retry of that same part number; a part is skipped only when its saved SHA-256 and size match the staged source. Missing/expired upload IDs restart safely; conflicting completion checks the object first. HTTP 200 containing an S3 XML Error is a failure. Known incomplete multipart sessions are retired after successful object verification or explicit `cloud.abortUploads`; cancelling a task retains its parts and staged original for resumption.

`cloud.cleanupUploads` previews recorded multipart sessions idle for at least seven days by default.
Set `olderThanDays` to 1–3650 and `apply: true` to explicitly retire them. Age is measured from the last
durable receipt write, including progress, failure or cancellation; future timestamps never qualify.
Cleanup includes receipts whose photos were permanently removed. Young sessions and ordinary cancellation
retain resumable parts. It reserves the connection against manual and automatic uploads, performs disk/network
work on a worker and supports activity/MCP cancellation. Scans are bounded to 4096 entries and 32 candidates
per call; `remaining` requests another pass. Invalid/future/misnamed receipts, redirected receipt directories
and changed destinations stop cleanup while preserving their records.

After an abort acknowledgement, LightCraft uses `ListParts` to confirm that no parts remain and retries
abort up to three times when a part write outlives the first acknowledgement. Failure, uncertain responses
and cancellation retain the receipt; a failed local atomic write can reconcile a confirmed remote 404 on retry.
Originals, renditions, working files and complete remote objects are never deleted by cleanup.
See AWS's [abort verification guidance](https://docs.aws.amazon.com/AmazonS3/latest/API/API_AbortMultipartUpload.html).

For an initiation accepted by storage whose response and unknown upload ID were lost, configure a bucket
lifecycle rule scoped to the library prefix for `AbortIncompleteMultipartUpload`, with a retention window
that accommodates offline work. LightCraft cannot identify that session from its local receipts and does
not change bucket-wide policies. Lifecycle expiration removes incomplete parts; the staged original remains,
and retry creates a new session. See AWS's [lifecycle configuration](https://docs.aws.amazon.com/AmazonS3/latest/userguide/mpu-abort-incomplete-mpu-lifecycle-config.html).

AWS documents the [conditional-write retry rules and incomplete-upload lifecycle option](https://docs.aws.amazon.com/AmazonS3/latest/userguide/conditional-writes.html). The implementation also checks the response body because [multipart completion can report an error inside HTTP 200](https://docs.aws.amazon.com/AmazonS3/latest/API/API_CompleteMultipartUpload.html).

Worker phases are acknowledged on the session owner only after the local journal append succeeds. Local save failure restores the prior displayed state and leaves queued operations for save retry. A process crash after remote publication but before local acknowledgement rereads the remote chain and acknowledges without another import publication. Library switching cancels the old queue; library, asset and connection revisions are checked before owner events can change another catalog or acknowledge work against replaced connection settings.

Original synchronized means verified media and its exact causal publication are
acknowledged. Later edits use the same synchronization protocols. Durable receipt
finalization must also finish before staging can be promoted to disposable cache.

## Transfer scheduling and recovery

All native resolver downloads share a four-transfer admission queue, bounded to 4,096 waiting blobs. Active-photo requests precede nearby-photo preparation, visible thumbnails and background downloads; requests at the same priority are FIFO. A foreground duplicate promotes the queued blob rather than starting another transfer. Cached reads and decoding consume no network slot. Running transfers finish cooperatively and are not preempted by a higher-priority request. The render worker pool also retains its own render priorities.

Waiting for a shared blob and waiting for a network slot both honor cancellation. Cancelling one duplicate waiter leaves another caller's transfer running. Download tasks report byte progress for their current object, with its key in the activity detail; this counter resets at each object in a multi-photo task. Rendering-driven acquisitions use the same activity UI and cancellation commands. Selected-photo status additionally includes thumbnail availability and the active/queued transfer list. Partial bytes remain temporary and are removed on cancellation.

Retry a failed explicit download with `cloud.fetch`. `cloud.reconnect` reloads saved connection settings, resolves external credentials on subsequent requests, retries durable offline pin intent and invalidates failed renders. The Info panel shows source quality and byte progress; its Reconnect and retry control also requests the selected original again. There is no automatic repeated network retry loop.

Use `wait: true` with `cloud.fetch` or `cloud.pin` in one-shot headless CLI/MCP sessions when the download must finish before the session exits. The command returns transfer errors directly. Background calls return an activity task ID and require a host that remains running. Desktop dispatch rejects `wait: true` before starting work or changing pins, keeping network waits off the UI thread. Pin intent remains saved after a transfer failure, including a failed synchronous request.

| Available on device | Browse and develop | Explicit preview export | Full/original export and original-file operations |
|---|---|---|---|
| Verified original | Available offline | Available | Available offline |
| Editable smart preview only | Available offline at proxy quality | Available at cropped proxy dimensions | Acquire original; return a connection/availability error when it cannot be acquired |
| Matching thumbnail only | Display matching recipe; acquire an editable source for rendering adjustments | Needs smart preview or original | Acquire original |
| No usable source | Keep photo and edits in the library; show availability/reconnect instructions | Acquire usable source | Acquire original |

Named export presets are expanded before foreground source acquisition. An explicit caller setting overrides the preset, and an ordinary export does not inherit preview permission from a previous export. Uncached managed records remain outside filesystem relinking.

## Native S3 provider

The provider reuses `lightcraft-fetch` HTTP/1.1 with RustCrypto TLS and SigV4-signed GET, HEAD, PUT, POST and DELETE operations. It supports AWS bucket endpoints, HTTPS S3-compatible endpoints, library prefixes and explicit download versions. Writes cannot target historical versions. Upload bodies use fixed-size buffers, exact declared lengths, SHA-256 signing and cancellable I/O. S3 XML replies are bounded and reject unsupported namespaces, document types, malformed/error roots and excessive nesting. HTTP is accepted only on loopback for tests. Redirects return errors rather than forwarding signed credentials.

`object_store` was evaluated: its standard AWS feature brings a non-Rust crypto backend; `aws-base` permits custom transport and signing providers. Its Apache-2.0 license is compatible with this project, and its native adapters support conditional operations; its browser cloud support requires a separate adapter. Reusing the existing RustCrypto transport keeps this implementation pure Rust. The LightCraft boundary preserves conditional publication. See the [official transport and crypto feature documentation](https://docs.rs/object_store/latest/object_store/#feature-flags).

Connections save only bucket, region, prefix, endpoint and profile references to device-local `cloud-connections.json`. Credentials are resolved for each request from an explicit AWS shared credentials profile, or AWS environment credentials followed by the default profile. Temporary session tokens work. This is a limited resolver: SSO, credential processes, instance/container metadata and automatic renewal are not implemented. Commands accept no secret keys in their parameters, and credentials never go into LightCraft's settings or catalog. The one place the app writes keys is the AWS shared credentials file, when cloud setup saves the keys typed into it (see [Cloud setup](#cloud-setup)). Damaged settings are reported and left unchanged.

A request answered with an HTTP redirect (301/307/308) fails with a hint to check the region and endpoint, since a bucket in another AWS region answers that way.

## Cloud setup

File ▸ Cloud Library… on a library that doesn't sync yet opens setup in steps (`crates/ui-egui/src/cloud_setup.rs`); File ▸ Cloud Storage… opens the same flow ending at the connection. A library that already syncs opens the compact cloud popup. The browser keeps its authorization-service form.

1. **Where:** Backblaze B2, Amazon S3, or other S3-compatible storage (Wasabi, Cloudflare R2, MinIO…).
2. **Connect:** the bucket, the provider's location and the keys. B2 asks for the bucket's endpoint as Backblaze shows it (`s3.us-west-004.backblazeb2.com`) and derives the region; AWS asks for the region; other services ask for both. More options holds the folder in the bucket (default `lightcraft`), the connection's name and an AWS profile to sign with instead of typed keys. Next first checks the connection, and the step goes on only when the check passes.
3. **Library:** start a new cloud library from this catalog, join a library the check found in the folder, or join one by its ID. The protocol follows: a found library's own; otherwise append-only on B2 and other services, conditional writes on AWS S3. A folder that holds a conditional-head library can't take a new one. Finishing saves the connection (`cloud.configure`), enables synchronization (`cloud.syncEnable`) and starts it (`cloud.sync`).

The check (`lightcraft_engine::cloud_setup::probe`, and `cloud.testConnection` for agents with profile-based settings) is read-only: one `ListObjectsV2` page of `<prefix>/sync-v2/` with a delimiter, which proves the keys, bucket, endpoint, region and list permission and names the append libraries, and a read of `<prefix>/catalog/head.json` for a conditional-head library. It writes nothing. It runs on a worker thread with a "Checking cloud storage" row in the activity stack, and its failure says what to fix (rejected keys, a bucket that wasn't found, an unreachable endpoint or a different region). A passed check holds only for the settings it checked; changing any field asks for a new one.

Typed keys sign only the check (they ride on the connection value, never serialized or printed) until setup finishes. Then they are saved to the AWS shared credentials file (`AWS_SHARED_CREDENTIALS_FILE`, else `~/.aws/credentials`) as a unique `lightcraft-key-<random id>` profile, and the connection retains that exact binding. New keys and rotations get a fresh binding independent of the connection name or a copied library path; existing name-derived profiles remain readable. Rotating one connection does not overwrite any previous or independently configured profile. LightCraft writers coordinate the read/merge/atomic replacement with a private sidecar lock; a busy save keeps the form and existing credentials for an explicit retry. Reads and merged output are bounded to 1 MiB. Other profiles and comments in the file are kept; the file is replaced atomically and, on Unix, made readable by its owner only (0600). The AWS CLI and other S3 tools can use the same profile. Editing a connection whose keys were saved this way leaves the key fields blank to keep them. No OS keychain is used: that would add platform dependencies, and the credentials file is what the resolver already reads on every platform, FreeBSD included.

Use credentials with `s3:GetObject`, `s3:PutObject`, `s3:AbortMultipartUpload` and `s3:ListMultipartUploadParts` permissions for the library objects. The last action enables abort verification; KMS-encrypted multipart uploads can additionally need `kms:Decrypt` for listing parts. Versioned downloads need `s3:GetObjectVersion`. Absence checks also need `s3:ListBucket`: without it, S3 returns 403 for missing objects, and LightCraft conservatively stops instead of treating an authorization error as permission to overwrite. See AWS's [GetObject permissions](https://docs.aws.amazon.com/AmazonS3/latest/API/API_GetObject.html), [multipart permissions](https://docs.aws.amazon.com/AmazonS3/latest/userguide/mpuoverview.html) and [ListParts permissions](https://docs.aws.amazon.com/AmazonS3/latest/API/API_ListParts.html).

## Commands

Native commands use the shared engine registry, CLI, control channel and MCP:

| Command | Parameters | Behavior |
|---|---|---|
| `cloud.configure` | `{id, bucket, region, prefix?, endpoint?, profile?}` | Save and attach a connection; no network request. |
| `cloud.attach` | `{assetId, asset, fileName, width, height, kind?, format?}` | Add and select a photo referencing already uploaded blobs. The caller supplies its source description. |
| `cloud.adopt` | `{connection, ids?, requestId?, newRequest?, wait?}` | Move existing file-backed or generated demo photos and their live virtual copies into managed storage. Durable requests resume staging; relink guards reject changed originals. Undo restores local sources while retaining frozen publication resources. |
| `cloud.import` | `{connection, paths, wait?}` | Stage originals/renditions and atomically save local photos and upload intent. Headless defaults to waiting; desktop returns a background task. |
| `cloud.importStatus` | `{}` | Return preparation activity, bounded completion reports and durable local upload intents. |
| `cloud.recoverImports` | `{connection, imports?, wait?}` | Recover validated staged manifests without duplicating recorded imports; optional import IDs limit recovery. |
| `cloud.status` | `{ids?}` | Report original/proxy/thumbnail availability and active/queued transfers. |
| `cloud.connections` | `{}` | List device-local connection settings without credentials. |
| `cloud.testConnection` | `{id, bucket, region, prefix?, endpoint?, profile?}` | Read-only check of keys, bucket, region/endpoint and list permission; returns `{libraries: [{id, protocol}], headTaken}` found under the prefix. |
| `cloud.upload` | `{imports?: [id], verify?: false, wait?}` | Upload staged imports, resume multipart receipts, verify every object and conditionally publish. `verify` explicitly rechecks synchronized imports and upgrades legacy receipts. Headless defaults to waiting; desktop returns an activity task. |
| `cloud.uploadStatus` | `{}` | Inspect durable upload states, running tasks and recent reports. |
| `cloud.abortUploads` | `{imports: [id], wait?}` | Explicitly abort known incomplete multipart sessions, retaining staged originals and complete remote objects. |
| `cloud.cleanupUploads` | `{connection, olderThanDays?: 7, apply?: false, wait?}` | Preview/retire idle recorded multipart sessions, including deleted-photo receipts. Preserve original/working bytes; defaults to background on desktop. |
| `cloud.fetch` | `{ids?, original?: true, wait?: false}` | Download on a worker; returns a task ID. Byte progress/cancellation use `activity.list` and `activity.cancel`; headless `wait` returns after completion or error. |
| `cloud.reconnect` | `{id?}` | Reload saved connections; retry failed renders and durable offline pins. |
| `cloud.cache` | `{budgetMb?, keepAllOriginals?, keepAllPreviews?}` | Inspect/set the device-local asset budget and library-wide offline copies; return usage, persistent offline owners and recovery errors. |
| `cloud.storage` | `{wait?}` | The cache's drive (label, total, free), library app data, photo cache and offline bytes, size limit and keep-all choices (Settings → Cache). |
| `cloud.clearCache` | `{wait?}` | Evict every eligible download now without changing the limit; desktop returns an activity task. |
| `cloud.cacheLocation` | `{path? or reset?, wait?}` | Inspect or change the cache folder: verified copy, reopen on the new folder, then delete the old copies. Desktop returns a cancellable activity task. |
| `cloud.pin` | `{ids? or albumId?, quality?: "original" or "preview", wait?: false}` | Persist offline intent and return a download task; headless `wait` finishes before return. Live album pins follow managed membership, including initially empty albums. |
| `cloud.unpin` | `{ids? or albumId? or owner?}` | Release those owners' pins, preserving other photos/albums that share the blob. `owner` can release a deleted photo/album's retained pin. |
| `cloud.removeLocalCopy` | `{ids? or albumId?, wait?}` | Remove eligible original cache bytes; preserve library records, remote objects and previews. Desktop returns a cancellable activity task; CLI/MCP wait by default. Inspect completion in `cloud.uploadStatus`. |

Pass a JSON parameter object after `cloud.configure` in `lightcraft-cli run --library LIBRARY`:

```json
{"id":"personal","bucket":"my-photo-library","region":"us-west-2","prefix":"libraries/personal","profile":"photo-library"}
```

Then attach an uploaded original through `cloud.attach`:

```json
{
  "assetId": "photo-asset-1",
  "asset": {
    "original": {
      "id": "<64-character lowercase SHA-256 digest>",
      "size": 123456,
      "remote": {"connection":"personal","key":"libraries/personal/objects/photo-asset-1.dng"}
    }
  },
  "fileName": "photo.dng",
  "width": 6000,
  "height": 4000,
  "kind": "raw",
  "format": "DNG"
}
```

The Info panel identifies cloud storage, reports original availability and offers Download original, original/preview offline pinning, unpinning and local-original removal. Album context menus offer offline pinning. Settings → Cache manages the cloud asset cache (below). Merely uncached managed originals do not enter filesystem Missing Photos/relink detection.

## Settings → Cache

Settings opens on Cache, laid out like Lightroom's Preferences → Cache: a page list on the left and,
on the right, the drive the cloud cache is on with a Learn More link to this section.

- **Usage bar and legend.** As in the reference, "X GB of Y GB used" counts what LightCraft keeps
  on the drive (the three entries below) against that plus the free space ("Z GB free"), so the
  bar shows how much of the room left to it LightCraft takes. The legend splits what LightCraft uses: *App & user data* (the library folder
  without the cloud cache: catalog, thumbnails, smart previews, staged imports; measured in the
  background), *Photo cache* (downloads that can be evicted), *Stored offline* (pinned and
  library-wide offline copies) and *Free*. Figures are GiB shown as "GB"; an unknown figure
  (drive query failed, measurement pending) shows "—".
- **Cache size limit.** 5 GB to 1 TB, capped at the drive size (`cloud.cache` `budgetMb`).
  Offline copies and files in use can exceed it.
- **Clear cache** (ⓘ explains it) evicts every eligible download at once (`cloud.clearCache`).
  Offline copies, files in use and staged or unsynchronized originals stay.
- **Store a copy of all Smart previews locally on "drive"** and **Store a copy of all originals**
  keep every managed smart preview / original offline (`keepAllPreviews` / `keepAllOriginals`).
  Covered blobs are derived from the catalog when the library opens and whenever its assets
  change, so photos imported or synchronized later are downloaded too, by one background
  worker ("Download Offline Copies" in Activity). A blob that fails (offline, credentials) is
  retried at the next change or reopen. Turning a choice off deletes nothing; the copies become
  ordinary cache entries under the size limit. The choice lives in `retention.json` (version 2,
  which still reads version 1 files), never in the catalog.
- **Storage location for originals: Browse…** moves the cache (`cloud.cacheLocation`): every
  cached file is copied to the chosen folder and its SHA-256 verified there, the retention
  settings follow, a device-local pointer (`cloud-cache-location.json` in the library's cache
  folder) is saved, the library reopens on the new folder and only then are the old copies
  deleted. Cancelling or a failed copy leaves the old folder in use. Folders inside the current
  cache, `Managed/` or `Originals/`, or holding another cache, are refused. When the chosen
  folder is unavailable at open (a disconnected drive) the library folder is used meanwhile with
  a warning, and the choice is kept.

Settings changes apply as they are made, so the dialog closes with **Done**; unlike Lightroom
there is no Cancel that reverts them.

## Device-local retention

Managed photos use logical names and albums: `photo.renamePreview`/`photo.rename` update the displayed
name, and `album.create` with `folder: true`, `album.move`, `album.rename` and `album.addPhotos` organize
My Photos. These operations never move immutable cloud keys or staged blobs. Mixed local/cloud renaming
still moves local files and sidecars where appropriate. Undo, redo and restart preserve both behaviors.
Physical folder moves reject managed photos and direct the user to albums and logical folders.

`assets-cache/retention.json` version 3 stores live owner/quality policy (versions 1/2 remain readable), a 20 GiB default budget, pin owners and explicitly removed originals. It is not catalog data. Zero budget retains only pinned or actively leased bytes. Original, smart-preview and thumbnail blobs share the asset budget; the existing rendered-thumbnail cache has its own budget. Pins and active operations may exceed the soft limit, and usage is measured asynchronously.

A bounded maintenance worker evicts least recently used eligible flat SHA-256 cache files. Acquisitions and eviction share per-blob gates; decoded-source clones retain an `Arc<AssetLease>` throughout their memory-cache/job lifetime. Downloads, decoding and exports therefore keep their source bytes protected. Cache bytes are verified downloads or verified copies of safely uploaded staging data.

Staging migration requires an fsynced synchronized catalog acknowledgement, a compatible exact publication receipt,
verified remote objects, no unfinished multipart sessions, and the receipt's matching storage destination.
Every other pending catalog intent, import manifest and upload receipt protects its referenced blobs,
including shared bytes destined for another bucket. Missing, malformed, redirected, future or legacy
ownership records fail closed. A worker atomically copies and verifies the cache file before removing
the staged link; active leases defer migration and offline pins protect the resulting cache copy.
Namespace guards and generation checks serialize this operation with staging and receipt writers.
`cloud.cache` reports migration errors separately as `stagingError`.

Legacy synchronized receipts remain durable until explicitly reverified with
`cloud.upload {"imports":["import-id"],"verify":true}` using their original destination.
This reads back object hashes, reconciles publication and saves the new destination proof before
migration becomes eligible. It does not republish a matching mutation. Missing staged bytes needed
to repair a missing remote object produce a recovery error rather than discarding local data.
`Managed/working/` and unsynchronized originals never become cache eviction candidates.

Photo and album pin owners are independent even when virtual copies share bytes. Preview pinning requires an editable `.lcsp` rendition and returns an actionable error if one is absent. The pin intent is atomically saved before downloading, retried when reopening or reconnecting, and retained after failed/cancelled transfers. Album pins follow current managed members, including changes to smart rules or matching metadata; local-file members are already local and are skipped. Empty albums retain owner/quality intent. Photo pins follow the current immutable source revision. Deleted or temporarily missing owners retain empty intent for undo/restoration.

Blob dependencies are derived after complete catalog operation batches, including received
changes, undo and redo. New dependencies are protected immediately; earlier dependencies
remain protected until the catalog append succeeds. Automatic new-dependency downloads begin after durability,
using one offline worker shared with library-wide copies. A failed save keeps both sets
protected; a later save finishes the transition. Reopen resolves every saved owner before
allowing eviction, including with a zero-byte budget. Policy files contain no authoritative
blob membership lists. Missing requested previews remain explicit policy with recovery in
`cloud.cache.offlineErrors`, `cloud.storage.offlineErrors` and Settings → Cache. Reconnect
retries transfers without discarding the saved choice.

Removing an original first releases decoded sources for every photo sharing it. Active workers and persistent pins still block removal. Removed originals are not automatically downloaded again for preview-allowed browsing; previews and disposable rendered thumbnails remain usable. Explicit download, metadata and original-required export can reacquire them. This operation never deletes the photo or the remote object.

`photo.delete` moves library records to Recently Deleted; restoring or undoing preserves the managed
asset reference. Emptying Recently Deleted removes local photo records and memberships while retaining
asset/upload records and staged or working data for recovery. Neither deletion command issues an object
DELETE. Published immutable image objects, catalog history and deleted-photo receipts are retained indefinitely
in this implementation; there is no automatic remote garbage collector. Explicit multipart retention
only aborts incomplete uploads and preserves these objects. Enabled libraries publish deletion tombstones through synchronization; explicit
conflict recovery retains alternate edits. Neither mechanism reclaims remote bytes.
The separate [orphan reclamation tool](cloud-retention.md) retains every published
historical reference and explicitly reviews unreferenced media; it does not expire deleted photos.

Malformed, oversized or future retention settings are preserved and reported. Policy writes and eviction are disabled until repaired; verified local bytes remain usable. Atomic-save failures preserve the previous policy. Settings and staging files, subdirectories and symbolic links are never eviction candidates.

## Verification

See the [dated acceptance archive](cloud-acceptance-history.md) for the exact releases,
fixtures, provider results and limitations. Newer reliability verification is recorded
in [cloud-reliability-tasks.md](cloud-reliability-tasks.md#verification-log). Historical
acceptance is not a claim that the current checkout or another provider was tested.

## Multi-device synchronization

The catalog retains a provider-independent bounded causal mutation model, durable outbox and protocol-specific progress. Shared engine capture covers commands, system operations, undo/redo and finalized slider groups, excluding device-local storage and preferences. Native conditional publication, incremental polling, checkpoints, deterministic field reconciliation, explicit conflict resolution and retained-tombstone recovery are described in [cloud-sync-protocol.md](cloud-sync-protocol.md).

## Browser support

[The browser adapter](browser-cloud.md) shares wire records, causal reconciliation and source
requirements with the engine. Fetch uses scoped authorized URLs with conditional writes in head mode or immutable readback in append mode,
expiration refresh and cancellation/deadlines. OPFS/IndexedDB retain verified blobs and pending
work. The owner-thread scheduler waits for actual storage completion of catalog/cursor and
acknowledgement proof before removing pending IDs. Worker/foreground readers use smart previews
only where explicitly allowed, and source-generation changes reject stale results.

## Coordinated migration workflow

File → Migrate Cloud Library… and the cloud popup More menu/Info buttons pause this session's
automatic publication. The migration (`cloud.upgradeLibrary`, alias `cloud.migrateAppend`) puts
the conditional head into maintenance, so other current devices stop publishing without manual
coordination; confirming that every other device is stopped is needed only when storage cannot
hold that barrier. Start the cancellable background migration in Activity. Cancelled preparation stays paused until explicit
Sync now; restart does not preserve this pause. Each device must complete its own cutover before
resuming the fleet. Native owner persistence changes protocol/progress together, preserves edits
made during the worker's remote scan, and rolls back on journal failure. A partially published
remote seed is retained for safe retry. See [migration operation](cloud-sync-protocol.md#setup-cutover-and-retention)
and [browser requirements](browser-cloud.md).

## Device-local cloud mode

The native cloud popup offers explicit on/off actions. Turning on merges local and cloud library state, stages added local originals, and reuses verified identical bytes. Turning off requires confirmation, blocks edits/navigation, stops local writers and downloads a verified live library snapshot to permanent `Local Originals/blobs` storage. It preserves identities, conflicts, local edits and all cloud originals. Progress includes stage, bytes/files, remaining work and destination; Retry, Cancel and safe Quit support interrupted conversions. The atomic `cloud-mode.json` record prevents restart or reconnect from silently enabling sync. Browser users can pause but need desktop for permanent local conversion. See [the transition contract](cloud-sync-protocol.md#turning-synchronization-off-on-this-device) for snapshot semantics, disk checks and hard-link filesystem requirements.

## Revision history

CR.43 partial (2026-10-19): route native desktop sync, upgrade/rejoin, discovery, retention and erasure through the B2 head adapter; browsers remain unsupported for upgraded B2 libraries. Live workflow acceptance pending.

CUX.11 (2026-10-17): implement resumable native cloud on/off transitions and blocking conversion UX; protect permanent originals from cache maintenance and output writes, preserve identities and reprojection of albums/stacks during adoption. See [dated validation and screenshots](cloud-acceptance-history.md#2026-10-17-device-local-cloud-onoff-cux11).


CUX.10 (2026-10-16): include generated demo originals in automatic cloud staging;
verify float pixels, appearance, stable identities, staged recovery and append-mode native upload.
The native acceptance uses an empty signed loopback append-mode bucket, a small generated
scene, and the ordinary frame pump: no manual adopt/upload command. The uploaded original
decodes as 32-bit DNG and the inspected popup reaches Synced and backed up. Live B2 data
was not changed by this acceptance run.
Validation: all 39 cloud-import engine regressions and all eight `cargo xtask ci` gates passed.

CUX.9 (2026-10-16): native cloud mode automatically stages existing and future added
library photos, reusing verified cloud originals. Remove the duplicate popup connection action.

CUX.8 (2026-10-16): guarded native whole-library erasure removes historical versions and
unfinished multipart uploads, retains terminal writer barriers, and blocks restored
catalog uploads before media writes. Empty existing buckets initialize on first sync.

CR.19 (2026-10-17): shared original input/geometry checks across render, conversion, merge and denoise; explicit native/browser decode budgets and quantitative status reporting.

CR.18 (2026-10-17): explicit retained-deletion notices and native orphan retention preview/apply; historical, staged and versioned content stays protected.

CR.17 (2026-10-17): browser verified-cache eviction, catalog headroom, offline intent and temporary source delivery; actual quota/reload acceptance is recorded in the reliability checklist.

| Date | Change |
|---|---|
| 2026-10-17 | CR.20: maintained personal browser signer, bounded metadata authorization, deployment recipe and capability matrix. |
| 2026-10-12 | Initial managed-asset and verified-cache foundation; completion evidence tracked per task. |
| 2026-10-12 | Deferred native UI acquisition, cached-only dispatch, source-change/cancellation recovery and managed-cache output protection. |
| 2026-10-12 | Device-local asset budgets, persistent offline photo/album pin owners, retained decoded-source leases and safe local-original removal; offline restart/export and native controls verified. |
| 2026-10-12 | Versioned remote rendition keys, recipe-matched thumbnail browsing, proxy/original refresh, stale-result rejection and corrupt/missing-thumbnail fallback; native delay/export checks verified. |
| 2026-10-12 | Bounded priority downloads, byte progress and shared cancellation, headless wait completion, reconnect/retry controls and preset-aware foreground acquisition; signed native progress/cancellation/retry acceptance passed. |
| 2026-10-12 | Durable local managed import and explicit manifest recovery; atomic photo/asset/upload-intent journal records (format 9), pending-original protection and cached-proxy replacement in direct renders. |
| 2026-10-12 | Durable upload receipts, resumable multipart transfer, SHA-256 readback verification, conditional initial publication and headless MCP byte progress/ping/cancellation; native connection/import and retry controls. |
| 2026-10-12 | Logical managed rename and album-folder organization; mixed-source undo/restart and rename-sequence overflow regression, native offline organization/export acceptance. |
| 2026-10-12 | Explicit idle multipart retention, deleted-photo receipt recovery, destination-bound receipts and abort verification; native activity cancellation/retry and durable-byte preservation. |
| 2026-10-12 | Verified staging/cache migration after durable acknowledgement, legacy receipt re-verification, cancellable local-copy removal and explicit retained-object deletion policy; native completion feedback and staging-error display. |
| 2026-10-13 | Managed DNG/XMP revisions, durable external-editor return ingestion, immutable pending revision publication, partial-rendition recovery and hard-link output protection; native actual-editor and CLI/MCP acceptance passed. |
| 2026-10-13 | Durable causal mutation synchronization, conditional head reconciliation, incremental tails/checkpoints, conflict versions and explicit tombstone recovery; shared-engine and CLI failure/rebootstrap checks. |
| 2026-10-13 | Browser Fetch authorization, verified managed worker media, durable OPFS/IndexedDB outbox acknowledgements, cloud backups and native/browser synchronization acceptance; optional metadata backend remains deferred. |
| 2026-10-14 | Coordinated migration dialog and asynchronous desktop cutover; operator-confirmed peer pause, cancellation/retry and local save rollback. |
| 2026-10-14 | Cloud queue popup, indexed sync-status filters and persisted device-local pause/resume; native multipart recovery and browser OPFS/IndexedDB controls verified. |
| 2026-10-14 | Settings → Cache: drive usage, size limit, Clear cache, library-wide offline copies and verified cache relocation. |
| 2026-10-16 | Preserve and apply supporting XMP on managed import; retain foreign properties through XMP, DNG and external-editor revisions with explicit rendered-recipe semantics. |
| 2026-10-17 | Separate current behavior/limits, normative protocol and dated acceptance; remove superseded current claims (CR.14). |
| 2026-10-17 | Live owner/quality offline policy, empty albums, current dependencies and save/restart eviction protection (CR.15). |
| 2026-10-17 | Existing-photo adoption preserves records, stages on workers and resumes durable requests (CR.16). |

## Cloud queue UX

The upper-right cloud icon opens the cloud-library popup. Its badge counts photos with pending
original/recipe work or issues; a pause indicator shows a device-local pause. The popup shows
managed photo/album counts, known original data (deduplicated shared originals), queued files/
data, active transfer byte progress, pending library mutations and errors. These are this
catalog's known totals, not a storage-provider quota or an inventory of the whole bucket.

Pause Syncing (`cloud.pause`) persists in this device's library preferences, cancels active
uploads/catalog network work and prevents manual/automatic publication until Resume Syncing
(`cloud.resume`). Pending edits, staged originals and multipart receipts remain. Transfers
finish stopping before resume is enabled; confirmed parts and the upload ID are reused.
Reconnect/configuration do not override the persisted pause. A failed pause preference write
still stops this session and reports that it could not save; a failed resume write keeps pause.
Downloads remain available for editing/export. Explicit migration is a separate coordinated
operation. Resume also releases cancelled migration preparation, like explicit Sync now.

When the library refuses publication (another device holds maintenance, the library moved to
another protocol, or it needs a newer writer), the popup headline reads "Publishing paused by the
cloud library" with the reason; changes from other devices still arrive and local changes wait.
Conflicting edits to any synchronized field are listed in Resolve cloud conflicts (and for the
selected photo in Info) with each retained value and a Keep button (`cloud.resolveConflict`);
the displayed value is marked. Recovery dialogs and Info provide bounded search and page
controls for retained fields and choices. Storage tools pages all retained deleted cloud
photos with the full count. The default status response still lists at most 200 deleted
photos for compatibility; sectioned recovery queries reach the complete retained list.

Sync Status in the filter bar offers Waiting to sync, Syncing, Synced, Sync issues and Local
only, with a removable chip. Status derives from durable original-upload intent and pending
photo recipe/metadata mutations; sync issues include failed uploads and retained field
conflicts. Local files stay Local only until explicitly imported to cloud storage. Browser
OPFS/IndexedDB supports the same popup/filter and persisted metadata pause; browser original
upload/import remains unsupported. The shared status index is invalidated by sync/source/
collection changes, rather than rebuilt for unrelated metadata edits on each frame.

## Supporting XMP in managed imports and revisions

Managed import uses the ordinary preferred/alternate sidecar naming rules,
including uppercase `.XMP`, with raw embedded XMP as the fallback when there is no
sidecar. It stages the exact packet as an immutable associated object, applies
import defaults and then the sidecar's metadata/develop values, and freezes that
interpretation before publishing the manifest. A later recovery does not reread a
changed neighboring file. Supporting documents are limited to 16 MiB; unreadable
or malformed XMP reports an actionable import error while preserving source files.

Revision policy:

- An XMP write merges fresh owned metadata/settings into the previous packet,
  preserving foreign namespaces and history.
- DNG conversion embeds that merged packet and retains it as associated XMP.
- An external-editor render carries foreign supporting data, with fresh owned
  metadata and neutral develop settings because source edits are already rendered
  into its pixels. Reading that packet cannot reapply the original recipe.
- An editor's returned original retains the derived photo's current catalog recipe
  and refreshes its owned packet; existing supporting documents remain associated.

Every carried document is staged and verified before a revision becomes ready,
including when the previous bytes must first be acquired on a new device. Earlier
immutable assets and their packets remain available to retained revisions.


## Managed import requests and partial recovery

`cloud.import` saves a device-local request before staging any candidate. Repeating
its paths and destination resumes that request, including after restart; `requestId`
can resume without source paths. The saved recipe freezes defaults, presets, label
names and sidecar naming for siblings not yet prepared. Finished manifests reuse
their identities and frozen descriptions; missing immutable copies can only be
restaged with bytes matching the saved hashes. Source paths never enter cloud data.

`newRequest:true` explicitly starts another attempt. The default `duplicatePolicy:
"skip"` reuses a managed photo in that connection with the same original SHA-256; `"allow"` permits an
intentional separate photo record. Resume a deliberately new attempt using its
returned request ID, rather than repeating `newRequest:true`. Manifests retain the
policy so unfiltered recovery cannot turn skipped content into new records.
A skipped decision retains its exact manifest under `Managed/skipped-imports`,
then retires its active staging manifest. It deletes no media bytes and cannot
weaken the existing import's proof requirements. A retry reuses that decision even
if the source or staged original is gone; a crash between saving the decision and
retiring the manifest is completed by retry/recovery. Results preserve request order.
A settled retry never selects a reused local photo ID whose original bytes changed;
it reports the previous target as unavailable and requires a new request for another
photo record.

Preparation and recovery return independent `failed` and `skipped` arrays alongside
successful `ids`. Healthy candidates share one catalog/journal commit; revision
candidates still require their recorded prior asset. Global publication-capacity
or journal failures remain atomic and leave all staged data recoverable. Malformed
manifests remain untouched and continue blocking staging maintenance.

Per-candidate failures and retry actions persist in `Managed/import-issues.json`,
appear in `cloud.importStatus` and the cloud popup, and clear only after successful
owner acknowledgement and worker status finalization. Request registration, original
preparation and status-file writes run on workers in desktop use; the owner alone
commits the catalog. Preparation through acknowledgement is serialized per library;
media uploads remain independent. Committed upload intents suppress obsolete
preparation failures on restart, while upload-integrity failures remain visible.
A library switch cancels waiting workers without applying a
result to the replacement library.


### Upload decision boundary (2026-10-16, CR.13)

`cloud_upload/transitions.rs` owns pure decisions for content readiness, phase
acceptance, one-batch publication binding, exact-proof dependency waiting and
receipt-finalization completion. Worker stages own byte transfer/verification and
receipt I/O. `VerifiedUpload` can only be produced after a fresh byte-verification
stage; durable verified keys alone cannot make a restarted attempt ready.

The owner applies accepted intent/catalog operations, persists them, then replies
to the worker. Failed persistence restores the prior intent or catalog inverse.
A removed/relinked photo can finish its retained frozen publication without being
recreated. Missing causal publication evidence remains dependency waiting.
Ordinary obsolete failures cannot revoke a synchronized acknowledgement; explicit
remote byte damage remains a distinct failure. Receipt completion requires written
proof covering the queued generation and every queued exact mutation digest.

### CUX.12 — batch progress, polling cadence, focused cloud controls (2026-10-17)

Automatic enrollment submits a bounded page of up to 1,000 photos to one preparation
worker. The popup and cloud badge include all preparing originals immediately. The
completed batch enters the upload queue together; four workers retain independent
per-transfer progress and bounded concurrency.
Virtual copies still share originals, and individual preparation/upload failures remain
recoverable. Preparation stays sequential within a worker.

Native and browser ordinary remote scans run every 60 seconds while the user is active.
After more than 60 seconds without focused input, the interval becomes 15 minutes.
New input restores the active cadence. New local work, explicit resume/retry and bounded
connectivity recovery retain their existing immediate or retry deadlines. Repaints alone
do not count as activity.

File > Cloud Library opens the popup for enabled libraries. The former all-purpose
Cloud Synchronization dialog is removed: connection editing opens setup, Choose opens
only conflict resolution, and More exposes storage tools, migration and guarded deletion.
Capacity details are collapsed in storage tools; deleted-photo recovery and orphan-media
review remain available there. Setup and re-enabling remain short, explicit workflows.

Validation: all eight workspace CI gates, 40 focused import tests, seven final popup
tests, and presented native 20-photo batches with both protocols passed. See the
[dated acceptance record](cloud-acceptance-history.md#2026-10-17-batched-enrollment-and-focused-cloud-controls-cux12).

### CR.58 — Local background media revisions (2026-10-17)

Managed background completion retains its originating typed operation. XMP save, DNG conversion, external-editor copy and reload commit with cloud sync off; explicit cloud enrollment remains blocked. The managed-media subsystem extraction remains CR.69.

### CR.59 — Original-owner conversion certification (2026-10-17)

Cloud-to-local conversion recertifies the final received state against the unchanged original owner and its retained append publication state. Repeated scans retain exact acknowledgements, recover already-published pending IDs from verified receipts, and compare semantic fingerprints independent of transport witnesses. Unpublished mutations remain pending.

### CR.62 — Stable absolute local-original destinations (2026-10-17)

Native library binding and the permanent Local Originals destination are resolved to absolute paths. Conversion persists the reviewed destination and verifies that binding again before worker startup; existing symlink/junction and blob validation remain in force.

### CR.61 — Incremental append discovery (2026-10-17)

Native and browser append discovery share stage-and-drain planning. Causally ready operations are applied and their superseded payloads released during discovery; integrity checks and the 64 MiB unresolved-dependency staging bound remain enforced.

### CR.60 — Permanent-local editing and causal rejoin (2026-10-17)

Permanent-local edits use the ordinary catalog independently of the bounded cloud outbox. A device-local projection baseline is journaled atomically with the first edit, while shared causal state and existing pending IDs remain frozen. Re-enable coalesces final differences against that context before receiving remote changes, durably prepares the new mutations and removes the baseline before enabling remote work. Cloud capacity limits can refuse rejoin with local edits retained and sync still off. Unpublished intermediate media revisions become Superseded, retain protected staging, and leave the upload queue; referencing one again reactivates it. Catalog format 21 records the baseline.

### CR.49 — Durable successor exposure (2026-10-17)

A version-2 maintenance envelope records Preparing before successor publication and durably latches Publishing through the conditional head before the first output PUT. Desktop and browser share the transition. Only Preparing can cancel back to the old authority; Publishing and v1 unknown exposure require finishing the same migration. Failures, uncertain responses, renewal and takeover never clear exposure. Activation and retirement require the latch. Existing successor objects are never erased. Healthy initial envelopes remain version 1. Typed newer-envelope and finish-upgrade advice survives head decoding and migration health. The no-CAS writersStopped path remains cooperative and is still CR.43; this is not a hard append barrier or a schema converter.

### CR.53 — Safe cache relocation cleanup (2026-10-17)

Cache relocation carries its intended destination and copied blob sizes through owner completion. Reopen must select that exact root and adopt the retention settings, offline owners and keep-all intent before old-copy cleanup is authorized. Fallback, damaged policy or changed settings restores the old selection and retains both copies. Cleanup holds the original resolver and its byte-lease gates, verifies replacement SHA-256 and size under the destination gate, and defers in-use files. A library-owned deletion fence stops cleanup before another move, reopen or process-lock release; retired source-cache maintenance and policy cannot affect a later reuse of the same folder. Without an exclusive library lock old copies are retained. Cleanup retries leases for 30 seconds and may leave extra old copies after a long lease, failed verification, shutdown or interruption; it never discards old retention settings while other files remain. No remote or catalog format changes.

### CR.63 — Library-wide cloud diagnostics (2026-10-17)

Cloud Details opens a focused library-wide diagnostic instead of the connection editor. The shared typed Diagnostic/Recovery projection is used by popup, Info and the dialog, preserving the original public failure, affected operation/resource, activity task, available HTTP status and captured retry state. Recovery provenance is explicit: authentication/configuration offers Edit connection; capacity offers Storage tools; integrity and local storage retain repair advice; newer schemas require an update without an irrelevant connection editor or ordinary retry. Popup recovery text is compact. Unknown legacy/future classifications retain their diagnostic and never guess actions from message words. Upgrade failure operation is explicit. Details retry respects device pause; repair-command errors preserve the dialog. ui.inspect exposes only cloudDiagnostic, while credential-entry dialog state remains excluded. This does not close CR.66/CR.71 primary-status/action unification or CR.70 transport provenance: native S3 still collapses some HTTP responses before health. No catalog or remote protocol changes.

### CR.64 — Explicit cloud authentication source (2026-10-17)

Cloud setup makes Saved keys, New access keys, Named AWS profile and Environment/default profile explicit authentication choices. Only the selected source supplies credentials to both probing and persistence; retained inactive key fields cannot override a named profile, nor be saved when another source was selected. Missing or partial replacement keys never fall back silently. A loaded saved-key binding retains its actual profile across a connection-name edit. Source and binding participate in the probe fingerprint, and final saving requires a successful probe of the current settings before any credential write or configuration change. The profile field appears with its selected source instead of being hidden under More options. Public connection parameters remain credentials-free. CR.54 credential-record isolation and CR.68 complete dialog lifecycle unification remain separate open tasks. No remote/catalog format changes.

### CR.65 — Complete recovery navigation (2026-10-17)

Read-only cloud.syncStatus recovery sections provide bounded cursor pages and search across all conflicting fields, retained values, deleted photos and structural conflicts. Cursors bind library identity/generation, catalog revision and normalized query, including page size and selected field. Invalid, stale or cross-query cursors and oversized inputs are refused. Value search covers full retained values beyond shortened previews. The default summary remains compatible. Desktop and browser forward the same query parameters. Conflict dialogs, Storage tools and selected-photo Info use shared search and Previous/Next controls, caching pages until the query, catalog or library changes. Browsing never resolves, restores or publishes. Full conflict metadata/preview remains CR.72. No catalog or remote protocol changes.

### CR.66 — Shared cloud actions (2026-10-17)

Info and the cloud popup derive manual commands, labels, availability and reasons from one typed engine Actions projection. User pause offers Resume on both; Retry restarts durable upload/import/catalog work via cloud.resume. Healthy manual Sync now uses cloud.sync and Pause remains available while checking. Native and browser status expose authoritative userPaused/canResume. Resume stays disabled while tasks drain. Local migration preparation has an explicit Sync now action to leave preparation without migrating; a running upgrade offers no ordinary Resume. Update, rejoin, unfinished-upgrade and new-library recovery, and unknown failure classifications, lead to diagnostic Details. Guarded erasure and record-specific retries are preserved. Precise status/backup wording and secondary Sync now remain CR.71; dialog lifecycle remains CR.68. No remote/catalog format changes.

### CR.67 — Fresh retention consent (2026-10-17)

Retention review moves from egui temporary memory into an app-owned runtime, bound to library generation and cloud identity. Closing, reopening or replacing Storage tools clears only the all-writers-paused assertion; explicit reopening also clears it. Successful cloud.resume clears consent on both native-engine and intercepted command paths. Failed Resume is not treated as success. Apply consumes consent immediately, including a refused attempt. Running task identity and audit/error results survive view dismissal, and local task reports are harvested while closed without reopening the view or adding server polls. A library change, including reopening the same cloud identity, discards old tasks, audit tokens and consent before another command or frame can apply them. ui.inspect exposes only the consent boolean, task ID and preview-presence flag, excluding audit tokens and credential dialog fields. The engine still repeats its full retention audit and validates the preview token before deletion. Dialog confirmation unification remains CR.68. No cloud/catalog format changes.

### CR.68 — Shared dialog confirmation lifecycle (2026-10-17)

Pointer, keyboard and control confirmation now use one dialog lifecycle handler. Cloud setup advances Provider, Connect and Library through either route, refuses duplicate confirmation while a probe runs, checks library-choice validity before final saving, and retains entered values and the checked authentication source on synchronous, asynchronous and final-save errors. Back and Cancel stop view-owned probes. Shared handling preserves existing import/export/album/rules/SAM behavior, nested replacement including closing a dialog, and blocking local-conversion guards. Cloud dialogs and their operator assertions are discarded on library-generation changes before another control confirmation or rendered action; global dialogs remain available. This reuses retention lifecycle observation from CR.67. Public command IDs, credentials-free control output and remote/catalog formats are unchanged.

### CR.69 — Managed media preparation and catalog plans (2026-10-17)

The native managed_media subsystem owns bounded verified reads, immutable staging, photo/default/sidecar preparation, rendition generation, working-file preparation and media catalog plans. PreparedMedia and its typed revision/operation kinds contain media identity and revision context independently of cloud enrollment. The caller supplies storage destination, resolver, defaults and fenced checkpoint/staging callbacks. The legacy durable Manifest remains a compatibility wrapper; its request/adoption proof, duplicate handling, global cloud identities and upload intents stay in the enrollment adapter. Source proofs still run before rendition preparation, and the same durable checkpoints remain in the same order. Media plans retain photo IDs, virtual copies, current edits and working/stack bindings; the enrollment adapter composes policy operations before the existing owner-thread commit. The queue, library token checks, catalog commit, acknowledgement and durable completion settlement order are preserved. No catalog/staging/remote format or public command changes. Browser capability is unchanged; this extraction covers the native durable-file implementation.

### CR.70 — Typed cloud failures through transport and scheduling (2026-10-17)

Native S3 requests and downloads retain HTTP 401/403, while credential-file I/O is a local resource failure. Head scope validation has a typed control-result API used by native and browser synchronization; the textual audit adapter remains compatible. Head decoding already checked newer-reader requirements early; this behavior is preserved. Browser publication preserves typed writer/control refusals. Append catalog adapters and actual object, unresolved-staging, record, outbox and per-entity limits retain Capacity. Browser startup and writer ownership preserve Catalog/Engine failure provenance, expose authentication/local-storage failures in health, and distinguish paused/running/full-queue/parameter guards from health failures. Guards run before ownership rotation; invalid manual requests leave retry health unchanged. Public browser command responses remain textual at their existing boundary.

### CR.71 — Precise shared cloud status and manual checks (2026-10-17)

Presentation projects device mode, local save condition, pending publication, last successful scan and age, exact diagnostic, publication restriction, local destination and shared Actions from the cloud overview JSON boundary. Popup and Info use that same model. Up to date replaces Synced and backed up and requires a successful scan with no pending work, active scan or attention condition; newly configured empty storage remains Checking until checked. Explicit device pause, retrying, read-only publication, migration preparation, erased/incomplete deletion and permanent local mode have distinct labels. Local mode shows its permanent-original destination. Info keeps the exact diagnostic and last-check information with pending changes. The popup offers a secondary Sync now using the existing shared cloud.sync {wait:false} action, disabled during an existing scan, while Pause remains the active footer action. Separate widget IDs prevent Info/popup collisions. Typed upload failures select the same durable Retry action as other cloud work; unknown storage failures select Details. Erasure results offer Review deletion instead of enabling the erased library. No independent backup-verification claim is made.

### CR.72 — Read-only retained-value inspection and exact edit comparison (2026-10-17)

cloud.syncStatus section choice returns one complete exact retained register value, writer identity and sequence, current-choice status, local photo identity and library context. Invalid, missing and stale library/revision requests fail without resolving anything. Existing bounded searchable pages remain intact. Conflict rows and selected-photo Info expose Full value and develop Preview/Compare, with separate Info/dialog widget identities and the existing explicit Keep command. Full metadata is readable and copyable. The read-only viewer renders materialized supported develop alternatives with each pane fitted to its own actual output, preserving retained crop and orientation. Its exact temporary render path chooses source resolution from the retained settings with orientation-aware crop density, without polluting current develop hashes or changing ordinary aligned Before/After behavior. Viewing changes no catalog, history, outbox, selection or Before settings. Preview context changes and library switches dismiss the view; closing or changing modes retires queued requests, textures and failure state so late results cannot repopulate it. Done returns to conflicts when opened there. Opening another library also clears transient Before settings to prevent reused-photo-ID leakage. No device names or timestamps are invented from writer identifiers. Inspection-only render results do not accept decoded metadata into the session, preventing learned raw white balance from journaling edits; ordinary views retain their normal metadata-learning path.

### CR.73 — Preserve Recently Deleted originals during local conversion (2026-10-17)

Cloud-to-local conversion includes recoverable Recently Deleted photos by default. Its read-only review reports their logical photo count and additional unique immutable bytes, deduplicating shared originals and sidecars without merging photo identities. A conversion-only projection materializes retained deleted content even for a fresh reader whose ordinary synchronization has never created the photo locally; retained album/reference relationships are projected with it. Normal receive and withdrawal behavior stays intact, including protection for a deliberately retained local-file original after cloud adoption is undone. Conversion still certifies each final outcome against the original owner, verifies every required current/retained content blob and supporting document, and commits conversion-only projection with the final owner operation. Existing file-backed Recently Deleted originals must verify too. Missing or changed originals fail conversion while cloud mode remains enabled. Permanent local copies remain independent of staging and the disposable cache; cloud data is not deleted. Supported retention does not purge published tombstone content; externally missing data is an error, not an implicit exclusion.

### CR.74 — Explain local-only cloud re-enablement as a merge (2026-10-17)

The Turn on cloud sync dialog detects actual device-local mode from cloud.overview.localOnly. In local-only mode it explains that local edits and new photos merge with changes in the same cloud library, conflicts remain available for review and permanent local originals stay on the device. Existing connection, protocol and library identity fields remain bound/read-only. The shared confirmation still enables the same library and starts synchronization. Both an untouched converted library and one with a durable local-edit baseline show the explanation; baseline presence alone does not switch an actively synchronized library into the rejoin flow. Active synchronization continues opening the existing cloud popup and first-time setup retains its ordinary introduction. No protocol or merge semantics change.

### CR.48 — Refuse unsupported append epoch namespaces (2026-10-17)

The shared append-control gate now checks the effective epoch and metadata root before any discovery or publication. This runner supports only the epoch-one append root, so a valid readable Active epoch-two envelope, or a conditional-head root presented as append control, returns a typed update-to-read refusal rather than scanning or writing its old root. Native transport and browser callers preserve Unsupported/Update recovery. Existing maintenance, retirement, deletion and reader/schema checks remain intact. Discovery progress, catalog state and exact pending mutations remain unchanged on refusal. Epoch routing and exclusive append upgrades remain CR.43.

### CR.51 — renew and fence ownership throughout conditional-head upgrades (2026-10-17)

Native and browser conditional-head upgrade runners supervise expensive source reads, archive/checkpoint verification and successor writes while renewing the same migration halfway through each ten-minute lease. Shared policy binds library, source boundary, plan, schema, epoch/root, owner, token and monotonic revision; only the registered completion is accepted after activation/retirement. Native renewal failure cancels private work without relabelling contention as user cancellation, while a separate watcher keeps cancellation responsive during blocked renewal I/O. Scoped shutdown also stops monitors on unwinding. Browser renewal races the migration future, and dropping a losing Fetch aborts its request. Rebase completion now replays and certifies one confirmed head snapshot instead of pairing frozen state with a newer concurrently published cursor. The cooperative no-CAS fallback is unchanged; exclusive append maintenance and old-epoch recovery remain open under CR.43.

### CR.50 — Retain typed publication capability on every outcome (2026-10-17)

Every certified outcome carries allowed, a typed cloud_control::Blocked refusal, or intentional receive-only capability. Native and browser fast paths retain the exact observed control decision even with an empty outbox and an unchanged head. Re-certification preserves capability and acknowledgement evidence. Successful readable blocked scans retain normalized publicationFailure separately from polling lastError and follow the existing 60-second active/15-minute inactive cadence without consuming failure retries or adding catalog operations. Cancellation completion now pairs its received state and cursor from the same confirmed read. The popup shows a short cause and a separate publication Details action, which preserves the original reason and Update/Rejoin/maintenance recovery even beside an unrelated polling error; unclassified legacy reasons grant no guessed repair action. Additive status/report fields only; no catalog or cloud wire-format change.

### CR.54 — Isolate saved credential bindings and coordinate saves (2026-10-17)

New access keys receive a fresh 256-bit managed profile binding for each setup/rotation form, independent of connection name or copied library path. Cloning a form preserves its binding through the probe and final save; restoring a form requires a new probe. Existing legacy and unique saved profiles remain readable through their exact stored binding. Missing saved bindings fail explicitly rather than falling back to a name-derived record. Final destination validation still runs before any credential write. Rotation writes a new profile, preserving old/copied/independent bindings. LightCraft credential writers serialize read/merge/atomic replacement with a private standard-library file lock; a busy save retains the form and old file for retry. Both input reads and merged output are bounded to 1 MiB; unrelated profiles/comments remain, with private file permissions on Unix. Explicit named/environment profiles retain their existing semantics. No catalog or cloud wire-format change; previously overwritten historical secrets cannot be reconstructed.

### CR.52 — Recover browser cache transactions and durable offline protections (2026-10-17)

A durable cache transaction marker spans verified blob/index publication, policy changes and deletion; interrupted commits force bounded SHA-256 reconciliation of the owned cloud/blobs namespace. Recovery adopts verified orphan bytes with actual sizes, removes missing index entries and retains LRU ages. Bounded directory discovery has matching OPFS/IndexedDB root semantics, with 64 KiB hashing chunks and explicit file/library/byte limits. Every saved library's authoritative owner-plus-quality intent is resolved against its durable catalog; legacy owners remain a fallback, and shared/empty selections survive. Missing, damaged, unknown or future protection inputs refuse reclamation; an established-policy marker prevents a missing index from resetting legacy protections to empty. Current-library catalog/intent revisions remain pending through drained and failed browser flushes, with in-flight space reserved. Cleanup and urgent unpins/Clear wait for a certified completed save; stale async owner jobs cannot replace a newer revision. Normal catalog changes recertify current membership without rehashing the whole cache. Critical catalog writes share the cache lock and precede mutation with a durable protection marker; they never reclaim against pending selections. Media admission leaves catalog headroom and may reclaim only verified unpinned files; a failed media save remains temporary rather than retrying deletion against stale state. Render workers preserve headroom and cannot evict managed copies. Backup restore holds the same lock, marks incomplete new directories and forces recovery before cleanup. No catalog or cloud wire-format change.

### CR.55 — Validate complete guided cloud library discovery (2026-10-17)

Native guided setup and cloud.testConnection now use one bounded read-only discovery scanner. It follows every continuation page, rejects repeated tokens, checks cancellation before each request and limits total requests, records and libraries. Candidate folders require the exact supported immutable manifest and canonical effective control records. Readable maintenance/newer-writer libraries remain discoverable; deleted, conflicting, malformed or unsupported namespace controls and unknown root metadata refuse discovery. A retired conditional head keeps its occupied-folder flag but offers only its validated supported append successor; a missing successor, wrong manifest or ambiguous same-ID active head plus append metadata returns an actionable error. Distinct library identities/protocols remain available. Later-page failure returns no partial list. A reachable empty prefix remains available for new-library setup, without a probe PUT or destructive permission test. Browser manual configuration is unchanged; the guided wizard is native. No catalog or cloud wire-format change.


### 2026-10-18 — CR.43 partial: native B2 revision CAS

A native pure-Rust Backblaze control primitive reads one destination/prefix-bound bucket-info entry, stages and verifies exact content-addressed Head bytes, and changes visibility only with b2_update_bucket ifRevisionIs. Its opaque observed snapshot preserves every unrelated bucket-info value and omits all other bucket settings from updates. Full destination binding detects shortened-key collisions; pointer/key/response sizes and the 10,000-byte bucket-info limit are bounded. Unusable catalog library names refuse staging and pointer publication, while missing control permissions retain explicit Edit connection recovery. Official HTTPS API/region validation, no redirects, native-only tokens, short-lived bounded auth caching and one forced refresh on HTTP 401 keep credentials outside serialized settings and browser surfaces. Lost, malformed or oversized success responses require rereading. Append immutable-write validation is unchanged; only the exact B2 head adapter reaches a private verified metadata PUT helper. One conditional authority owns a dedicated storage prefix, matching current conditional catalog and deletion ownership. This is a provider primitive only: ordinary native/browser synchronization, maintenance election, append upgrade/rejoin and late-legacy-write recovery do not use it yet. CR.43 stays unchecked; CR.44 remains conditional on a real semantic schema change, and CR.45 live upgrade acceptance remains open. No existing catalog or cloud wire format is changed.


### 2026-10-18 — CR.43 partial: native B2 head transitions and legacy intake

The native B2 head adapter uses a fixed immutable destination-bound descriptor and bucket-revision concurrency token. A descriptor without a committed pointer refuses normal reads rather than creating a new library. Initialization accepts only an explicit schema-1 append upgrade already in exposed maintenance, verifies the original manifest and supported source control, frozen checkpoint and marker chain, and refuses an active or unrelated raw S3 head. Subsequent writes validate generation, control transition, retained source binding and terminal deletion before staging a candidate. A same-generation CAS can change only legacy scan metadata. Exact content-addressed B2 catalog records use a narrow verified PUT adapter; the append-only write validator stays unchanged. Head format 5 adds an optional bounded legacyAppend intake reference, inherited across publication and epochs; ordinary heads and nonpersonal deletion barriers still emit format 4. Control format 3 is restricted to the retired original append source and its deletion barrier, while ordinary maintenance stays format 2 and mutation schema stays 1. The native legacy reader bypasses retired cooperative controls only for an active upgraded B2 head, verifies the original immutable records and causal proofs, and returns uncommitted input rather than a sync outcome or ACK. Callers explicitly restart budgeted historical audits. HTTP 429/503 control responses unwind immediate CAS retries; a bounded host cooldown and typed Retry-After delay reach the existing retry scheduler. Ordinary native/browser synchronization, maintenance election, upgrade/rejoin, discovery and erasure do not yet select this adapter. CR.43 stays open; these are tested components, not a shipped upgrade workflow.


### 2026-10-19 — CR.43 partial: native B2 authority in desktop workflows

Native desktop synchronization now selects the B2 head adapter whenever the destination's immutable descriptor exists. `read_head` prefers the verified B2 authority, and every head writer (ordinary publication, maintenance transitions, legacy admission and erasure) goes through one `write_head`: a `b2:` revision token uses bucket-revision CAS, and a raw S3 `catalog/head.json` write is refused once the descriptor exists. Content-addressed catalog records on such a destination use the verified B2 PUT adapter. The original append manifest remains a fence: head-mode readers continue only when the B2 successor names the same library and carries a legacy intake, otherwise they keep the "rejoin with Migrate Cloud Library…" recovery and retain pending work.

Each native poll offers verified late append input to the conditional authority once, before computing any ACK. Admission writes the new intake and, when state changed, a checkpoint plus marker batch, then CASes the head and rereads, including after a lost response. A requested historical audit is passed through, so a conflicting old-author identity behind a cursor fails the poll instead of being skipped.

`cloud.upgradeLibrary` without a plan resolves the successor in the worker, never with UI-thread I/O. A conditional-head library already on a B2 descriptor rebases; other head libraries keep the append-log plan; an append-log library on Backblaze B2 with a dedicated folder elects a frozen source through B2 initialization (one winner, others reread), retires the original append control to the B2 successor, then activates epoch 2 under the existing rebase lease, or rejoins without writing when that successor is already active. The owner thread switches the local binding protocol together with the outcome's ACKs, preserving device, owner, previous authors and pending mutations and requiring an existing remote; a failed save keeps the old binding. Pending local work is never seeded into the frozen checkpoint. `canUpgrade` (status, overview, File → Migrate Cloud Library…, cloud popup More → Migrate library…, Info) uses the cached destination class without credentials or network I/O. The confirmation copy is provider-neutral.

Discovery lists an upgraded library once, as its conditional head, and keeps unrelated append libraries in the same folder. The retention audit recognizes the B2 descriptor, authority heads and legacy intakes only when a B2 authority was read, requires the original manifest and every referenced record, and refuses a preview if the authority changed during the audit. Whole-library erasure writes the deletion barrier by B2 CAS, confirms it, keeps the descriptor and manifest as nonpersonal barriers, and rechecks the authority before and after deleting versions.

Browsers do not use native B2 authentication, so B2 upgrade and rejoin are desktop-only. A browser append device on an upgraded library is refused as retired with desktop guidance instead of the unavailable Migrate action, and its pending work stays in the browser. The desktop confirmation for an append library says browsers cannot sync the upgraded library. Live acceptance of these workflows is still open, and CR.43 stays unchecked.

### 2026-10-20 — CR.43 partial: empty joined libraries and live native validation

A fresh joined library may have no local `Managed` directory until its first staged file. Retention preview treats only initial root absence as empty and does not create it; existing redirected roots and malformed or disappearing nested metadata still refuse reclamation. Whole-library erasure creates the missing real directory before atomically saving the local deletion-intent marker and before changing remote authority. A file or symlink at the root refuses deletion. The completion marker remains durable across reopen and prevents synchronization from recreating an erased library.

Both live native B2 workflows now pass, including competing upgraders, preserved pending work, rejoin, late append recovery committed before acknowledgement, historical fork rejection, rebase, retention preview and complete erasure. Signed CLI regressions cover both protocols and desktop UI deletion of an empty joined library passes with inspected screenshots. Evidence and the distinction from remaining live-provider UI/request-cost acceptance are recorded in [cloud-reliability-tasks.md](cloud-reliability-tasks.md). Browser B2 integration remains unfinished, CR.43–CR.45 remain unchecked, and AWS live validation is unrun because no disposable AWS bucket/profile is available.


### 2026-10-20 — CR.43 partial: native personal-signer B2 control component

The personal signer's authenticated `/authorize/control` route now exposes only destination-bound read, frozen-source initialization, conditional head replacement and legacy-source fencing. It delegates to the validated native B2 authority, returns typed failures and reread-required success, and never serializes account tokens or unrelated bucket metadata. The server independently refuses activation before the exact source's permanent retirement. Strict runtime endpoint recognition keeps setup's bare-region convenience from misclassifying custom saved hosts as B2. Narrow presigning and a shared catalog validator support exact immutable B2 batches/checkpoints/legacy intakes while keeping mutable heads, descriptors, authority/control records and media outside the generic immutable grant. See [browser-signer.md](browser-signer.md) for route, bounds, deployment and permissions.

Focused handler/model tests, both live native B2 workflows and native positive/negative upgrade-availability checks pass; evidence and limits are in [cloud-reliability-tasks.md](cloud-reliability-tasks.md). This is a server component. The browser sync/upgrade/rejoin runner is not connected to it yet, so the desktop-only browser warning remains accurate and CR.43–CR.45 remain open.

### 2026-10-20 — CR.43 partial: browser B2 authority integration

The browser now routes upgraded B2 head reads and metadata commits through the scoped personal signer, supports append-to-head upgrade/rejoin and identity rebases, and retains the existing lease/exposure fences. The initial seed excludes local pending edits. A protocol switch saves the new binding and received catalog together, preserving current author, device owner and exact pending identities; the durable acknowledgement receipt still precedes removal from the outbox. Same-head rebases do not reset the binding. Read-only legacy intake becomes visible only after a successful head CAS and reread, and historical audits continue rejecting forks behind the discovery frontier.

Immutable head catalogs and append receipt archives use separate validated grants. Browser runtime testing caught receipt compaction using the catalog-only grant; family validation now keeps receipt archives in their original namespace. An explicit upgrade can inspect a verified descriptor without an initialized pointer and resume it; ordinary authority reads refuse that interrupted state. Old signers, bad logins, typed throttles and captured retry delays preserve their specific recovery and pending work. Upgrade availability comes from cached destination capability. The obsolete desktop-only warning is removed.

Local OPFS/IndexedDB runtime acceptance covers failure, interruption, rejoin/reload, late-input commit-before-ACK, identity rebase, competing upgraders, repeated lease renewal, takeover and historical-fork refusal. These contract fixtures exercise the browser and durable storage, while the native authority has separate transition/HTTP/live evidence. They do not establish live browser CORS, native provider request costs or current provider compatibility. The attempted fresh B2 run is blocked by its download/Class B transaction cap; the temporary CORS rule was removed. No AWS test bucket/profile exists. CR.43–CR.45 remain open; evidence is in the reliability checklist. Mutation schema remains 1; no schema converter was invented for CR.44.


### Bounded ready-upload publication (CR.85, 2026-10-21)

Automatic resumption selects at most 32 imports per task. Four media workers transfer
independently; identical blob transfers share a cancellable destination/revision-bound
gate. Once the task's ready results are collected, the per-library publication permit
serializes one synchronization pass. The owner first saves and supplies its latest
catalog outbox, rejecting changed libraries, destinations, protocols, cancellation or
failed saves. Media readiness never acknowledges edits. Existing per-import publication
proofs and durable catalog/sidecar acknowledgement remain authoritative, including
imports acknowledged by a sibling batch. Interrupted finalization remains local-only
recovery, and original staging survives cancellation and errors.

Fresh deletion/maintenance discovery still precedes each blob transfer. Immutable
content-addressed append control bodies use verified cache reuse after that fresh
listing; a new deletion or fork remains visible. Future/unrecognized control envelopes
cannot enter the cache; a bounded raw reread retains their typed Update refusal.
Explicit fresh verification bypasses
the cache. Quota suspension prevents untouched items starting; up to four in-flight
workers in a task may already have issued requests. Four upload tasks remain the
existing upper bound. Per-task byte progress aggregates the bounded current worker streams independently
from cumulative request-byte accounting; per-photo queue
counts remain catalog-derived. No remote schema or publication guarantee changes.
