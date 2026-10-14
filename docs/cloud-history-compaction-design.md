# Causal-history maintenance

Status: implemented receipt compaction, 2026-10-14. `cloud.sync {checkpoint:true}`
archives verified remote receipt history on native and browser transports. It does
not delete remote objects, retire causal identities, or expire tombstones.
See [cloud-reliability-tasks.md](cloud-reliability-tasks.md), CR.5–CR.7.

## Representation and integrity

Catalog format 14 and synchronization state version 2 introduced an immutable Merkle forest
for each author. A forest represents an exact contiguous prefix, with at most 64
SHA-256 roots: the descending powers of two in the prefix length. History length
therefore does not grow the number of live roots. Existing causal state version 1
can be read and upgraded; obsolete synthetic import replicas remain rejected.

Each leaf binds library, author, sequence, mutation digest and the hash of the
canonical causal context. A branch binds the same scope, aligned sequence range,
and hashes of its two halves. Nodes are canonical JSON, at most 1 KiB, stored at
`sync-v2/<library>/receipts/<sha256>.json` inside the configured storage prefix.
The checkpoint commits the forest roots. Readers verify content hashes, scope,
alignment, lengths and bounded depth; they never accept a clock as an exact receipt.

All current field heads, concurrent alternatives, deletion heads and local pending
mutations retain full contexts. Other receipts covered by the authenticated prefix
can leave live state. Photo IDs, content manifests, metadata, user edit history,
named versions and tombstone values stay unchanged. This compacts synchronization
bookkeeping, not user-visible history or the library's document payloads.

Different prefixes of the same author must have identical hashes for their shared
history. Merge reconstructs trees from locally available full receipts and fetches
only necessary missing consistency paths. Incomparable authors/checkpoints merge
normally. A changed body or context under an existing author/sequence stops the
merge atomically; both remote evidence and local pending work remain available.

## Publication and acknowledgement

Only verified remote state seeds an archive/checkpoint. Unpublished local changes
cannot appear in a checkpoint. New leaves/branches are written and verified before
the compact checkpoint is published. Uncertain writes reuse the same immutable hash
keys. Cancellation, failed writes, lost responses and failed local journal saves
leave the prior durable local state and original pending IDs intact. Unreferenced
archive objects from an interrupted attempt are harmless and retained.

A remote checkpoint can compact an operation before its author saves an ACK.
Workers verify the archived leaf against the exact local pending mutation and hydrate
its full receipt in the temporary result. Incoming changes and acknowledgement
commit together. Pending receipts remain full until that durable acknowledgement;
proof-path caches are removed from saved state. Browser ACK digests exclude temporary
witnesses so reload can finish after witnesses have been removed.

Missing or corrupt nodes stop synchronization with an archive-recovery error and
retain the outbox. Restore the retained archive objects; retrying or observing a
larger clock does not authorize dropping pending work. Archived duplicates are
verified and do not reapply effects. Old operation records can still expose a cloned
author fork during an integrity audit, including after compaction.

## Offline and retired devices

Receipt maintenance never removes author clocks or renumbers revisions. Restored
catalogs rotate their current author while retaining previous-author pending IDs.
An offline current-format device keeps its original sequence and context, merges the
checkpoint, and publishes its original pending mutations. A late edit of a deleted
photo remains retained behind the tombstone until explicit restore/resolution.

Retired authors remain in the causal clock; retirement by age is unsupported.
The 4,096-author limit remains explicit. Reclaiming author slots would require a
separate coordinated epoch/rejoin workflow and must never be inferred from receipt
compaction. No epoch cutover is needed here because causal identities and deletion
safety are preserved indefinitely.

New conditional-head writers publish head version 3, which older builds reject.
Current checkpoints carry state version 3 (CR.25 selection semantics), preserving the
required reader barrier; archive roots were introduced in version 2. Older builds reject
the newer state before normal
publication. Upgrade all devices before maintenance. An already running older append
writer can finish an immutable write; retaining its original author/sequence space
allows current readers to verify it without losing edits. This is not a credential
revocation fence or permission to discard old records. There are no existing users
or production libraries; the maintainer authorized discarding obsolete test data.

## Capacity and remaining limits

Maintenance restores capacity consumed by historical receipts at the 100,000-record
and combined 64 MiB live-state bounds. It cannot shrink active document payloads,
full pending work or current conflicts. Those still have their existing limits;
oversized entities and early warnings are CR.6/CR.7. Authors and tombstones remain
bounded by the existing library/causal limits, rather than a new time-based policy.

Archive writes are one immutable object per leaf/branch (roughly two per newly
archived receipt). Initial maintenance can therefore make many provider requests;
it runs in the synchronization worker. Operation audit pages stream with a bounded
proof cache rather than accumulating all archived history in memory. Audits still
read covered operations, and unchanged checkpoints are still fetched: request-count
optimization and budgeted audits remain CR.10. Large-library/provider performance
has not been established by small fixture acceptance.

Remote reclamation remains separate and disabled. Retain checkpoints, archive nodes,
operations, original revisions and tombstones. Manually deleting an archive node can
prevent exact pending acknowledgement even when the visible library still opens.
