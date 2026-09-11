---
id: trust-bundle
title: Trust bundle
sidebar_position: 3
---

# Trust bundle

The public TLS root store every guest reads. A node serves it as one
read-only file, `/etc/ssl/certs/ca-certificates.crt`, from host memory; a
guest's TLS stack opens it the way it would open a distribution's bundle. It
is on by default, independent of the host operating system's trust store,
and the same on every node running the same release.

## Provenance

The bundle is the curl project's PEM conversion of Mozilla's root store,
fetched by date from `https://curl.se/ca/cacert-<YYYY-MM-DD>.pem` together
with the `.sha256` file published beside it. Mozilla maintains the source,
`certdata.txt`, under its root program; curl converts it with
`mk-ca-bundle.pl`. The repository keeps the bundle and its manifest under
`trust/`, and `trust/README.md` documents the ingest tooling.

The `.sha256` file is served from the same origin as the bundle: matching it
proves the download was not corrupted in transit, not that a particular
publisher produced it. Re-deriving the bundle from Mozilla's `certdata.txt`
and comparing the hash the bundle header names is a consistency check
between two sources under different control. Neither is a signature.

## Versioning

`trust/ca-bundle.manifest.json` records the bundle a build embeds:

| Field | Meaning |
|---|---|
| `generation` | Goes up by one on every change to the bundle bytes, including a rollback, so two nodes on the same generation serve the same bytes |
| `bundle_date` | The date in curl's file name: the bundle's publication date |
| `bundle_sha256` | SHA-256 of the bundle as fetched; verified against the embedded bytes at every start |
| `certdata_sha256` | The hash of Mozilla's `certdata.txt` the bundle header names |
| `mk_ca_bundle_version` | The converter version the header names |
| `certificate_count` | Number of certificates; verified against the embedded bytes at every start |
| `mozilla_as_of` | The header's Mozilla timestamp |
| `ingested_at` | The last successful upstream check, UTC; refreshed even when the bytes did not change |
| `source_url` | Where the bytes came from |

`wispd --check` validates the configured bundle and prints its provenance
on the `ok:` line.

The modification time a guest sees on the file is derived from the content,
not from a clock or a file on disk: 2000-01-01T00:00:00Z plus the first eight
bytes of the bundle's SHA-256, read big-endian, as seconds modulo fifty years
of 365 days. Two nodes serving the same bytes report the same time, and a
rollback changes it. It is not a date and says nothing about when the bundle
was published.

## Rotation

To update the embedded bundle, ingest the new date, review the difference,
commit, build and restart the host with the new build.

```sh
trust/ingest-bundle.sh 2026-08-13
trust/review-bundle.sh old.pem trust/ca-bundle.pem
```

The review lists each added and removed certificate by SHA-256 fingerprint,
subject, issuer, validity, key algorithm and size, and whether it is
self-signed.

The bundle is loaded when the host starts. Updating it requires restarting
the host; in-flight requests and host-owned connections do not survive the
restart.

### What a rotation reaches

A guest observes a new bundle at its next certificate validation that
happens at least five seconds of guest time after its store provider last
checked the file (unconditionally after five minutes). The check runs only
when a validation runs: nothing refreshes in the background. Established
connections stay up;
resumed sessions are still validated by `SslStream`. See the guest TLS section of
[Build a C# guest](../how-to/build-a-csharp-guest.md#tls-in-the-guest).

## Staleness

The node warns at startup when `ingested_at` is more than
`trust.warn_after_days` days old, naming both the date of the last check and
the bundle's publication date, and keeps serving. Nothing about the bundle's
age refuses startup.

The recommended cadence: check upstream weekly from an automated job that
runs the ingest script and opens a change when the bytes differ, so the
default warning at 30 days fires only when that job has failed. Treat an
urgent distrust, where Mozilla removes a root because of misissuance or
compromise, as an expedited out-of-band release rather than a wait for the
next scheduled one.

## Configuration

The `[trust]` section of `wispd.toml`:

| Key | Default | Meaning |
|---|---|---|
| `bundle` | unset | Path of a PEM bundle served instead of the one shipped with the build. `sha256` is required with it. |
| `sha256` | unset | Hex SHA-256 the operator bundle must hash to. Refused without `bundle`. |
| `warn_after_days` | `30` | Days since the last successful upstream check after which startup warns. |

Without a `[trust]` section the node serves the shipped bundle and checks it
against the shipped manifest. With `bundle`, the node reads the file, refuses
to start if its hash differs from `sha256`, and looks for a manifest written
by the ingest script beside it, named `<stem>.manifest.json`, to learn the
bundle's date, generation and last check. Without that manifest the node
serves the bundle and reports that its age is unknown.

Every refusal is a startup refusal, reported by `wispd --check` with exit
code 1:

| Refusal | Cause |
|---|---|
| `Sha256Mismatch` | The bundle's bytes hash differently from `trust.sha256`, or from the manifest beside them |
| `Sha256Missing` | `trust.bundle` without `trust.sha256` |
| `BundleMissing` | `trust.sha256` without `trust.bundle` |
| `Sha256Malformed` | `trust.sha256` is not 64 hexadecimal characters |
| `Unreadable` | The bundle or its manifest cannot be read |
| `ManifestMalformed` | The manifest is not the JSON the ingest script writes |
| `CountMismatch` | The manifest's certificate count does not describe the bundle |
| `NoCertificates` | The bundle holds no `BEGIN CERTIFICATE` block |
| `IngestedAtMalformed` | The manifest's `ingested_at` is not `YYYY-MM-DDTHH:MM:SSZ` |

## Licensing

The bundle is a conversion of Mozilla's `certdata.txt` and carries that
file's licence, the Mozilla Public License 2.0. The PEM file itself contains
no licence text; `trust/NOTICE` supplies it and ships beside the bundle.

## What a PEM bundle does not carry

A PEM bundle is a list of certificates and nothing else. Mozilla's root
program records more than that, and none of it survives the conversion:

- **Trust purpose bits.** NSS marks each root for server authentication,
  email, or both. The PEM keeps the roots trusted for server authentication
  and drops the bits: the file is a server-auth-only store.
- **Distrust-after dates.** Mozilla stops trusting some roots for
  certificates issued after a date while keeping the root for older ones.
  The PEM includes such a root or omits it; there is no middle.
- **Name constraints.** Some roots are constrained in Firefox to particular
  domains. The constraints are not in the file.
- **Negative trust.** Explicit distrust records do not appear. A distrusted
  certificate is absent, indistinguishable from one never listed.
