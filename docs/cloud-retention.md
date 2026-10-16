# Cloud deletion and retention

Last updated: 2026-10-17

Deleting a managed photo changes the shared catalog. Emptying Recently Deleted removes
its local photo record. **Both actions retain uploaded originals, earlier revisions,
supporting XMP, tombstones and metadata history. Bucket charges continue.** Recovery and
conflict resolution depend on this retained content. Neither action sends DeleteObject.
The deletion confirmation, cloud popup and Cloud Synchronization dialog state this policy.

Cloud Synchronization → Cloud retention and orphan media provides a native preview and
an explicit apply action. `cloud.reclaim` exposes the same workflow through CLI/MCP/control.
This is orphan reclamation, not a retention period for deleted photos: every published
historical reference remains a protection root. Supporting XMP and all metadata objects
are retained even when orphaned. Incomplete multipart uploads have the separate
`cloud.cleanupUploads` preview/apply workflow. Local `cloud.clearCache` and
`cloud.removeLocalCopy` never reclaim bucket objects.

## Preview and apply

Publish pending metadata and media on every writing device, then pause synchronization.
Wait for imports/transfers/cache moves and the current edit to finish and save. Use a
dedicated nonempty prefix containing only this protocol's library objects.

```sh
lightcraft-cli run --library /path/to/catalog cloud.pause '{}' cloud.reclaim '{"connection":"personal"}'
```

The default grace period is 30 days; `olderThanDays` accepts 7–3650. Preview returns an
exact `preview` token, eligible keys/ETags/sizes, total candidate bytes, retained-reason
counts, audit costs and `applySupported`. It performs no object writes or deletions.
Do not infer eligibility from age alone.

After reviewing the concrete preview, apply its token:

```json
{
  "connection": "personal",
  "apply": true,
  "preview": "<exact preview token>",
  "allWritersPausedAndPublished": true
}
```

The confirmation is an operator assertion, not something the tool can discover for
unknown offline devices. All writers must remain stopped until completion. Include
restored backups and independently editing installations in that decision; do not run
apply while an offline device has unpublished imports. After interruption, let uncertain
requests settle and review a fresh preview before resuming writers. Local edits, undo,
redo and library switches are blocked while apply runs. Activity cancellation and status
queries remain usable. Cancellation or a failed precondition preserves remaining objects;
rerun preview to inspect partial completion. There is no automatic reclamation job.

Desktop commands return a cancellable background activity (`wait:false`); inspect its
result with `cloud.uploadStatus`. Headless CLI/MCP waits by default and supports progress
and cancellation. Closing the retention section does not cancel a running activity.
Synchronization remains paused until explicitly resumed.

## Protection and provider scope

Each preview/apply lists the complete prefix, validates every retained head/batch,
checkpoint, append operation, manifest and receipt node, and gathers references from
all versions/conflicts/tombstones, local assets, manifests, working-import requests and
receipts. Every sequence through the observed author clocks must have its retained
operation payload; a compact checkpoint is not a substitute for those historical media
references. Retained operations must agree with checkpoint receipts and archived leaf
commitments, including causal-context hashes; archive roots and branches must have the
expected author and sequence ranges. Missing references, incomplete history, unknown metadata, malformed local
receipts, repeated pagination or bounds failures stop the audit before deletion.

Only canonical original, version-1 preview and version-1 thumbnail keys can qualify.
Reference keys and hashes protect them regardless of whether a current live photo uses
them. Candidates require a valid Last-Modified older than the grace period and complete
HEAD identity (length and ETag). Future timestamps and versioned objects are retained.
The final inventory and mutable head are checked again. Applying repeats the whole audit
and compares a token binding destination, local state, retained metadata and candidates.
A changed plan requires another review. Each delete rechecks HEAD and carries the reviewed
If-Match ETag, then requires absence on readback. It never retries without the condition.

Apply is supported on native AWS S3 connections using the default endpoint and on
loopback conformance fixtures.
Other custom endpoints, including Backblaze B2, are preview-only. There is no claim of
fresh live AWS/B2 acceptance. AWS supports ETag-conditional deletion; permissions must
include the required GetObject/DeleteObject actions. See [AWS conditional deletes](https://docs.aws.amazon.com/AmazonS3/latest/userguide/conditional-deletes.html).
Versioned buckets are excluded from reclamation: a normal delete can create a delete
marker while retaining billable versions. See [DeleteObject semantics](https://docs.aws.amazon.com/AmazonS3/latest/API/API_DeleteObject.html).
The browser cannot apply or audit remote retention; its offline/cache controls affect
only browser storage. No signer DELETE capability is introduced by this feature.

This explicit maintenance scan has limits: 20,000 objects, 64 listing pages, 100,000
mutations and 256 MiB aggregate remote metadata (64 MiB per object). Local scanning is
bounded to 100,000 entries, depth 12, 256 MiB aggregate JSON and 64 MiB per JSON file;
symlinks stop it. The reference walk is also bounded. Exceeding a limit produces an error
and no deletion, rather than treating a partial scan as complete. Audits read retained
history and candidate HEADs again on every application; this is not a cheap polling path.

## Revision history

| Date | Change |
| --- | --- |
| 2026-10-17 | CR.18: prominent retained-deletion policy and explicit, reference-checked orphan preview/apply with conditional deletion. |
