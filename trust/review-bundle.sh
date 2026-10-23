#!/usr/bin/env bash
# List the certificates a rotation adds and removes.
#
#   trust/review-bundle.sh <old.pem> <new.pem>
#
# Certificates are matched by SHA-256 fingerprint. Each added or removed
# certificate is printed with its fingerprint, subject, issuer, validity,
# key algorithm and size, and whether it is self-signed. Needs openssl.
set -euo pipefail

if [[ $# -ne 2 || ! -f "$1" || ! -f "$2" ]]; then
    echo "usage: $0 <old.pem> <new.pem>" >&2
    exit 2
fi
command -v openssl >/dev/null || { echo "$0: needs openssl" >&2; exit 2; }

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

# Split a bundle into one file per certificate, named by fingerprint.
split_bundle() {
    local bundle="$1" dir="$2"
    mkdir -p "$dir"
    awk -v dir="$dir" '
        /-----BEGIN CERTIFICATE-----/ { n++; out = dir "/" n ".pem" }
        out { print > out }
        /-----END CERTIFICATE-----/ { close(out); out = "" }
    ' "$bundle"
    local f fp
    for f in "$dir"/*.pem; do
        [[ -e "$f" ]] || continue
        fp="$(openssl x509 -in "$f" -noout -fingerprint -sha256 | sed 's/^.*=//; s/://g' | tr 'A-F' 'a-f')"
        mv "$f" "$dir/$fp.pem"
    done
}

describe() {
    local f="$1"
    local subject issuer not_before not_after key self_signed
    subject="$(openssl x509 -in "$f" -noout -subject -nameopt RFC2253 | sed 's/^subject=//')"
    issuer="$(openssl x509 -in "$f" -noout -issuer -nameopt RFC2253 | sed 's/^issuer=//')"
    not_before="$(openssl x509 -in "$f" -noout -startdate | sed 's/^notBefore=//')"
    not_after="$(openssl x509 -in "$f" -noout -enddate | sed 's/^notAfter=//')"
    key="$(openssl x509 -in "$f" -noout -text \
        | sed -n -E 's/^ *Public Key Algorithm: (.*)$/\1/p; s/^ *(Public-Key|RSA Public-Key|EC Public-Key): \(([0-9]+) bit\)$/\2 bit/p; s/^ *ASN1 OID: (.*)$/\1/p' \
        | paste -sd ' ' -)"
    self_signed=no
    # Self-signature reporting is independent of expiry and TLS security policy.
    # Verification failures mean "no"; the x509 calls above reject malformed input.
    if [[ "$subject" == "$issuer" ]] && openssl verify \
        -trusted "$f" -no-CAfile -no-CApath -no-CAstore \
        -check_ss_sig -no_check_time -auth_level 0 "$f" >/dev/null 2>&1; then
        self_signed=yes
    fi
    printf '  fingerprint  %s\n  subject      %s\n  issuer       %s\n  not before   %s\n  not after    %s\n  key          %s\n  self-signed  %s\n\n' \
        "$(basename "$f" .pem)" "$subject" "$issuer" "$not_before" "$not_after" "$key" "$self_signed"
}

split_bundle "$1" "$work/old"
split_bundle "$2" "$work/new"

added=0
removed=0
echo "ADDED in $2"
for f in "$work/new"/*.pem; do
    [[ -e "$f" ]] || continue
    if [[ ! -e "$work/old/$(basename "$f")" ]]; then
        describe "$f"
        added=$((added + 1))
    fi
done
echo "REMOVED from $1"
for f in "$work/old"/*.pem; do
    [[ -e "$f" ]] || continue
    if [[ ! -e "$work/new/$(basename "$f")" ]]; then
        describe "$f"
        removed=$((removed + 1))
    fi
done
echo "$added added, $removed removed"
