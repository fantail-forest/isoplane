# Platform trust bundle

`ca-bundle.pem` is the public TLS root store every guest sees as the
read-only file `/etc/ssl/certs/ca-certificates.crt`. The daemon embeds it at
build time and serves it from host memory; a guest's TLS stack reads it the
way it would read a distribution's bundle, and nothing else in the sandbox
consults it. Rotating it is a node release.

## Provenance

The bundle is the curl project's PEM conversion of Mozilla's root store,
fetched by date from `https://curl.se/ca/cacert-<YYYY-MM-DD>.pem`. Mozilla
maintains `certdata.txt` in the NSS source tree under its root program's
policy; curl runs `mk-ca-bundle.pl` over it and publishes the result with a
same-origin `.sha256` file. `ca-bundle.manifest.json` records what was
fetched:

| Field | Meaning |
|---|---|
| `generation` | Increases by one on every change to the bundle bytes, including a rollback. Two nodes on the same generation serve the same bytes. |
| `bundle_date` | The date in curl's file name. |
| `source_url` | Where the bytes came from. |
| `bundle_sha256` | SHA-256 of `ca-bundle.pem` as fetched; the daemon verifies the embedded bytes against it at every start. |
| `certdata_sha256` | The `## SHA256:` line from the bundle header: the hash of the Mozilla `certdata.txt` the bundle was converted from. |
| `mk_ca_bundle_version` | The converter version named in the header. |
| `certificate_count` | Number of `BEGIN CERTIFICATE` blocks. |
| `mozilla_as_of` | The header's Mozilla timestamp. |
| `ingested_at` | The last successful upstream check, UTC. Refreshed even when the bytes did not change. |

## What the checks prove, and what they do not

The `.sha256` file is served from the same origin as the bundle. Matching it
proves the download was not corrupted in transit. It is not a publisher
signature: anyone who can alter the bundle on that origin can alter the hash
beside it. Re-deriving the bundle from Mozilla's `certdata.txt` with curl's
`mk-ca-bundle.pl` and comparing the `certdata_sha256` is a consistency check
between two sources under different control, which is stronger, but it is
still not a signature. Neither Mozilla nor curl signs these artifacts.

A PEM bundle carries a certificate list and nothing else. Mozilla's root
program records more than that, and none of it survives the conversion:

- **Trust purpose bits.** NSS marks each root for server authentication,
  email, or both. The PEM keeps every root Mozilla trusts for server
  authentication and drops the bits; treat the file as a server-auth-only
  store and do not use it for S/MIME.
- **Distrust-after dates.** Mozilla stops trusting some roots for
  certificates issued after a date while keeping the root for older ones.
  The PEM either includes the root or omits it.
- **Name constraints.** Some roots are constrained in Firefox to particular
  domains. The constraints are not in the file; curl's page says so.
- **Negative trust.** Explicit distrust records do not appear; a distrusted
  certificate is simply absent, indistinguishable from one never listed.

## Rotating

```sh
trust/ingest-bundle.sh 2026-08-13          # fetch, verify, write pem + manifest
trust/review-bundle.sh old.pem trust/ca-bundle.pem   # what changed, per certificate
```

Find the current date on <https://curl.se/docs/caextract.html>. Ingest in a
commit of its own with the review output in the commit message, and release
the node. Check upstream weekly from an automated job; the daemon warns when
`ingested_at` is more than `trust.warn_after_days` old (default 30) and keeps
serving. An urgent distrust is an expedited out-of-band release, not a wait
for the next scheduled one.

## Licence

`ca-bundle.pem` is under the Mozilla Public License 2.0; see `NOTICE`. The
PEM file carries no licence text of its own.
