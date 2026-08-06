#!/bin/sh
# Regenerate the test certificates in this directory.
#
# They are committed rather than generated at test time so the suite needs no certificate
# toolchain and produces the same bytes on Linux, macOS and Windows. Run this only if they
# genuinely need replacing — a hundred-year validity means expiry is not a reason.
#
# The shape is deliberately the real one rather than a single self-signed certificate: rustls
# refuses to use a CA certificate as an end entity (`CaUsedAsEndEntity`), and more importantly
# a test that trusted the leaf directly would not exercise the chain building that every real
# deployment depends on.
set -eu
cd "$(dirname "$0")"

# 1. The certificate authority. This is the trust anchor an uplink's `ca_file` points at.
openssl req -x509 -newkey rsa:2048 -nodes -sha256 -days 36500 \
  -keyout ca-key.pem -out test-ca.pem \
  -subj "/CN=aprsr test CA" \
  -addext "basicConstraints=critical,CA:TRUE,pathlen:0" \
  -addext "keyUsage=critical,keyCertSign,cRLSign"

# 2. The server certificate it signs, carrying every name the tests connect by. `IP:127.0.0.1`
#    is what lets an uplink verify a server it reached on a port-0 loopback address.
openssl req -newkey rsa:2048 -nodes -sha256 \
  -keyout test-key.pem -out server.csr -subj "/CN=aprsr-test"

cat > server.ext <<'EXT'
basicConstraints = critical,CA:FALSE
keyUsage = critical,digitalSignature,keyEncipherment
extendedKeyUsage = serverAuth
subjectAltName = DNS:localhost, DNS:aprsr-test, IP:127.0.0.1, IP:::1
EXT

openssl x509 -req -in server.csr -CA test-ca.pem -CAkey ca-key.pem -CAcreateserial \
  -out server.pem -days 36500 -sha256 -extfile server.ext

# 3. `test-cert.pem` has the leaf first and then the authority — the `fullchain.pem` shape a
#    certificate authority hands an operator, so `read_certificates` is exercised on a file
#    with more than one certificate in it.
cat server.pem test-ca.pem > test-cert.pem

# The CA's private key is deliberately not kept: nothing needs to sign anything again, and a
# committed signing key is a bad habit even for a test authority.
rm -f server.csr server.ext server.pem ca-key.pem test-ca.srl
chmod 644 test-ca.pem test-cert.pem test-key.pem
