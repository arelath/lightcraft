# Causal-history maintenance design

Status: proposed protocol work, 2026-10-14. This document does not describe an
implemented maintenance command. Current checkpoints retain receipts and clocks;
they do not restore capacity at the 100,000-record or 64 MiB state bounds.
See [cloud-reliability-tasks.md](cloud-reliability-tasks.md), CR.4–CR.7.

Maintainer clarification, 2026-10-14: there are no existing users or libraries, only
disposable test data. A pre-release format boundary can reject those old catalogs
and remote records; legacy-library migration and support for writable old builds
are no longer acceptance requirements. The retirement/offline recovery requirements
below apply to clients created under the replacement format. This clarification
does not authorize discarding future unsynchronized edits.

## Compatibility boundary

Do not rewrite sequence numbers, pool synthetic authors into hash buckets, drop
tombstones, or increase the device limit. Existing receipts bind exact mutation
IDs, digests, contexts and register alternatives. A migration must replace that
representation explicitly, with all writers stopped/upgraded, as the existing
conditional-head-to-append migration does.

Use a new protocol/schema and epoch namespace. A new immutable epoch manifest
binds the old library identity, previous manifest/checkpoint hashes, final covered
author prefixes, effective seed state, schema, and retirement policy. Keep photo,
asset and album identities; keep device-local credentials and bindings separate.
Retain the old namespace and blobs. Compaction and remote reclamation are separate
operations; a successful compact never deletes the historical recovery evidence.

Old writers must be fenced before cutover. Current append readers fetch newly
discovered checkpoints before publication: a hash-verified incompatible checkpoint
sentinel in the old namespace can make those supported readers fail closed. A
conditional-head writer already refuses an append manifest. A proposed implementation
must test the exact supported old native/browser builds; unrecognized older builds
require revoking/restricting their write credentials. Listing is not an atomic fence.
The coordinated stopped-writer assertion remains required, including workers that
started before the sentinel appeared. New clients must verify the sentinel and epoch
manifest rather than infer cutover from filenames or an incomplete listing.

## Compact representation

Code discovery confirms that historical contexts are needed only while a receipt is
a register head or pending intent. A first implementation may demote other receipts
to fixed-size proofs of the canonical mutation and context, promoting a matching full
receipt during checkpoint merge before causal pruning. Every current field head,
deletion head and pending ID must retain its full context. Replay and acknowledgement
lookups must accept either representation and compare exact digests. Two forms with
different context hashes are a collision, even if their mutation digest strings match.

Inline proof packing is not the complete capacity solution: those proof bytes still
grow with operation count. To recover at the final byte bound, immutable receipt
archives need authenticated lookup/prefix proofs, with current state retaining their
bounded roots rather than every historical digest. Native/browser workers must
verify archive proofs before owner-thread merge or acknowledgement. Ordinary polls
must not download whole archives. The checkpoint/frontier and audit changes are part
of this implementation, not a later reason to discard collision checks.

Retirement is separate from context packing. Retain every actual author clock by
default; a clock cannot be removed merely because its full receipts were packed or
archived. Epoch retirement requires the coordinated policy below. This keeps an
offline device's sequence allocation and causal observations meaningful.

1. Freeze a remotely verified checkpoint covering exact contiguous author prefixes.
   Pending local mutations cannot seed it. Retain every causally maximal value,
   deletion/restore choice, immutable content descriptor and named conflict version.
2. Anchor those values to the epoch seed. Preserve their historical origin references
   for inspection, but do not carry a per-photo legacy author in the new live clock.
   A new-epoch edit observes the seed and supersedes its applicable heads explicitly.
3. Represent actual legacy imports as seed rows keyed by their immutable import
   identity, with preserved baseline-versus-authored precedence. Keep old immutable
   records as external exact collision/recovery evidence. Do not synthesize one
   active replica for each import or revision.
4. Keep exact receipts for pending/new-epoch work and current field heads. Covered
   records are summarized by authenticated contiguous coverage and historical object
   commitments; older records are verified against retained immutable history when
   an audit or collision check needs them. Bloom filters cannot be authoritative:
   a false positive must never discard a real edit.
5. Live vector clocks contain only admitted authors in this epoch. Retirement is an
   explicit, previewed operation with a covered prefix and cutover identity, not a
   timeout inferred from the last poll. Repeated epochs bound historical replica width.
6. Tombstones remain effective seed rows. Retirement/compaction cannot resurrect a
   deleted photo, strip retained content/edits, or make an explicit restore implicit.
   Tombstone expiry needs a separately approved retention contract and fencing proof.
7. User-visible edit history and versions are not disposable receipt history. If those
   arrays dominate an entity, retain them in verified immutable history chunks with
   bounded references. Large album membership requires chunked/sparse membership or
   an explicit actionable limit; receipt compaction alone does not fix entity size.

## Offline clients and recovery

An old-epoch mutation cannot be submitted as a new-epoch mutation under its old ID.
Preserve its original immutable bytes and outbox. On upgrading/rejoining, first load
the verified seed and current changes, then present a proposed replay against the
retained edits and tombstones. Accepted replay allocates new-epoch revisions and
records the old origin identity; it never silently discards the original outbox.
An old deleted-photo edit stays retained until explicit restore/resolution.

An active offline device that has not approved retirement prevents an unattended
cutover. A user may explicitly retire it and later rejoin it through the recovery
flow. Stale credentials or unsupported schemas pause with an actionable error;
transient connectivity does not authorize retirement or a weaker integrity check.

## Transaction and acceptance requirements

Preview reports state bytes, entity/receipt/replica counts, pending work, proposed
coverage, retired devices, incompatible clients, retained objects and expected new
state size. Validate and read back the seed/manifest before switching local protocol.
Save epoch selection and mappings atomically; cancellation, uncertain writes, full
disk and restart retain the prior epoch and all unacknowledged work. A resumed
operation recognizes the same manifest/seed identities. No sequence or key is reused.

Required acceptance before CR.5 can be checked (CR.4's current-import author bound
is verified separately; obsolete pre-release test-library migration is waived):

- Large current libraries compact with bounded live clock width and unchanged
  effective metadata, IDs, content and conflict choices.
- Repeated edits reach the old receipt/byte bound, compact, and can edit again with
  materially smaller state; user edit history remains inspectable.
- Duplicate, changed-body and cloned-author replays cannot replace authenticated
  coverage or acknowledge an unpublished mutation.
- Concurrent/incomparable checkpoints, a missing predecessor, interrupted cutover,
  lost response and failed local save preserve exact pending IDs and prior authority.
- Tombstone versus old offline edit, explicit restore, retirement, and late rejoin
  preserve both deletion safety and recoverable edits across repeated epochs.
- Supported old native/browser clients fail before metadata publication after the
  sentinel; an in-flight old writer is caught by the stopped-writer requirement.
- Remote garbage collection remains disabled during compaction. Independent future
  reclamation must prove references/retention from the authoritative epoch graph.

Until this is implemented, warn before existing limits and state plainly that ordinary
checkpointing and local journal compaction do not remove the synchronization-state cap.
