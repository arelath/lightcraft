# Browser cloud adapter

Last updated: 2026-10-14

The browser uses Fetch and authorized object URLs. Native AWS profiles and long-lived keys
are not available to this adapter. A user-controlled, stateless authorization endpoint signs
requests for one bucket/prefix; it does not store library metadata. Metadata remains in the
direct bucket protocol described in [cloud-sync-protocol.md](cloud-sync-protocol.md).

Connection configuration contains `id`, `prefix`, `authorizeEndpoint`, `storageOrigin`, and optional `bucketRoot`.
It is device-local. Login tokens and returned signed URLs stay in memory. The authorization
endpoint and storage origin require HTTPS; loopback HTTP is supported for development.
Do not put an application key or secret key in browser configuration or catalog data.

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

Browser transfers are bounded to 512 MiB per blob. Larger originals require the desktop app;
an available smart preview can still support browser editing. A fetched blob is usable only
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
