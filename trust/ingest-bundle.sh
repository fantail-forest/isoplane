#!/usr/bin/env bash
# Ingest curl's dated Mozilla root bundle into trust/.
#
#   trust/ingest-bundle.sh <YYYY-MM-DD>
#
# Fetches https://curl.se/ca/cacert-<date>.pem and its .sha256, verifies the
# hash, reads the bundle header, counts certificates, and writes
# trust/ca-bundle.pem and trust/ca-bundle.manifest.json. The manifest's
# generation goes up by one whenever the bundle bytes change, including a
# rollback to an older date; re-ingesting identical bytes only refreshes
# ingested_at. Needs curl, sha256sum and jq.
set -euo pipefail

date_arg="${1:-}"
if [[ ! "$date_arg" =~ ^[0-9]{4}-[0-9]{2}-[0-9]{2}$ ]]; then
    echo "usage: $0 <YYYY-MM-DD>   (the date in the bundle's name on https://curl.se/docs/caextract.html)" >&2
    exit 2
fi
for tool in curl sha256sum jq; do
    command -v "$tool" >/dev/null || { echo "$0: needs $tool" >&2; exit 2; }
done

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
bundle_out="$here/ca-bundle.pem"
manifest_out="$here/ca-bundle.manifest.json"
source_url="https://curl.se/ca/cacert-${date_arg}.pem"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

curl --fail --silent --show-error --location --output "$work/bundle.pem" "$source_url"
curl --fail --silent --show-error --location --output "$work/bundle.pem.sha256" "$source_url.sha256"

published_sha="$(awk '{print $1}' "$work/bundle.pem.sha256")"
actual_sha="$(sha256sum "$work/bundle.pem" | awk '{print $1}')"
if [[ ! "$published_sha" =~ ^[0-9a-f]{64}$ ]]; then
    echo "$0: $source_url.sha256 does not contain a sha256: $(cat "$work/bundle.pem.sha256")" >&2
    exit 1
fi
if [[ "$published_sha" != "$actual_sha" ]]; then
    echo "$0: sha256 mismatch for $source_url: published $published_sha, fetched $actual_sha" >&2
    exit 1
fi

header="$(grep '^##' "$work/bundle.pem" || true)"
mozilla_as_of="$(sed -n -E 's/^## Certificate data from Mozilla (as of|downloaded on): (.*)$/\2/p' <<<"$header" | head -n1)"
mk_version="$(sed -n -E 's/^## Conversion done with mk-ca-bundle\.pl version ([0-9]+(\.[0-9]+)*)\.?$/\1/p' <<<"$header" | head -n1)"
certdata_sha="$(sed -n -E 's/^## SHA256: ([0-9a-f]{64})$/\1/p' <<<"$header" | head -n1)"
for field in mozilla_as_of mk_version certdata_sha; do
    if [[ -z "${!field}" ]]; then
        echo "$0: the bundle header has no $field line; the format has changed, inspect $source_url" >&2
        exit 1
    fi
done

count="$(grep -c -- '-----BEGIN CERTIFICATE-----' "$work/bundle.pem")"
if (( count == 0 )); then
    echo "$0: $source_url contains no certificates" >&2
    exit 1
fi

generation=1
if [[ -f "$manifest_out" ]]; then
    previous_generation="$(jq -er '.generation' "$manifest_out")"
    previous_sha="$(jq -er '.bundle_sha256' "$manifest_out")"
    if [[ "$previous_sha" == "$actual_sha" ]]; then
        generation="$previous_generation"
    else
        generation=$((previous_generation + 1))
    fi
fi
ingested_at="$(date -u +%Y-%m-%dT%H:%M:%SZ)"

jq -n \
    --argjson generation "$generation" \
    --arg bundle_date "$date_arg" \
    --arg source_url "$source_url" \
    --arg bundle_sha256 "$actual_sha" \
    --arg certdata_sha256 "$certdata_sha" \
    --arg mk_ca_bundle_version "$mk_version" \
    --argjson certificate_count "$count" \
    --arg mozilla_as_of "$mozilla_as_of" \
    --arg ingested_at "$ingested_at" \
    '{generation: $generation, bundle_date: $bundle_date, source_url: $source_url,
      bundle_sha256: $bundle_sha256, certdata_sha256: $certdata_sha256,
      mk_ca_bundle_version: $mk_ca_bundle_version, certificate_count: $certificate_count,
      mozilla_as_of: $mozilla_as_of, ingested_at: $ingested_at}' >"$work/manifest.json"

mv "$work/bundle.pem" "$bundle_out"
mv "$work/manifest.json" "$manifest_out"
echo "ingested $source_url: generation $generation, $count certificates, sha256 $actual_sha"
