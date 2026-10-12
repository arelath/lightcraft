# Cloud storage implementation

Last updated: 2026-10-12. Completion and acceptance checks are tracked in [../tasks.md](../tasks.md). The proposal is [cloud-storage.md](cloud-storage.md).

## Current architecture

The catalog remains a local journal and snapshot. Format 8 adds `Source::Managed { asset_id }`, a shared asset table and `Op::SetAsset`. Photo IDs identify recipes and library records, asset IDs identify originals, and blob IDs are SHA-256 digests of immutable bytes. Virtual copies share an asset record. Changing original bytes requires a new asset identity. Versions 1–7 still load and upgrade; older builds refuse format 8.

`crates/catalog/src/assets.rs` describes originals and optional smart-preview/thumbnail blobs, with connection IDs, object keys and optional object versions. Device cache paths, transfer state and connection settings are excluded from the catalog. Existing SipHash duplicate-detection and cache hashes retain their semantics.

`crates/engine/src/assets.rs` separates `BlobStore` (transport), `TransferManager` (verified acquisition and availability), and `CatalogSync` (a future conditional metadata-publication interface). **Catalog synchronization is not implemented yet.**

The resolver streams to an atomic-write temporary file, checks the declared size and SHA-256, then publishes it. Failed, cancelled, interrupted or corrupt transfers never become valid cached files. Reopening verifies cached content before use. Per-blob locks deduplicate simultaneous downloads; leases protect files during use. Errors distinguish confirmed missing objects, rejected credentials and temporary failures.

`Library.cache_dir` is independent of catalog storage and `Library.on_disk`, which continues to control filesystem original copying/moving and native catalog-directory behavior. Native hosts can use `open_library_in_with_cache` to attach a separate device cache to another catalog store. Thumbnails, smart previews, denoise products and face caches use the cache capability. Native filesystem libraries default to their library directory, with verified blobs in `assets-cache/`.

Managed acquisition precedes existing image, embedded-preview and denoise pair loaders. Original-byte exports, metadata inspection and merge readers acquire verified originals through detached readers scoped to the selected assets. Render-source factories do not initiate network requests; normal desktop render workers acquire sources before decoding. `cloud.fetch` starts a separate cancellable background download. Availability changes wake the UI and invalidate failed render requests and cached source state. Managed exports require originals even when a decoded proxy is cached. Explicit ``allowSmartPreview: true`` permits proxy export and caps the output to its cropped native size; original-file and DNG exports always require originals.

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
| `cloud.status` | `{ids?}` | Report remote, downloading, cached, missing or failed availability. |
| `cloud.fetch` | `{ids?, original?: true}` | Download on a worker; returns a task ID. Progress/cancellation use `activity.list` and `activity.cancel`. |
| `cloud.cache` | `{budgetMb?}` | Inspect/set the device-local asset budget; return usage, persistent offline owners and recovery errors. |
| `cloud.pin` | `{ids? or albumId?, quality?: "original" or "preview"}` | Persist offline intent before starting downloads. Album pins retain current managed members; repeat after membership changes. |
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

Unchecked work includes remote thumbnail discovery and transfer priorities, safe import/uploads, synchronization/conflicts, browser adapters and the optional backend. Browser workers currently return an unsupported error for managed sources. The presence of managed records or `CatalogSync` does not establish multi-device synchronization.

`cargo +1.95.0 xtask ci` passed fmt, strict Clippy, HEIF checks, workspace tests, parity references, layering, asset attribution and WASM on Windows. The required Rust 1.95 toolchain exposed older Clippy suggestions and Unix-specific test assumptions, which were corrected. Recovery tests also exposed duplicate Windows `.xmp`/`.XMP` alias moves: sidecar operations now deduplicate physical aliases, preserve actual filename spelling and retain distinct packets on sensitive volumes. Synthetic and native regressions cover these cases.

Native UI dispatch is cached-only: it cannot start a transfer or wait for another download's lock. Metadata, external-edit and foreground export/control-render requests acquire remote content on a worker, then resume on the owner thread with leases held. Pending actions retain their initial target IDs and are rejected after a library/source change. Control clients receive their ordinary result after acquisition; GUI commands expose an activity task, including cancellation. Normal view rendering, merge, denoise and background exports already acquire on workers. Private original-reader handles include a NUL separator so valid local filenames cannot collide with them.

User-output guards protect managed originals and renditions, and reject writes anywhere inside the managed cache, including prospective filenames and paths through existing directory links. Internal verified-cache writers retain their own publication path.

A second native acceptance used a 2.5-second server delay. Concurrent metadata and `ui.render` requests completed after one independently verified download while another connection's `ui.inspect` answered in 26 ms. It returned 31 metadata rows, rendered 32×24, exported at original dimensions and rejected a render output pointed at the original cache file; its SHA-256 remained intact. The inspected screenshot is `target/cloud-acceptance/native-deferred.png`; results are `native-deferred-results.json`. Final UI update time was 0.73 ms with empty render queues. These are small-fixture responsiveness checks.

Native retention acceptance passed on 2026-10-12 through control port 18837. Clicking Info → Keep original offline persisted the pin; Settings → Performance displayed the separate budget and its pinned-only option. After the local S3-compatible server stopped, reopening retained the pin and a zero-byte budget, and an adjusted RAW exported at 32×24. Pinned removal was refused. Unpinning and explicit removal deleted the cached original while preserving the photo, its remote reference and an empty Missing Photos result; browsing did not recreate the removed original. Screenshots (`native-cache-info.png`, `native-cache-settings.png`, `native-cache-removed.png`) and results are in gitignored `target/cloud-acceptance/`. Final native update time was 7.79 ms with no queued/in-flight renders; this tiny fixture is not a large-library benchmark. Engine tests additionally verify editable preview pins across restart, separate album/photo ownership, corruption recovery and atomic-save failures. CLI/MCP retention acceptance passed too.

## Revision history

| Date | Change |
|---|---|
| 2026-10-12 | Initial managed-asset and verified-cache foundation; completion evidence tracked per task. |
| 2026-10-12 | Deferred native UI acquisition, cached-only dispatch, source-change/cancellation recovery and managed-cache output protection. |
| 2026-10-12 | Device-local asset budgets, persistent offline photo/album pin owners, retained decoded-source leases and safe local-original removal; offline restart/export and native controls verified. |
