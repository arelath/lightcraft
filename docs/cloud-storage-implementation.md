# Cloud storage implementation

Last updated: 2026-10-14. Completion and acceptance checks are tracked in [../tasks.md](../tasks.md). The proposal is [cloud-storage.md](cloud-storage.md).

Checkpoint maintenance now archives exact historical receipt proofs in bounded Merkle forests. Full current-head, tombstone and pending contexts remain live; retired author clocks and remote objects remain retained. See [cloud-history-compaction-design.md](cloud-history-compaction-design.md) for the implemented native/browser contract and remaining capacity limits.

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
and append checkpoint-audit optimization are separate follow-up tasks.

## Current architecture

The catalog remains a local journal and snapshot. Format 8 adds `Source::Managed { asset_id }`, a shared asset table and `Op::SetAsset`; format 9 adds device-local upload intents journaled with imports. Photo IDs identify recipes and library records, asset IDs identify originals, and blob IDs are SHA-256 digests of immutable bytes. Virtual copies share an asset record. Changing original bytes requires a new asset identity. Versions 1–12 still load and upgrade; older builds refuse format 13. Format 10 adds associated XMP, stable identity mappings and device-local working-copy bindings. Format 11 adds durable mutation synchronization; format 12 adds explicit append/CAS protocol selection, verified append progress and restored-author ownership. Format 13 adds sync-status filter criteria; absent criteria still mean no status constraint. Shared checkpoints exclude upload intents and all device-local paths and settings.

`crates/catalog/src/assets.rs` describes originals and optional smart-preview/thumbnail blobs, with connection IDs, object keys and optional object versions. Device cache paths, transfer state and connection settings are excluded from the catalog. Existing SipHash duplicate-detection and cache hashes retain their semantics.

`crates/engine/src/assets.rs` separates `BlobStore` (transport), `TransferManager` (verified acquisition and availability), and `CatalogSync` (a future conditional metadata-publication interface). Catalog synchronization uses the durable mutation protocols described in [cloud-sync-protocol.md](cloud-sync-protocol.md).

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

Resolver acquisition prefers durable staged bytes and verifies them before decoding or exporting. Cache trimming and remove-local-copy cannot remove them; output guards prevent overwriting the immutable blob namespace. Invalid overlapping cache/managed directory mappings disable trimming and staging until repaired. `Managed/working` is reserved for durable external-edit revisions and excluded from cache maintenance; the external-editor ingestion workflow remains open.

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

Historical initial import publication used a bounded version-1 protocol (superseded by
the causal protocol in [cloud-sync-protocol.md](cloud-sync-protocol.md)):

| Object | Role |
|---|---|
| `<prefix>/originals/<sha256>` | Immutable original bytes |
| Versioned preview/thumbnail keys from `assets::keys` | Immutable supporting renditions |
| `<prefix>/catalog/changes/<sha256>.json` | Immutable import batch with its parent batch reference |
| `<prefix>/catalog/head.json` | Small head containing protocol version, generation and content-addressed tip |

The publisher reads the head and its ETag, reconciles an already published import identity, creates a batch with `If-None-Match: *`, and creates or advances the head with `If-None-Match: *` or `If-Match`. A failed condition reloads and rebases against the new head; there is no unconditional head write. A lost head response is resolved by finding the same mutation and exact immutable snapshot in the verified chain. Unsupported/malformed heads and mismatched identities stop publication without replacing remote data. Batch size is capped at 2 MiB, publication retries at 16, and history traversal at 4,096 batches. Checkpoints and history compaction remain future work.

Worker phases are acknowledged on the session owner only after the local journal append succeeds. Local save failure restores the prior displayed state and leaves queued operations for save retry. A process crash after remote publication but before local acknowledgement rereads the remote chain and acknowledges without another import publication. Library switching cancels the old queue; library, asset and connection revisions are checked before owner events can change another catalog or acknowledge work against replaced connection settings.

Original synchronized means the original, supporting objects and initial import reference are safely published. Later edits remain in the local catalog; multi-device edit/album synchronization is not implemented. Staged bytes remain protected even after synchronization in this slice. Moving safely uploaded staging into the disposable cache, associated XMP and external-editor ingestion remain open tasks.

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

Connections save only bucket, region, prefix, endpoint and profile references to device-local `cloud-connections.json`. Credentials are resolved for each request from an explicit AWS shared credentials profile, or AWS environment credentials followed by the default profile. Temporary session tokens work. This is a limited resolver: SSO, credential processes, instance/container metadata and automatic renewal are not implemented. The app accepts no secret keys in connection parameters and never stores credentials in its settings or catalog. Damaged settings are reported and left unchanged.

Use credentials with `s3:GetObject`, `s3:PutObject`, `s3:AbortMultipartUpload` and `s3:ListMultipartUploadParts` permissions for the library objects. The last action enables abort verification; KMS-encrypted multipart uploads can additionally need `kms:Decrypt` for listing parts. Versioned downloads need `s3:GetObjectVersion`. Absence checks also need `s3:ListBucket`: without it, S3 returns 403 for missing objects, and LightCraft conservatively stops instead of treating an authorization error as permission to overwrite. See AWS's [GetObject permissions](https://docs.aws.amazon.com/AmazonS3/latest/API/API_GetObject.html), [multipart permissions](https://docs.aws.amazon.com/AmazonS3/latest/userguide/mpuoverview.html) and [ListParts permissions](https://docs.aws.amazon.com/AmazonS3/latest/API/API_ListParts.html).

## Commands

Native commands use the shared engine registry, CLI, control channel and MCP:

| Command | Parameters | Behavior |
|---|---|---|
| `cloud.configure` | `{id, bucket, region, prefix?, endpoint?, profile?}` | Save and attach a connection; no network request. |
| `cloud.attach` | `{assetId, asset, fileName, width, height, kind?, format?}` | Add and select a photo referencing already uploaded blobs. The caller supplies its source description. |
| `cloud.import` | `{connection, paths, wait?}` | Stage originals/renditions and atomically save local photos and upload intent. Headless defaults to waiting; desktop returns a background task. |
| `cloud.importStatus` | `{}` | Return preparation activity, bounded completion reports and durable local upload intents. |
| `cloud.recoverImports` | `{connection, imports?, wait?}` | Recover validated staged manifests without duplicating recorded imports; optional import IDs limit recovery. |
| `cloud.status` | `{ids?}` | Report original/proxy/thumbnail availability and active/queued transfers. |
| `cloud.connections` | `{}` | List device-local connection settings without credentials. |
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
| `cloud.pin` | `{ids? or albumId?, quality?: "original" or "preview", wait?: false}` | Persist offline intent and return a download task; headless `wait` finishes before return. Album pins retain current managed members; repeat after membership changes. |
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

`assets-cache/retention.json` stores a versioned policy, a 20 GiB default budget, pin owners and explicitly removed originals. It is not catalog data. Zero budget retains only pinned or actively leased bytes. Original, smart-preview and thumbnail blobs share the asset budget; the existing rendered-thumbnail cache has its own budget. Pins and active operations may exceed the soft limit, and usage is measured asynchronously.

A bounded maintenance worker evicts least recently used eligible flat SHA-256 cache files. Acquisitions and eviction share per-blob gates; decoded-source clones retain an `Arc<AssetLease>` throughout their memory-cache/job lifetime. Downloads, decoding and exports therefore keep their source bytes protected. Cache bytes are verified downloads or verified copies of safely uploaded staging data.

Staging migration requires an fsynced synchronized catalog acknowledgement, a matching version-2 receipt,
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

Photo and album pin owners are independent even when virtual copies share bytes. Preview pinning requires an editable `.lcsp` rendition and returns an actionable error if one is absent. The pin intent is atomically saved before downloading, retried when reopening or reconnecting, and retained after failed/cancelled transfers. Album pins snapshot their current cloud members; local-file members are already local and are skipped. Repeating a pin updates that owner's snapshot.

Removing an original first releases decoded sources for every photo sharing it. Active workers and persistent pins still block removal. Removed originals are not automatically downloaded again for preview-allowed browsing; previews and disposable rendered thumbnails remain usable. Explicit download, metadata and original-required export can reacquire them. This operation never deletes the photo or the remote object.

`photo.delete` moves library records to Recently Deleted; restoring or undoing preserves the managed
asset reference. Emptying Recently Deleted removes local photo records and memberships while retaining
asset/upload records and staged or working data for recovery. Neither deletion command issues an object
DELETE. Immutable image objects, catalog history and deleted-photo receipts are retained indefinitely
in this implementation; there is no automatic remote garbage collector. Explicit multipart retention
only aborts incomplete uploads and preserves these objects. Multi-device tombstones and conflict
recovery are separate synchronization work and are not yet provided by local library deletion.

Malformed, oversized or future retention settings are preserved and reported. Policy writes and eviction are disabled until repaired; verified local bytes remain usable. Atomic-save failures preserve the previous policy. Settings and staging files, subdirectories and symbolic links are never eviction candidates.

## Verification and remaining work

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

The native desktop acceptance also passed on 2026-10-12 using control port 18837 and a separate loopback server that independently checked SigV4 with Python's HMAC implementation. It attached the remote-only RAW, applied +0.75 exposure, reached a completed native render, saved its catalog and exported a 32×24 PNG. One verified request acquired the original; the export reused it. The inspected native screenshot and results are in gitignored `target/cloud-acceptance/`. The final inspected frame measured 0.82 ms for UI update and 6.29 ms for the last render, with no queued or in-flight renders. These measurements use a tiny synthetic fixture and are not a large-library performance benchmark. This does not establish live AWS interoperability. Per-task completion and actual validation results are recorded in `tasks.md`.

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

## Staging migration and deletion acceptance

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

## Managed revision acceptance

Managed revision acceptance (2026-10-13): all eight CI stages passed in `target/cloud-managed-revisions-ci-final.log`. Fifteen focused preparation/recovery tests and real CLI/MCP revision tests cover partial rendition recovery, source changes, protected hard-link aliases and immutable upload descriptions. Native control-channel checks converted a procedural RAW to DNG, saved/read associated XMP, launched an actual test editor, ingested its returned 16×12 TIFF under the same derived photo ID, exported at full dimensions and removed eligible cache data while retaining the working file. All 54 fixture requests passed independent signing checks. Screenshots `native-revisions-dng.png` and `native-revisions-external.png` were inspected; results are in `target/cloud-acceptance/native-revisions-results.json`. Maximum inspection latency was 59 ms on this tiny fixture.

DNG encoding verifies decoded sample identity before publication. XMP is a bounded associated immutable blob; explicit reads and writes use managed storage, while automatic neighboring-file sidecar writes remain local-file-only. External working copies are durable device data, never disposable cache. Recovery refuses changed source bindings and keeps staged originals. Full-resolution merges acquire originals on workers and produce durable local outputs. These checks do not establish synchronization of subsequent edits between devices.

## Multi-device synchronization

Catalog format 11 adds the provider-independent, bounded causal mutation model, durable outbox and cursor. Shared engine capture covers commands, system operations, undo/redo and finalized slider groups, excluding device-local storage and preferences. Native conditional publication, incremental polling, checkpoints, deterministic field reconciliation, explicit conflict resolution and retained-tombstone recovery are described in [cloud-sync-protocol.md](cloud-sync-protocol.md).

Verified core evidence is in `target/cloud-sync-engine-tests.log`, `target/cloud-sync-owner-tests.log`, `target/cloud-sync-cli-tests.log` and `target/cloud-sync-ci-final.log`. Eleven shared-engine scenarios, two owner/persistence tests and four real CLI scenarios complement five catalog reconciliation tests. They cover independent offline imports, metadata/membership convergence, competing recipes, deletion recovery, revision-aware undo, bounded migration, compaction, atomic failure/retry, conditional races/response loss, incremental downloads and checkpoint rebootstrap with pending work. Browser acceptance and its authorization/storage contract are described in [browser-cloud.md](browser-cloud.md).

Native synchronization acceptance (2026-10-13) passed through control port 18877 with a signed loopback S3 fixture. Two catalogs converged after independent metadata/develop edits; the Info conflict controls opened Versions and explicitly resolved a recipe while preserving its alternative. Permanent deletion followed by an offline peer edit did not recreate the photo; clicking Recover restored the retained five-star edit, then both catalogs synchronized. Cancellation retained pending work, rejected credentials produced actionable Info text, and clicking Sync now completed retry. Setup, progress, failure, conflict, versions and recovery screenshots were inspected. All 140 scenario requests passed independent signing checks; every write was conditional. Results are in `target/cloud-acceptance/native-sync-results.json`. The isolated delayed-head check recorded 61 inspection samples with a maximum of 52 ms; the complete feedback flow recorded a 493 ms outlier around screenshot/control interaction. Final UI update time was 8.87 ms with empty render queues. These tiny procedural checks do not establish large-library performance or live AWS compatibility. All eight CI stages passed in `target/cloud-sync-ci-final.log`.

## Browser support

[The browser adapter](browser-cloud.md) shares wire records, causal reconciliation and source
requirements with the engine. Fetch uses scoped authorized URLs with conditional writes,
expiration refresh and cancellation/deadlines. OPFS/IndexedDB retain verified blobs and pending
work. The owner-thread scheduler waits for actual storage completion of catalog/cursor and
acknowledgement proof before removing pending IDs. Worker/foreground readers use smart previews
only where explicitly allowed, and source-generation changes reject stale results.

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

## Coordinated migration workflow

File → Migrate Cloud Library… and the Cloud Synchronization/Info buttons pause this session's
automatic publication. Confirm that every other device is stopped and upgraded, then start the
cancellable background migration in Activity. Cancelled preparation stays paused until explicit
Sync now; restart does not preserve this pause. Each device must complete its own cutover before
resuming the fleet. Native owner persistence changes protocol/progress together, preserves edits
made during the worker's remote scan, and rolls back on journal failure. A partially published
remote seed is retained for safe retry. See [migration operation](cloud-sync-protocol.md#setup-cutover-and-retention)
and [browser requirements](browser-cloud.md).

## Revision history

| Date | Change |
|---|---|
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

Sync Status in the filter bar offers Waiting to sync, Syncing, Synced, Sync issues and Local
only, with a removable chip. Status derives from durable original-upload intent and pending
photo recipe/metadata mutations; sync issues include failed uploads and retained field
conflicts. Local files stay Local only until explicitly imported to cloud storage. Browser
OPFS/IndexedDB supports the same popup/filter and persisted metadata pause; browser original
upload/import remains unsupported. The shared status index is invalidated by sync/source/
collection changes, rather than rebuilt for unrelated metadata edits on each frame.

Verification (2026-10-14): native six-file queue/multipart pause-resume and browser OPFS/IndexedDB
popup/filter/reload/native-peer workflows passed against the signed loopback fixture, with
screenshots inspected (`target/cloud-ux-acceptance`). Native release and browser builds passed
(`target/cloud-ux-native-final.log`, `target/cloud-ux-web-final.log`); all eight full CI stages
passed (`target/cloud-ux-ci.log`). This UX acceptance does not add a fresh live-B2 or large-library
performance claim.
