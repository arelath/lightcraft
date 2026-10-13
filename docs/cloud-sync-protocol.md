# Direct bucket synchronization

Last updated: 2026-10-13

Configure the same bucket and library prefix on each computer. In File → Cloud Synchronization,
the first computer leaves Library ID blank; subsequent computers enter the first computer's
Library ID (also shown in Info). Connection names and credentials belong to each computer.
Use separate local catalog directories. A catalog's saved device identity must not be cloned
to another independently editing device. Existing local photos remain local until imported
into managed storage. Existing catalogs open without a cloud connection.

`cloud.syncEnable {connection, library?}` enables a durable offline mutation journal.
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

Catalog format 11 persists global entity mappings, device/library identities, causal revisions,
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
headers with HTTP 501. Synchronization publication remains unavailable on that endpoint.
The independently signed loopback S3 fixture verifies supported conditional behavior; live
AWS interoperability has not been verified.
