# Deploying the personal browser signer

Last updated: 2026-10-17

`lightcraft-cli cloud-signer` is the maintained personal deployment for the
[browser adapter](browser-cloud.md). It serves one storage connection, one dedicated
library prefix and one exact browser origin. A separate session token grants reads
and metadata editing within that prefix. Anyone holding it has those capabilities;
this service does not manage accounts, invitations, per-photo permissions or login.
Use a separate service/configuration/token for each independently authorized library.

AWS secret access keys stay on the signer host. The browser
receives short-lived, exact-method/key/version query-signed URLs. PUT authorization
includes a signed SHA-256 checksum, exact Content-Length and the requested conditional-head precondition.
Fetch supplies Content-Length automatically for the byte-array PUT body; it is not a
script-set header or an additional bucket CORS allowed-header entry; see the
[Fetch network request algorithm](https://fetch.spec.whatwg.org/#http-network-or-cache-fetch).
Append writes are content addressed and require the exact digest in their canonical key.
Media uploads, deletion, arbitrary destinations and bucket-wide listings are rejected.
Append discovery lists only operations, checkpoints and library control records under the
configured prefix. Rebased conditional-head libraries write changes/checkpoints under
`catalog/epochs/<n>/` (n ≥ 2) with the same bounds.

Metadata PUT bounds match the protocol: head 16 KiB, changes/operations 2 MiB,
checkpoints 64 MiB, receipt nodes 1 KiB. The fixed append manifest is capped at 64 KiB.
Every immutable key includes the exact body digest; only the head may use an observed
ETag instead of initial-create `If-None-Match: *`. Append immutable writes carry no
conditional headers and resolve uncertain writes by verified readback.

## Build and local deployment

Build the CLI from this checkout:

```sh
cargo build --release -p lightcraft-cli
```

Create a private configuration outside the repository (no credentials in this JSON):

```json
{
  "connection": {
    "id": "personal",
    "bucket": "my-private-photo-bucket",
    "region": "us-east-1",
    "prefix": "lightcraft/personal"
  },
  "origin": "https://photos.example.com",
  "expiresSeconds": 300
}
```

For a custom S3-compatible provider add its HTTPS origin as `connection.endpoint`;
the adapter uses path-style bucket addressing. An optional `connection.profile`
selects a server-side AWS shared-credentials profile. Without it the existing native
AWS environment/default-profile loader is used, including `AWS_SESSION_TOKEN` for
temporary credentials. It does not implement an EC2/ECS credential broker or automatically
renew environment credentials. Arrange credential renewal/restart on the host when needed.

Generate at least 32 random characters, store them in a private environment file or secret
manager as `LIGHTCRAFT_SIGNER_TOKEN`, and start:

```sh
lightcraft-cli cloud-signer --config /etc/lightcraft/signer.json \
  --listen 127.0.0.1:8787 --max-connections 16
```

`--token-env NAME` chooses a different environment variable. The service refuses public
bindings, empty prefixes, wildcard origins, invalid configuration and missing/short tokens.
Requests have bounded headers (8 KiB), JSON bodies (16 KiB), a 10-second read deadline
and bounded concurrent connections. It closes connections after each response, emits
no request/URL/token logs and returns generic errors without storage credentials.
Keep proxy tracing and application response-body logging disabled too: signed URLs
are bearer capabilities until they expire. Lifetimes are 30–900 seconds, default 300.

For local development only, exact loopback HTTP browser/storage origins are accepted.
Use an unprivileged service account, private credential/configuration files and a
firewall that exposes only the HTTPS reverse proxy in a remote deployment.

## AWS signer permissions

For the example bucket/prefix, attach an identity policy like the following to the
signer account (replace both placeholders). This grants no original-media writes or
deletes. A separate desktop credential can have the import/reclamation permissions
it needs. Check other attached policies too: an additional broad allow is not narrowed
by this example. Bucket/KMS policy requirements still apply.

```json
{
  "Version": "2012-10-17",
  "Statement": [
    {
      "Effect": "Allow",
      "Action": ["s3:GetObject", "s3:GetObjectVersion"],
      "Resource": "arn:aws:s3:::my-private-photo-bucket/lightcraft/personal/*"
    },
    {
      "Effect": "Allow",
      "Action": "s3:PutObject",
      "Resource": [
        "arn:aws:s3:::my-private-photo-bucket/lightcraft/personal/catalog/*",
        "arn:aws:s3:::my-private-photo-bucket/lightcraft/personal/sync-v2/*"
      ]
    },
    {
      "Effect": "Allow",
      "Action": "s3:ListBucket",
      "Resource": "arn:aws:s3:::my-private-photo-bucket",
      "Condition": {
        "StringLike": {
          "s3:prefix": [
            "lightcraft/personal/sync-v2/*/ops/*",
            "lightcraft/personal/sync-v2/*/checkpoints/*"
          ]
        },
        "NumericLessThanEquals": {"s3:max-keys": 1000}
      }
    }
  ]
}
```

See [AWS prefix and listing condition keys](https://docs.aws.amazon.com/AmazonS3/latest/userguide/amazon-s3-policy-keys.html).
This policy is a deployment template, not a verification of the permissions attached
to your account. Keep public access blocked and verify the intended prefix in your deployment.

## HTTPS reverse proxy

For example, configure Caddy separately from the static browser site:

```caddyfile
signer.example.com {
    @authorize path /authorize
    reverse_proxy @authorize 127.0.0.1:8787
    respond 404
}
```

Run the signer under your service manager with its private environment, for example:

```ini
[Unit]
Description=LightCraft personal browser signer
After=network-online.target

[Service]
User=lightcraft-signer
ExecStart=/usr/local/bin/lightcraft-cli cloud-signer --config /etc/lightcraft/signer.json
EnvironmentFile=/etc/lightcraft/signer.env
Restart=on-failure
NoNewPrivileges=true
PrivateTmp=true
ProtectSystem=strict

[Install]
WantedBy=multi-user.target
```

Give that account read access only to its own configuration/credential files. Keep
`signer.env` private (0600) and out of backups shared with browser users. Configure
server credentials for the intended bucket/prefix only, with object reads and metadata
writes plus prefix-restricted ListBucket for append discovery. Do not grant deletion
or public ACL changes. This signer cannot narrow overly broad credentials for other
programs on the server, so restrict the storage policy as well.

Query signing uses SigV4's `UNSIGNED-PAYLOAD` canonical marker while the signed
`x-amz-checksum-sha256` header binds exact write bytes. See
[AWS query authentication](https://docs.aws.amazon.com/AmazonS3/latest/developerguide/sigv4-query-string-auth.html)
and [checksum support](https://docs.aws.amazon.com/AmazonS3/latest/userguide/using-presigned-url.html).
Temporary credentials can expire before the returned URL lifetime. Custom providers
must support the signed checksum and the chosen synchronization protocol; URL issuance
alone does not verify provider behavior.

## Browser and bucket setup

Deploy the built browser bundle with the isolation headers documented in
[web.md](web.md). Configure the bucket's exact-origin CORS rule from
[browser-cloud.md](browser-cloud.md#bucket-cors), including `x-amz-checksum-sha256`,
write conditions and exposed ETags. The signer handles its own POST/OPTIONS CORS,
requires the configured Origin even for non-browser clients, and accepts the session
token only as `Authorization: Bearer …`. It never accepts cookies or a token in a URL.

In File → Cloud Storage enter:

| Field | Example |
| --- | --- |
| Connection name | `personal` (must match the signer's connection ID) |
| Prefix | `lightcraft/personal` (must match exactly) |
| Storage origin | `https://my-private-photo-bucket.s3.us-east-1.amazonaws.com` |
| Bucket root | that origin followed by `/` |
| Authorization endpoint | `https://signer.example.com/authorize` |
| Session token | the separate `LIGHTCRAFT_SIGNER_TOKEN` value |

For a path-style custom endpoint the storage origin is the provider origin and bucket
root is `https://provider.example/my-private-photo-bucket/`. A bucket root binds the
exact bucket/path as well as origin; set it explicitly. Open Cloud Synchronization,
choose the same protocol and join using the desktop's exact library ID. Publish originals
and smart previews from desktop first. Connection configuration survives reload;
the session token stays in memory and must be supplied again after reload.

Rotate the browser token by changing the private environment and restarting the signer,
then reconnect the browser with the new token. Previously issued URLs remain valid until
their short expiry (or storage credentials are revoked). Rotate AWS credentials on the
host without changing bucket, prefix, region or browser destination. Metadata/outbox and
originals must remain intact during reauthorization.

To smoke-test, check exact-origin preflight and rejection of a missing/wrong token,
join a published photo, render/export its preview, edit its title, synchronize, and
confirm the edit on desktop. Repeat with append mode when that is the deployment's
protocol. Verify an attempted out-of-prefix write/delete is refused. Never paste signed
URLs, storage keys or session tokens into reports.

## Desktop/browser scope

| Capability | Desktop / native CLI | Browser |
| --- | --- | --- |
| Import/adopt originals into managed cloud storage | Supported, durable upload/staging and XMP | Publish from desktop; ordinary browser imports remain local |
| Join/read/edit metadata and develop recipes | Both protocols | Both protocols through this signer |
| Original decoding | 512 MiB / 128 MP sensor budget | 512 MiB transfer, 32 MP decode budget; larger shared photos need smart previews |
| Shared photo descriptors | Up to 128 MP | Up to 128 MP; does not grant original decode capacity |
| Rendered export | Original by default; explicit preview export | Same policy within browser decode limits; explicit preview export stays at preview resolution |
| DNG conversion and external editor roundtrip | Native file/command workflows | External editor and native DNG workflows unavailable |
| Offline original/preview retention | Local cache and live photo/album/whole-library policy | OPFS/IndexedDB policy, verified cache and quota headroom; browser may deny persistence or erase site data |
| Cloud media reclamation | Reviewed guarded native tool | Native preview/apply required; clearing browser cache never deletes bucket objects |
| Host credentials | Environment/private AWS profile | Separate ephemeral signer token; no AWS secret |

See [input limits](cloud-input-limits.md), [retention](cloud-retention.md) and the
[current implementation contract](cloud-storage-implementation.md) for precise limits.
Local procedural acceptance verifies the maintained signer plus both adapters. HTTPS,
DNS, systemd/Caddy operation, IAM policy and each live provider require operator
verification in that deployment; local fixture acceptance is not evidence of those.

## Revision history

| Date | Change |
| --- | --- |
| 2026-10-17 | CR.20: maintained personal signer recipe, exact scope, rotation and native/browser capability matrix. |
