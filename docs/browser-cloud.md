# Browser cloud adapter

Last updated: 2026-10-13

The browser uses Fetch and authorized object URLs. Native AWS profiles and long-lived keys
are not available to this adapter. A user-controlled, stateless authorization endpoint signs
requests for one bucket/prefix; it does not store library metadata. Metadata remains in the
direct bucket protocol described in [cloud-sync-protocol.md](cloud-sync-protocol.md).

Connection configuration contains `id`, `prefix`, `authorizeEndpoint`, and `storageOrigin`.
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
interoperability or large-library performance. The provided B2 bucket previously returned
HTTP 501 for the conditional writes required by this protocol; see
[the native compatibility evidence](cloud-storage-implementation.md). No unconditional fallback
is enabled. Bucket CORS and a user-controlled signer must be configured for a live deployment.
The optional managed metadata backend remains deferred.
