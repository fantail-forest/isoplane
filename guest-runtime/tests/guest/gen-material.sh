#!/usr/bin/env bash
# Generate the fixed key and certificate material of the cryptography and
# TLS acceptance families, plus the known answers the tests assert, as C#
# and Rust sources. Test material only; rerun to rotate everything at once.
#
# The chain and revocation information a peer can serve live at fixed
# loopback ports (the URLs are baked into the certificates): MATERIAL_PORT
# for the material the tests serve, DENIED_PORT for an address no test
# policy admits.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
MATERIAL_PORT=47001
DENIED_PORT=47002
OUT="$(mktemp -d)"
cd "$OUT"
openssl genpkey -algorithm RSA -pkeyopt rsa_keygen_bits:2048 -out root.key 2>/dev/null
openssl req -x509 -new -key root.key -days 36500 -subj "/CN=Isoplane Acceptance Root" -sha256 -out root.pem \
  -addext "basicConstraints=critical,CA:TRUE" -addext "keyUsage=critical,keyCertSign,cRLSign" 2>/dev/null
openssl genpkey -algorithm RSA -pkeyopt rsa_keygen_bits:2048 -out leaf.key 2>/dev/null
openssl req -new -key leaf.key -subj "/CN=leaf.acceptance.test" -out leaf.csr 2>/dev/null
cat > leaf.ext <<EOF
basicConstraints=CA:FALSE
keyUsage=digitalSignature,keyEncipherment
extendedKeyUsage=serverAuth
subjectAltName=DNS:leaf.acceptance.test
EOF
openssl x509 -req -in leaf.csr -CA root.pem -CAkey root.key -CAcreateserial -days 36500 -sha256 -extfile leaf.ext -out leaf.pem 2>/dev/null
# An intermediate CA under the root, and a leaf under it: the chained peer
# presents both, the guest trusts only the root.
openssl genpkey -algorithm RSA -pkeyopt rsa_keygen_bits:2048 -out intermediate.key 2>/dev/null
openssl req -new -key intermediate.key -subj "/CN=Isoplane Acceptance Intermediate" -out intermediate.csr 2>/dev/null
cat > intermediate.ext <<EOF
basicConstraints=critical,CA:TRUE,pathlen:1
keyUsage=critical,keyCertSign,cRLSign
EOF
openssl x509 -req -in intermediate.csr -CA root.pem -CAkey root.key -CAcreateserial -days 36500 -sha256 -extfile intermediate.ext -out intermediate.pem 2>/dev/null
openssl x509 -in intermediate.pem -outform DER -out intermediate.der
openssl genpkey -algorithm RSA -pkeyopt rsa_keygen_bits:2048 -out chained.key 2>/dev/null
openssl req -new -key chained.key -subj "/CN=chained.acceptance.test" -out chained.csr 2>/dev/null
cat > chained.ext <<EOF
basicConstraints=CA:FALSE
keyUsage=digitalSignature,keyEncipherment
extendedKeyUsage=serverAuth
subjectAltName=DNS:chained.acceptance.test
EOF
openssl x509 -req -in chained.csr -CA intermediate.pem -CAkey intermediate.key -CAcreateserial -days 36500 -sha256 -extfile chained.ext -out chained.pem 2>/dev/null
# A second intermediate under the first, naming its issuer's URL: a chain
# the guest completes iteratively, deepest issuer last.
openssl genpkey -algorithm RSA -pkeyopt rsa_keygen_bits:2048 -out intermediate2.key 2>/dev/null
openssl req -new -key intermediate2.key -subj "/CN=Isoplane Acceptance Intermediate 2" -out intermediate2.csr 2>/dev/null
cat > intermediate2.ext <<EOF
basicConstraints=critical,CA:TRUE,pathlen:0
keyUsage=critical,keyCertSign,cRLSign
authorityInfoAccess=caIssuers;URI:http://127.0.0.1:$MATERIAL_PORT/intermediate.der
EOF
openssl x509 -req -in intermediate2.csr -CA intermediate.pem -CAkey intermediate.key -CAcreateserial -days 36500 -sha256 -extfile intermediate2.ext -out intermediate2.pem 2>/dev/null
openssl x509 -in intermediate2.pem -outform DER -out intermediate2.der
# The leaves whose issuer, CRL and OCSP responder live at URLs: remote (all
# three, under the first intermediate), deep (under the second, issuer only),
# ocsp (issuer and responder, no CRL), denied (URLs at the port no policy
# admits).
leaf_with_urls() {
  local name="$1" issuer="$2" san="$3" urls="$4"
  openssl genpkey -algorithm RSA -pkeyopt rsa_keygen_bits:2048 -out "$name.key" 2>/dev/null
  openssl req -new -key "$name.key" -subj "/CN=$san" -out "$name.csr" 2>/dev/null
  {
    echo "basicConstraints=CA:FALSE"
    echo "keyUsage=digitalSignature,keyEncipherment"
    echo "extendedKeyUsage=serverAuth"
    echo "subjectAltName=DNS:$san"
    printf '%b' "$urls"
  } > "$name.ext"
  openssl x509 -req -in "$name.csr" -CA "$issuer.pem" -CAkey "$issuer.key" -CAcreateserial -days 36500 -sha256 -extfile "$name.ext" -out "$name.pem" 2>/dev/null
}
leaf_with_urls remote intermediate remote.acceptance.test \
  "authorityInfoAccess=caIssuers;URI:http://127.0.0.1:$MATERIAL_PORT/intermediate.der,OCSP;URI:http://127.0.0.1:$MATERIAL_PORT/ocsp\ncrlDistributionPoints=URI:http://127.0.0.1:$MATERIAL_PORT/intermediate.crl\n"
leaf_with_urls deep intermediate2 deep.acceptance.test \
  "authorityInfoAccess=caIssuers;URI:http://127.0.0.1:$MATERIAL_PORT/intermediate2.der\n"
leaf_with_urls ocsp intermediate ocsp.acceptance.test \
  "authorityInfoAccess=caIssuers;URI:http://127.0.0.1:$MATERIAL_PORT/intermediate.der,OCSP;URI:http://127.0.0.1:$MATERIAL_PORT/ocsp\n"
leaf_with_urls denied intermediate denied.acceptance.test \
  "authorityInfoAccess=caIssuers;URI:http://127.0.0.1:$DENIED_PORT/intermediate.der\n"
# The intermediate's OCSP signer.
openssl genpkey -algorithm RSA -pkeyopt rsa_keygen_bits:2048 -out ocspsigner.key 2>/dev/null
openssl req -new -key ocspsigner.key -subj "/CN=Isoplane Acceptance OCSP Signer" -out ocspsigner.csr 2>/dev/null
cat > ocspsigner.ext <<EOF
basicConstraints=CA:FALSE
keyUsage=digitalSignature
extendedKeyUsage=OCSPSigning
EOF
openssl x509 -req -in ocspsigner.csr -CA intermediate.pem -CAkey intermediate.key -CAcreateserial -days 36500 -sha256 -extfile ocspsigner.ext -out ocspsigner.pem 2>/dev/null
# A client certificate under the root, for peers that require one.
openssl genpkey -algorithm RSA -pkeyopt rsa_keygen_bits:2048 -out client.key 2>/dev/null
openssl req -new -key client.key -subj "/CN=client.acceptance.test" -out client.csr 2>/dev/null
cat > client.ext <<EOF
basicConstraints=CA:FALSE
keyUsage=digitalSignature,keyEncipherment
extendedKeyUsage=clientAuth
EOF
openssl x509 -req -in client.csr -CA root.pem -CAkey root.key -CAcreateserial -days 36500 -sha256 -extfile client.ext -out client.pem 2>/dev/null
openssl genpkey -algorithm RSA -pkeyopt rsa_keygen_bits:2048 -out stranger.key 2>/dev/null
openssl req -x509 -new -key stranger.key -days 36500 -subj "/CN=Stranger Root" -sha256 -out stranger.pem 2>/dev/null
openssl genpkey -algorithm EC -pkeyopt ec_paramgen_curve:P-256 -out ec.key 2>/dev/null

# The intermediate's revocation records, as an OpenSSL CA database: the
# leaves it issued, valid, and the remote leaf revoked.
mkdir -p ca/newcerts
echo 1000 > ca/crlnumber
index_line() {
  local status="$1" cert="$2" revoked="$3"
  local expiry serial subject
  expiry="$(openssl x509 -in "$cert" -noout -enddate | sed 's/notAfter=//' | xargs -I{} date -u -d "{}" +%y%m%d%H%M%SZ)"
  serial="$(openssl x509 -in "$cert" -noout -serial | sed 's/serial=//')"
  subject="$(openssl x509 -in "$cert" -noout -subject -nameopt RFC2253 | sed 's/subject=//; s/^/\//')"
  printf '%s\t%s\t%s\t%s\tunknown\t%s\n' "$status" "$expiry" "$revoked" "$serial" "$subject"
}
{
  index_line V remote.pem ""
  index_line V ocsp.pem ""
  index_line V chained.pem ""
} > index-good.txt
{
  index_line R remote.pem "$(date -u +%y%m%d%H%M%SZ)"
  index_line V ocsp.pem ""
  index_line V chained.pem ""
} > index-revoked.txt
cat > ca.cnf <<EOF
[ ca ]
default_ca = intermediate
[ intermediate ]
dir = ./ca
database = ./ca/index.txt
new_certs_dir = ./ca/newcerts
serial = ./ca/serial
crlnumber = ./ca/crlnumber
certificate = ./intermediate.pem
private_key = ./intermediate.key
default_md = sha256
default_crl_days = 36500
policy = any
[ any ]
commonName = supplied
EOF
echo 1000 > ca/serial
crl() {
  local index="$1" out="$2"; shift 2
  cp "$index" ca/index.txt
  openssl ca -config ca.cnf -gencrl "$@" -out "$out.pem" 2>/dev/null
  openssl crl -in "$out.pem" -outform DER -out "$out.der"
}
crl index-good.txt crl-good -crldays 36500
crl index-revoked.txt crl-revoked -crldays 36500
crl index-good.txt crl-expired -crl_lastupdate 20200101000000Z -crl_nextupdate 20200102000000Z
# OCSP responses the guest's nonce-less GET requests match: fresh for a
# century, fresh for a minute (expired by the guests' clock), signed by the
# stranger, and for the ocsp leaf.
ocsp_response() {
  local index="$1" cert="$2" out="$3"; shift 3
  cp "$index" ca/index.txt
  openssl ocsp -issuer intermediate.pem -cert "$cert" -no_nonce -reqout request.der 2>/dev/null
  openssl ocsp -index ca/index.txt -CA intermediate.pem -reqin request.der -respout "$out" "$@" 2>/dev/null
}
ocsp_response index-good.txt remote.pem ocsp-remote-good.der -rsigner ocspsigner.pem -rkey ocspsigner.key -ndays 36500
ocsp_response index-revoked.txt remote.pem ocsp-remote-revoked.der -rsigner ocspsigner.pem -rkey ocspsigner.key -ndays 36500
ocsp_response index-good.txt remote.pem ocsp-remote-expired.der -rsigner ocspsigner.pem -rkey ocspsigner.key -nmin 1
ocsp_response index-good.txt remote.pem ocsp-remote-stranger.der -rsigner stranger.pem -rkey stranger.key -ndays 36500
ocsp_response index-good.txt ocsp.pem ocsp-leaf-good.der -rsigner ocspsigner.pem -rkey ocspsigner.key -ndays 36500

printf 'isoplane' > message.txt
sig_hex="$(openssl dgst -sha256 -sign leaf.key message.txt | xxd -p | tr -d '\n')"
leaf_thumb="$(openssl x509 -in leaf.pem -noout -fingerprint -sha1 | sed 's/.*=//; s/://g')"
leaf_serial="$(openssl x509 -in leaf.pem -noout -serial | sed 's/serial=//')"
pkcs8() { openssl pkey -in "$1" -outform PEM 2>/dev/null; }
hex() { xxd -p "$1" | tr -d '\n'; }
cs="$ROOT/guest-runtime/tests/guest/src/App/CryptoMaterial.cs"
{
  echo "// Generated by guest-runtime/tests/guest/gen-material.sh; fixed keys and"
  echo "// certificates for the cryptography and TLS families. Test material only."
  echo "namespace Acceptance;"
  echo
  echo "internal static class CryptoMaterial"
  echo "{"
  for name in root leaf stranger client; do
    printf '    public const string %sCertificate = @"' "$(echo ${name:0:1} | tr a-z A-Z)${name:1}"; cat "$name.pem"; printf '";\n\n'
  done
  printf '    public const string LeafKey = @"'; pkcs8 leaf.key; printf '";\n\n'
  printf '    public const string ClientKey = @"'; pkcs8 client.key; printf '";\n\n'
  printf '    public const string EcKey = @"'; pkcs8 ec.key; printf '";\n'
  echo "}"
} > "$cs"
rs="$ROOT/crates/isoplane-runtime-acceptance/tests/common/crypto_vectors.rs"
{
  echo "//! Known answers for the cryptography family: standard vectors and the"
  echo "//! values computed with OpenSSL over the fixed material the guest embeds"
  echo "//! (generated by guest-runtime/tests/guest/gen-material.sh)."
  echo
  echo "/// SHA-256 of \"abc\" (FIPS 180-2 example)."
  echo 'pub const SHA256_ABC: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";'
  echo "/// HMAC-SHA256, RFC 4231 test case 2 (key \"Jefe\")."
  echo 'pub const HMAC_JEFE: &str = "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843";'
  echo "/// HKDF-SHA256, RFC 5869 test case 1, 42 bytes."
  echo 'pub const HKDF_RFC5869_1: &str = "3cb25f25faacd57a90434f64d0362f2a2d2d0a90cf1a5a4c5db02d56ecc4c5bf34007208d5b887185865";'
  echo "/// PBKDF2-HMAC-SHA256(\"password\", \"salt\", 1 iteration, 32 bytes)."
  echo 'pub const PBKDF2_ONE: &str = "120fb6cffcf8b32c43e7225256c4f837a86548c92ccc35480805987cb70be17b";'
  echo "/// AES-128-GCM, NIST GCM test case 2: zero key, zero IV, one zero block."
  echo 'pub const AES_GCM_CIPHERTEXT: &str = "0388dace60b6a392f328c2b971b2fe78";'
  echo 'pub const AES_GCM_TAG: &str = "ab6e47d42cec13bdf53a67b21257bddf";'
  echo "/// AES-128-CCM, RFC 3610 packet vector #1: a 13-byte nonce, 8 bytes of"
  echo "/// associated data, 23 of plaintext, an 8-byte tag."
  echo 'pub const AES_CCM_RFC3610_1_CIPHERTEXT: &str = "588c979a61c663d2f066d0c2c0f989806d5f6b61dac384";'
  echo 'pub const AES_CCM_RFC3610_1_TAG: &str = "17e8d12cfdf926e0";'
  echo "/// ChaCha20-Poly1305, RFC 8439 section 2.8.2: the 114-byte sunscreen"
  echo "/// sentence under a 12-byte nonce and 12 bytes of associated data."
  echo 'pub const CHACHA20_POLY1305_RFC8439_CIPHERTEXT: &str = "d31a8d34648e60db7b86afbc53ef7ec2a4aded51296e08fea9e2b5a736ee62d63dbea45e8ca9671282fafb69da92728b1a71de0a9e060b2905d6a5b67ecd3b3692ddbd7f2d778b8c9803aee328091b58fab324e4fad675945585808b4831d7bc3ff4def08e4b7a9de576d26586cec64b6116";'
  echo 'pub const CHACHA20_POLY1305_RFC8439_TAG: &str = "1ae10b594f09e26a7e902ecbd0600691";'
  echo "/// RSA PKCS#1 v1.5 SHA-256 signature of \"isoplane\" by the leaf key."
  echo "pub const LEAF_SIGNATURE_OF_ISOPLANE: &str = \"$sig_hex\";"
  echo "/// The leaf certificate's SHA-1 thumbprint and serial number."
  echo "pub const LEAF_THUMBPRINT: &str = \"$leaf_thumb\";"
  echo "pub const LEAF_SERIAL: &str = \"$leaf_serial\";"
} > "$rs"
tls="$ROOT/crates/isoplane-runtime-acceptance/tests/common/tls_material.rs"
{
  echo "//! The fixed certificate material of the cryptography family, for the"
  echo "//! test TLS peers (generated by guest-runtime/tests/guest/gen-material.sh):"
  echo "//! the leaf (DNS name leaf.acceptance.test) chains to the root, the"
  echo "//! chained leaf to the root through the intermediate, the remote leaf"
  echo "//! names its issuer, CRL and OCSP responder at URLs, the stranger chains"
  echo "//! to nothing the host trusts; the client certificate is the root's too."
  echo
  for pair in "ROOT_CERTIFICATE root.pem" "INTERMEDIATE_CERTIFICATE intermediate.pem" "LEAF_CERTIFICATE leaf.pem" "CHAINED_CERTIFICATE chained.pem" "REMOTE_CERTIFICATE remote.pem" "STRANGER_CERTIFICATE stranger.pem"; do
    set -- $pair
    printf 'pub const %s: &str = "' "$1"; sed 's/$/\\n/' "$2" | tr -d '\n'; printf '";\n'
  done
  for pair in "LEAF_KEY leaf.key" "CHAINED_KEY chained.key" "REMOTE_KEY remote.key" "STRANGER_KEY stranger.key"; do
    set -- $pair
    printf 'pub const %s: &str = "' "$1"; pkcs8 "$2" | sed 's/$/\\n/' | tr -d '\n'; printf '";\n'
  done
} > "$tls"
chain="$ROOT/crates/isoplane-runtime-acceptance/tests/common/chain_material.rs"
{
  echo "//! The material a chain build retrieves (generated by"
  echo "//! guest-runtime/tests/guest/gen-material.sh): the leaves whose issuer,"
  echo "//! CRL and OCSP responder live at URLs on the fixed ports, the issuers as"
  echo "//! DER, and the CRLs and OCSP responses a material peer serves, as hex."
  echo
  echo "/// The loopback port the certificates' URLs name; a material peer binds it."
  echo "pub const MATERIAL_PORT: u16 = $MATERIAL_PORT;"
  echo "/// A loopback port no test policy admits; the denied leaf's URLs name it."
  echo "pub const DENIED_PORT: u16 = $DENIED_PORT;"
  for pair in "DEEP_CERTIFICATE deep.pem" "OCSP_LEAF_CERTIFICATE ocsp.pem" "DENIED_CERTIFICATE denied.pem"; do
    set -- $pair
    printf 'pub const %s: &str = "' "$1"; sed 's/$/\\n/' "$2" | tr -d '\n'; printf '";\n'
  done
  for pair in "DEEP_KEY deep.key" "OCSP_LEAF_KEY ocsp.key" "DENIED_KEY denied.key"; do
    set -- $pair
    printf 'pub const %s: &str = "' "$1"; pkcs8 "$2" | sed 's/$/\\n/' | tr -d '\n'; printf '";\n'
  done
  for pair in "INTERMEDIATE_DER intermediate.der" "INTERMEDIATE2_DER intermediate2.der" "CRL_GOOD crl-good.der" "CRL_REVOKED crl-revoked.der" "CRL_EXPIRED crl-expired.der" "OCSP_REMOTE_GOOD ocsp-remote-good.der" "OCSP_REMOTE_REVOKED ocsp-remote-revoked.der" "OCSP_REMOTE_EXPIRED ocsp-remote-expired.der" "OCSP_REMOTE_STRANGER ocsp-remote-stranger.der" "OCSP_LEAF_GOOD ocsp-leaf-good.der"; do
    set -- $pair
    printf 'pub const %s: &str = "%s";\n' "$1" "$(hex "$2")"
  done
} > "$chain"
wc -l "$cs" "$rs" "$tls" "$chain"
rm -rf "$OUT"
