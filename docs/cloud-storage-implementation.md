# Cloud storage implementation

Last updated: 2026-10-12. Completion and acceptance checks are tracked in [../tasks.md](../tasks.md). The proposal is [cloud-storage.md](cloud-storage.md).

## Current architecture

The catalog remains a local journal and snapshot. Format 8 adds `Source::Managed { asset_id }`, a shared asset table and `Op::SetAsset`. Photo IDs identify recipes and library records, asset IDs identify originals, and blob IDs are SHA-256 digests of immutable bytes. Virtual copies share an asset record. Changing original bytes requires a new asset identity. Versions 1–7 still load and upgrade; older builds refuse format 8.

`crates/catalog/src/assets.rs` describes originals and optional smart-preview/thumbnail blobs, with connection IDs, object keys and optional object versions. Device cache paths, transfer state and connection settings are excluded from the catalog. Existing SipHash duplicate-detection and cache hashes retain their semantics.

`crates/engine/src/assets.rs` separates `BlobStore` (transport), `TransferManager` (verified acquisition and availability), and `CatalogSync` (a future conditional metadata-publication interface). **Catalog synchronization is not implemented yet.**

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

The provider reuses `lightcraft-fetch` HTTP/1.1 with RustCrypto TLS and SigV4-signed GETs. It supports AWS bucket endpoints, HTTPS S3-compatible endpoints, library prefixes and explicit object versions. HTTP is accepted only on loopback for tests. Redirects return errors rather than forwarding signed credentials.

`object_store` was evaluated: its standard AWS feature brings a non-Rust crypto backend; `aws-base` permits custom transport and signing providers. Its Apache-2.0 license is compatible with this project, and its native adapters support conditional operations; its browser cloud support requires a separate adapter. Reusing the existing RustCrypto transport keeps this initial read implementation pure Rust. The LightCraft boundary includes expected-ETag head publication so a broader adapter can preserve conditional writes. Conditional catalog publication and multipart uploads remain future work. See the [official transport and crypto feature documentation](https://docs.rs/object_store/latest/object_store/#feature-flags).

Connections save only bucket, region, prefix, endpoint and profile references to device-local `cloud-connections.json`. Credentials are resolved for each request from an explicit AWS shared credentials profile, or AWS environment credentials followed by the default profile. Temporary session tokens work. This is a limited resolver: SSO, credential processes, instance/container metadata and automatic renewal are not implemented. The app accepts no secret keys in connection parameters and never stores credentials in its settings or catalog. Damaged settings are reported and left unchanged.

## Commands

Native commands use the shared engine registry, CLI, control channel and MCP:

| Command | Parameters | Behavior |
|---|---|---|
| `cloud.configure` | `{id, bucket, region, prefix?, endpoint?, profile?}` | Save and attach a connection; no network request. |
| `cloud.attach` | `{assetId, asset, fileName, width, height, kind?, format?}` | Add and select a photo referencing already uploaded blobs. The caller supplies its source description. |
| `cloud.status` | `{ids?}` | Report original/proxy/thumbnail availability and active/queued transfers. |
| `cloud.fetch` | `{ids?, original?: true, wait?: false}` | Download on a worker; returns a task ID. Byte progress/cancellation use `activity.list` and `activity.cancel`; headless `wait` returns after completion or error. |
| `cloud.reconnect` | `{id?}` | Reload saved connections; retry failed renders and durable offline pins. |
| `cloud.cache` | `{budgetMb?}` | Inspect/set the device-local asset budget; return usage, persistent offline owners and recovery errors. |
| `cloud.pin` | `{ids? or albumId?, quality?: "original" or "preview", wait?: false}` | Persist offline intent and return a download task; headless `wait` finishes before return. Album pins retain current managed members; repeat after membership changes. |
| `cloud.unpin` | `{ids? or albumId? or owner?}` | Release those owners' pins, preserving other photos/albums that share the blob. `owner` can release a deleted photo/album's retained pin. |
| `cloud.removeLocalCopy` | `{ids? or albumId?}` | Remove eligible original cache bytes; preserve library records, remote objects and previews. Report each removed or refused blob. |

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

The Info panel identifies cloud storage, reports original availability and offers Download original, original/preview offline pinning, unpinning and local-original removal. Album context menus offer offline pinning. Settings → Performance has a separate cloud asset budget. Merely uncached managed originals do not enter filesystem Missing Photos/relink detection.

## Device-local retention

`assets-cache/retention.json` stores a versioned policy, a 20 GiB default budget, pin owners and explicitly removed originals. It is not catalog data. Zero budget retains only pinned or actively leased bytes. Original, smart-preview and thumbnail blobs share the asset budget; the existing rendered-thumbnail cache has its own budget. Pins and active operations may exceed the soft limit, and usage is measured asynchronously.

A bounded maintenance worker evicts least recently used eligible flat SHA-256 cache files. Acquisitions and eviction share per-blob gates; decoded-source clones retain an `Arc<AssetLease>` throughout their memory-cache/job lifetime. Downloads, decoding and exports therefore keep their source bytes protected. Every cache blob is a verified download of an already uploaded object. Future import staging and unsynchronized external working copies must use a separate durable directory and cannot enter this disposable cache.

Photo and album pin owners are independent even when virtual copies share bytes. Preview pinning requires an editable `.lcsp` rendition and returns an actionable error if one is absent. The pin intent is atomically saved before downloading, retried when reopening or reconnecting, and retained after failed/cancelled transfers. Album pins snapshot their current cloud members; local-file members are already local and are skipped. Repeating a pin updates that owner's snapshot.

Removing an original first releases decoded sources for every photo sharing it. Active workers and persistent pins still block removal. Removed originals are not automatically downloaded again for preview-allowed browsing; previews and disposable rendered thumbnails remain usable. Explicit download, metadata and original-required export can reacquire them. This operation never deletes the photo or the remote object.

Malformed, oversized or future retention settings are preserved and reported. Policy writes and eviction are disabled until repaired; verified local bytes remain usable. Atomic-save failures preserve the previous policy. Settings and staging files, subdirectories and symbolic links are never eviction candidates.

## Verification and remaining work

Catalog tests cover malformed references, shared originals/virtual copies, journal replay and compaction, format-7 upgrade and legacy file sources. A synthetic regression rejects `PhotoId(u64::MAX)` as an error instead of overflowing.

Engine tests cover a cloud-only synthetic Bayer DNG opening, developing, exporting and reopening from verified cache; concurrent requests; lease protection; corrupt/interrupted/cancelled/auth-failed transfers; injected failed writes; and path traversal rejection. A proxy-only render cannot satisfy a managed export's original requirement. A UI regression verifies retry after download completion without a catalog edit.

`apps/lightcraft-cli/tests/cloud.rs` drives the real CLI and MCP binaries against a loopback S3-shaped server with generated RAW bytes and fake credentials. This test passed on Windows with Rust 1.95.0. Set `LIGHTCRAFT_CLOUD_ACCEPTANCE_DIR` during the test to save its generated DNG for a separate desktop acceptance run.

The native desktop acceptance also passed on 2026-10-12 using control port 18837 and a separate loopback server that independently checked SigV4 with Python's HMAC implementation. It attached the remote-only RAW, applied +0.75 exposure, reached a completed native render, saved its catalog and exported a 32×24 PNG. One verified request acquired the original; the export reused it. The inspected native screenshot and results are in gitignored `target/cloud-acceptance/`. The final inspected frame measured 0.82 ms for UI update and 6.29 ms for the last render, with no queued or in-flight renders. These measurements use a tiny synthetic fixture and are not a large-library performance benchmark. This does not establish live AWS interoperability. Per-task completion and actual validation results are recorded in `tasks.md`.

Unchecked work includes durable import/upload and external-working-copy protection, synchronization/conflicts, browser adapters and the optional backend. Browser workers currently return an unsupported error for managed sources. The presence of managed records or `CatalogSync` does not establish multi-device synchronization.

`cargo +1.95.0 xtask ci` passed fmt, strict Clippy, HEIF checks, workspace tests, parity references, layering, asset attribution and WASM on Windows. The required Rust 1.95 toolchain exposed older Clippy suggestions and Unix-specific test assumptions, which were corrected. Recovery tests also exposed duplicate Windows `.xmp`/`.XMP` alias moves: sidecar operations now deduplicate physical aliases, preserve actual filename spelling and retain distinct packets on sensitive volumes. Synthetic and native regressions cover these cases.

Native UI dispatch is cached-only: it cannot start a transfer or wait for another download's lock. Metadata, external-edit and foreground export/control-render requests acquire remote content on a worker, then resume on the owner thread with leases held. Pending actions retain their initial target IDs and are rejected after a library/source change. Control clients receive their ordinary result after acquisition; GUI commands expose an activity task, including cancellation. Normal view rendering, merge, denoise and background exports already acquire on workers. Private original-reader handles include a NUL separator so valid local filenames cannot collide with them.

User-output guards protect managed originals and renditions, and reject writes anywhere inside the managed cache, including prospective filenames and paths through existing directory links. Internal verified-cache writers retain their own publication path.

A second native acceptance used a 2.5-second server delay. Concurrent metadata and `ui.render` requests completed after one independently verified download while another connection's `ui.inspect` answered in 26 ms. It returned 31 metadata rows, rendered 32×24, exported at original dimensions and rejected a render output pointed at the original cache file; its SHA-256 remained intact. The inspected screenshot is `target/cloud-acceptance/native-deferred.png`; results are `native-deferred-results.json`. Final UI update time was 0.73 ms with empty render queues. These are small-fixture responsiveness checks.

Native retention acceptance passed on 2026-10-12 through control port 18837. Clicking Info → Keep original offline persisted the pin; Settings → Performance displayed the separate budget and its pinned-only option. After the local S3-compatible server stopped, reopening retained the pin and a zero-byte budget, and an adjusted RAW exported at 32×24. Pinned removal was refused. Unpinning and explicit removal deleted the cached original while preserving the photo, its remote reference and an empty Missing Photos result; browsing did not recreate the removed original. Screenshots (`native-cache-info.png`, `native-cache-settings.png`, `native-cache-removed.png`) and results are in gitignored `target/cloud-acceptance/`. Final native update time was 7.79 ms with no queued/in-flight renders; this tiny fixture is not a large-library benchmark. Engine tests additionally verify editable preview pins across restart, separate album/photo ownership, corruption recovery and atomic-save failures. CLI/MCP retention acceptance passed too.

Native preview acceptance passed on 2026-10-12 with a 2.5-second proxy delay: a verified thumbnail appeared first, then an editable `.lcsp` render. Browsing fetched one thumbnail and one proxy and never requested the original. The maximum observed control-inspection response during the transfer was 56 ms; the final native update was 0.40 ms with empty render queues. +0.5 exposure rendered from the proxy. Full export reported an HTTP 503 while the original was unavailable, explicit proxy export succeeded at 32×24, and enabling/downloading the original refreshed the view and allowed full export. The decoded proxy/full export channels differed by at most one 8-bit level on this procedural fixture; `.lcsp` is a reduced-quality editing proxy. Screenshots and request/result logs are in gitignored `target/cloud-acceptance/native-preview-*` and `preview-server-requests.json`. This is not a large-library performance or real-camera fidelity benchmark.

Native transfer acceptance passed on 2026-10-12 through port 18847 against a signed loopback server that sent an original in delayed chunks. The Info panel displayed partial byte progress and cached-preview availability. Cancelling the activity left the original uncached and editing from the proxy succeeded. Clicking Reconnect and retry acquired the original, restored cached availability and allowed 32×24 full export. All HTTP requests passed independent SigV4 verification. Screenshots and results are `target/cloud-acceptance/native-transfer-*`; maximum observed inspection latency was 36 ms and the final native UI update was 1.18 ms with empty render queues. These measurements use a tiny procedural fixture, not a large-library benchmark. Focused tests also cover the four-download limit, priority ordering and duplicate promotion, cancellation while waiting for a shared blob or network slot, preview use during an original transfer, render-driven activity cancellation/reconnect, and one-shot CLI wait completion. Live AWS remains unverified.

## Revision history

| Date | Change |
|---|---|
| 2026-10-12 | Initial managed-asset and verified-cache foundation; completion evidence tracked per task. |
| 2026-10-12 | Deferred native UI acquisition, cached-only dispatch, source-change/cancellation recovery and managed-cache output protection. |
| 2026-10-12 | Device-local asset budgets, persistent offline photo/album pin owners, retained decoded-source leases and safe local-original removal; offline restart/export and native controls verified. |
| 2026-10-12 | Versioned remote rendition keys, recipe-matched thumbnail browsing, proxy/original refresh, stale-result rejection and corrupt/missing-thumbnail fallback; native delay/export checks verified. |
| 2026-10-12 | Bounded priority downloads, byte progress and shared cancellation, headless wait completion, reconnect/retry controls and preset-aware foreground acquisition; signed native progress/cancellation/retry acceptance passed. |
