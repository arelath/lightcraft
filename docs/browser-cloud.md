# Browser cloud adapter

Last updated: 2026-10-17

This document specifies the current browser adapter. The shared wire contract is
[cloud-sync-protocol.md](cloud-sync-protocol.md); operational gaps are indexed in
[current limits](cloud-storage-implementation.md#current-limits-and-remaining-work).
Verification is dated separately in [cloud-acceptance-history.md](cloud-acceptance-history.md).

The browser uses Fetch and authorized object URLs. Native AWS profiles and long-lived keys
are not available to this adapter. A user-controlled, stateless authorization endpoint signs
requests for one bucket/prefix; it does not store library metadata. Metadata remains in the
direct bucket protocol described in [cloud-sync-protocol.md](cloud-sync-protocol.md).

Connection configuration contains `id`, `prefix`, `authorizeEndpoint`, `storageOrigin`, and optional `bucketRoot`.
It is device-local. Login tokens and returned signed URLs stay in memory. The authorization
endpoint and storage origin require HTTPS; loopback HTTP is supported for development.
Do not put an application key or secret key in browser configuration or catalog data.

Use the maintained [personal signer deployment recipe and capability matrix](browser-signer.md)
for `lightcraft-cli cloud-signer`, private server credentials, exact-origin CORS and token rotation.

## Authorization endpoint contract

POST the following JSON to the configured endpoint, optionally with an ephemeral
`Authorization: Bearer <login-token>` header:

```json
{
  "connection": "personal",
  "method": "PUT",
  "key": "library/catalog/head.json",
  "version": null,
  "condition": "\"observed-etag\"",
  "size": 256,
  "contentSha256": "<lowercase SHA-256>"
}
```

The endpoint must authenticate the user, enforce its configured bucket/prefix, and authorize
the exact method/key/version/body hash/precondition. `condition: "*"` means `If-None-Match: *`;
any other supplied condition means `If-Match`. It must never downgrade a requested precondition.
Reply with JSON:

```json
{
  "url": "https://bucket.example/library/catalog/head.json?<signature>",
  "expiresAt": 1791850000000,
  "headers": {"If-Match": "\"observed-etag\""}
}
```

`expiresAt` is a UTC Unix timestamp in milliseconds. Use a short lifetime, at most 24 hours.
The URL origin must match `storageOrigin`; redirects are rejected. Only content type,
conditional-write, and S3 content/checksum headers are accepted in `headers`. Browser Fetch
omits cookies for both requests. The signer must allow the exact browser origin through CORS
and expose no credentials in error responses. A 401/403 during an object request triggers
one fresh authorization request with the same body and condition; a second refusal requires
reauthorization. Retrying an uncertain write still requires protocol readback/reconciliation.

Temporary credentials can expire before a signed URL's advertised lifetime. See
[AWS signed URL lifetime rules](https://docs.aws.amazon.com/AmazonS3/latest/userguide/using-presigned-url.html).

## Bucket CORS

Use the exact deployed browser origin, including scheme and port. For the S3 console, a
minimal JSON rule for this adapter is:

```json
[
  {
    "AllowedOrigins": ["https://lightcraft.example"],
    "AllowedMethods": ["GET", "HEAD", "PUT"],
    "AllowedHeaders": [
      "content-type", "if-match", "if-none-match",
      "x-amz-content-sha256", "x-amz-checksum-sha256"
    ],
    "ExposeHeaders": ["ETag", "Content-Length"],
    "MaxAgeSeconds": 300
  }
]
```

S3 handles preflight OPTIONS; OPTIONS is not an `AllowedMethods` entry. All requested headers
must match allowed headers, and scripts need exposed ETags for conditional head updates.
See [AWS CORS elements](https://docs.aws.amazon.com/AmazonS3/latest/userguide/ManageCorsUsing.html)
and [preflight behavior](https://docs.aws.amazon.com/AmazonS3/latest/developerguide/RESTOPTIONSobject.html).
The authorization endpoint separately allows POST, Content-Type and Authorization from the
browser origin. CORS does not grant bucket access: the signer/bucket policies enforce it.

Browser transfers are bounded to 512 MiB per blob and original decoding to 32 MP.
Shared photo descriptions accept 128 MP; a published smart preview supports browser
editing of larger photos, even with a cached original. Native input/decode remains
bounded to 512 MiB / 128 MP. See [input limits](cloud-input-limits.md); streamed cloud
blob bounds are separate from decoder admission. A fetched blob is usable only
after length/SHA-256 verification. Quota or storage failures retain local pending work and
report an error; they do not authorize eviction of unsynchronized data.

## Using the browser adapter

Open File → Cloud Storage and enter a connection name, the storage origin, authorization
endpoint and library prefix. Supply a session login token if your signer requires it. Open
Cloud Synchronization and enter the same library ID used by your native device. Leave that
ID blank only to create a new shared library. The Info panel reports pending work, errors,
conflicts and Sync now; conflict versions and explicit deleted-photo recovery share the native
commands. Connection settings survive reload; the session login token does not.

The JavaScript bridge exposes the same commands, for example:

```js
await lightcraft.command("cloud.configureBrowser", JSON.stringify({
  id: "personal", prefix: "library",
  storageOrigin: "https://bucket.example",
  authorizeEndpoint: "https://signer.example/authorize",
  token: "<session login token>"
}));
await lightcraft.command("cloud.syncEnable", JSON.stringify({connection: "personal", library: "<shared library id>"}));
await lightcraft.command("cloud.sync", "{}");
```

Transfer/synchronization commands return background task IDs. `cloud.syncStatus.reports`
provides their completion results; `activity.cancel` cancels them. Fetch and streamed bodies
have a two-minute deadline and respond to cancellation. Never put a long-lived storage key
in the token field. Managed import/upload and native external editors remain desktop operations;
the browser edits and exports photos already published in the shared library. The browser's
ordinary file import still stores local photos.

## Storage and export policy

OPFS is preferred; IndexedDB is the fallback or can be selected with `?store=idb`. Memory-only
sessions cannot configure cloud synchronization. Downloads are verified before persistent cache
publication, and memory eviction never removes durable media or pending records. Browser
storage limits and the browser's own eviction policy still apply; keep independent backups.
File → Back Up Library includes verified cloud blobs, connection configuration and the durable
outbox, plus the usual local media. Login tokens and signed URLs are excluded.

Catalog writes first enter a synchronous mirror, then flush asynchronously. Received changes
and their cursor are saved together. A device-local acknowledgement receipt must also finish
saving before pending IDs are removed; reload can finish the receipt without losing later
edits. A quota error leaves dirty data and pending intent available for retry. Do not reload
while the app reports unsaved changes. No unsynchronized data is evicted to recover space.

An available smart preview supports offline editing and explicit rendered preview export
(`allowSmartPreview:true`). Default full-quality and original-file exports require verified
original bytes; they never silently substitute a preview. Arrival of an original changes the
source identity and invalidates worker/main-thread results without modifying the catalog recipe.
Exports larger than the transfer/memory budget require smaller batches or the desktop app.

## Verification and limits

See [current limits](cloud-storage-implementation.md#current-limits-and-remaining-work)
and [dated acceptance](cloud-acceptance-history.md) for distinct implementation and
verification scope. A live deployment requires bucket CORS, a signer and session authorization; a
maintained [personal signer recipe](browser-signer.md) covers configuration, HTTPS proxy,
server permissions and the native/browser scope. The optional metadata backend is deferred.

## Append mode and signer extension

Configure `bucketRoot` as a canonical URL ending in `/`, on `storageOrigin`. For path-style B2,
for example, it is `https://s3.<region>.backblazeb2.com/<bucket>/`. The adapter pins this root and
prefix, rejects retargeting an enabled catalog, and verifies exact object paths and version IDs.
Library backups exclude the root-level installation identity; restored library directories
claim a new current author while keeping historical pending IDs. Browser Web Locks serialize
writers in the same browser storage.

Select Append-only (Backblaze B2) in Cloud Synchronization, or pass `protocol:"append-log-v1"`
to `cloud.syncEnable`. The default remains conditional S3. For migration, configure an
append-capable signer/root and use File → Migrate Cloud Library… or the setup/Info migration
button. The shared dialog pauses this session's automatic publication and requires confirmation
that every other device is stopped and upgraded. Cancel keeps it paused; reconfiguring the
signer or reconnecting does not resume it. Explicit Sync now resumes cancelled preparation.
The pause does not survive app restart; keep other devices closed until cutover is ready.
`cloud.prepareMigration` and `cloud.migrateAppend {writersStopped:true}` expose the same control
flow. Observe Activity and the background report before retrying `cloud.sync`. Cancellation
after remote seed publication preserves local mode/pending work; retry migration to finish.
Migration saves the remotely verified seed and local mode durably without acknowledging
unsent pending work. Each catalog must migrate; the app cannot stop old binaries remotely.

Existing requests retain the original JSON contract. Append requests add:

```json
{
  "connection":"personal", "method":"GET", "key":"", "operation":"list",
  "list":{
    "prefix":"library/sync-v2/<library-id>/ops/",
    "delimiter":true, "continuation":null, "startAfter":null
  }
}
```

The signer authorizes only this exact scoped `ListObjectsV2` request. `list-type=2` and
`max-keys=1000` are fixed. Optional delimiter, continuation and start-after parameters must
match exactly; bucket root, query names and duplicate parameters are checked. Root discovery
uses `/`; tails and checkpoints use ordinary listings. Each page is bounded to 4 MiB/1,000
entries. Keys outside its prefix, repeated tokens, unsupported namespaces and XML DTDs fail.

An immutable PUT includes `operation:"putImmutable"`, exact key, `size` and `contentSha256`.
Receipt maintenance also reads/writes `sync-v2/<library-id>/receipts/<sha256>.json`:
canonical, at-most-1-KiB Merkle nodes bound to that library. The signer must permit these
scoped immutable objects with the same exact content-hash binding as checkpoints.
Conditional-head maintenance uses conditional PUT plus readback for these objects;
append maintenance uses `putImmutable`. Retain receipt archives and upgrade devices
before maintenance. See [cloud-history-compaction-design.md](cloud-history-compaction-design.md).
The signer must reject mutable heads and arbitrary keys. Bind the exact SHA-256 using a signed
content hash or checksum header; the adapter verifies that binding. B2 presigning uses
`UNSIGNED-PAYLOAD` in the canonical request with a signed `x-amz-checksum-sha256` header
containing the Base64 SHA-256. Do not use the ordinary header-auth payload hash as the canonical
query-auth payload hash. The bucket CORS rule must allow the checksum header, including on PUT
preflight. [Backblaze CORS](https://www.backblaze.com/docs/cloud-storage-cross-origin-resource-sharing-rules)
explains the supported bucket APIs. The adapter never adds conditional-write headers in append
mode. It resolves lost responses by fresh GET/readback of the exact bytes and hash key.

Authorization and object requests disable the browser HTTP cache, so a reused signed GET URL
cannot retain a cached 404 after publication or a stale listing/head. Verified media still uses
the explicit OPFS/IndexedDB cache. Existing expiry refresh, cancellation and quotas apply.
Received changes/progress and an exact-ID acknowledgement receipt must actually finish saving
before pending IDs drop. Reload can finish a saved acknowledgement, including historical authors.

## Cloud status popup and pause

Synchronization reports typed failures and bounded transient retries through
`cloud.syncStatus.health` / `cloud.overview.health.sync`. Connectivity, signer unavailability,
HTTP 408/425/429/500/502/503/504 and contention retry after 2/4/8/16/30 seconds; the sixth
failure halts automatic polling. Authentication, corrupt or invalid history, identity/schema
errors and local resource failures halt immediately. Fetch cannot distinguish offline from
TLS/CORS rejection; its bounded connectivity diagnostic names those checks. The popup shows
retry timing, exhaustion and recovery actions. Reauthorize the same destination and Resume
after authentication repair; pending edits and exact IDs remain in browser storage. Successful
polling clears the error; unrelated media/publication success does not. Migration failures
require explicit cutover recovery. See [failure policy](cloud-sync-protocol.md#cloud-subsystem-health).

The upper-right cloud icon now opens a shared status popup with managed-data totals, pending
photo/library work, activity progress, errors and Pause/Resume Syncing. The Sync Status picker
filters Waiting to sync, Syncing, Synced, Sync issues or Local only. Status counts describe this
catalog, not the provider's storage capacity. `cloud.overview` exposes the same summary.

`cloud.pause` saves a device-local preference and cancels catalog network jobs, preserving
pending work. Reload/reconnect/sign-in changes keep the pause. `cloud.resume` waits for jobs to
stop, saves the resumed preference, then schedules synchronization after durable saves finish.
Acquisition/downloads remain available. Native original uploads can be paused/resumed from the
same popup; browser original upload/import is still unsupported. Dated popup/filter/reload acceptance is preserved in the [archive](cloud-acceptance-history.md).

## Browser source acquisition lifetime

Exports and renderer misses share one source-byte queue with at most four active
acquisitions. A deferred export reserves its complete distinct source set before
starting and pins those verified bytes through command completion. Unrelated
rendering cannot evict an earlier export original while a later original loads.
Overlapping commands share downloads and release their own lease independently.
Cancellation or failure releases the command's pins; an aborted transfer retains
its reservation until its future settles. A batch that cannot fit reports a retry
or smaller-group error before starting.

The 512 MiB admission limit counts resident source payloads and distinct reserved
or in-flight payloads together. It does not measure total browser memory: decoded
images, workers and transport/storage copies incur additional costs. Status exposes
`reservedBytes`, `budgetBytes`, and `pinned` alongside resident bytes and queue counts.
Preview selection uses a cache-only original lookup; choosing a smart preview does
not incidentally schedule an original download. Full-original export still requires
verified original bytes.


Append polling uses the shared CR.10 consumed-checkpoint and resumable historical-audit policy
specified in [cloud-sync-protocol.md](cloud-sync-protocol.md#cr10-append-discovery-and-historical-audit-budgets).
Ordinary polls discover new checkpoint keys and author tails; historical integrity work runs in
bounded slices and its device-local cursor is saved in OPFS/IndexedDB with append progress.
`cloud.syncStatus.appendAudit` distinguishes a partial audit from a completed round.

## Verified cache, offline selections and quota recovery

Settings → Cache exposes a 128 MiB default cache budget (0–4096 MiB), Clear cache,
keep-all originals/previews, quota usage and offline errors. Photo and album offline
selections retain owner and quality, including empty albums; membership and immutable
source revisions are resolved again after catalog changes. `cloud.pin`, `cloud.unpin`,
`cloud.clearCache`, and `cloud.cache {"budgetMb":64}` use this policy in the browser.
Per-library `browser-offline.json` travels in library backups. Global derived protection
also retains dependencies of closed libraries. Offline readiness requires saved intent,
a saved catalog and verified persistent bytes; a successful temporary export is insufficient.

Only SHA-256-verified, indexed `cloud/blobs/` entries are eviction candidates, in least
recently used order. Imported originals, library snapshots/logs/outbox/acknowledgements,
unknown files and every library's pinned dependencies are excluded. Pins can exceed the
soft cache budget. Clear cache and a zero budget preserve them. Explicit unpinning can
release space while unrelated catalog saves are blocked; other retained owners keep
both old and current dependencies until their catalog save completes.

Media admission leaves at least 64 MiB plus known snapshot/journal work for catalog saves
and accounts for replacement copies. Origin-wide Web Locks serialize policy changes,
eviction and writes across tabs and renderer workers. Storage estimates are hints;
actual quota errors trigger bounded eligible-cache reclamation and one retry. Catalog
write failure retains the dirty queue and pending edits. Retry follows normal save
recovery after space is freed. No estimate can guarantee a future write will succeed.
See [StorageManager estimates](https://developer.mozilla.org/en-US/docs/Web/API/StorageManager/estimate)
and [Web Locks](https://www.w3.org/TR/web-locks/).

A verified download that cannot be cached remains available for session viewing/export
through the shared byte admission queue. Renderer workers receive verified bytes on a
source cache miss, so temporary viewing does not depend on a successful disk write.
The UI distinguishes session-only bytes from saved offline selections, reports failed
pin downloads, and offers retry. Browser persistence permission can be denied, and
clearing site data or browser eviction can still remove storage: keep independent backups.
Missing coordination or malformed policy disables cache mutation and reports repair
instead of guessing which stored data is disposable. Browser original upload/adoption
remains unsupported.

## Revision history

| Date | Change |
| --- | --- |
| 2026-10-17 | CR.20: maintained personal signer deployment, scoped checksummed writes, token rotation and native/browser capability matrix. |
| 2026-10-17 | CR.17: verified cache eviction, catalog headroom, live offline intent, temporary source delivery and quota recovery. |

| 2026-10-17 | CR.19: explicit original decode budgets, shared descriptor limits and oversized-photo proxy selection after original caching. |
