#!/bin/sh
# Makes the TLS certificate and key of the test server (serve-test).
#
# Usage: e2e/tls-cert.sh <dir>
#
# The script writes <dir>/cert.pem and <dir>/key.pem: a self-signed EC
# P-256 certificate for localhost and 127.0.0.1, valid for 2 days. Each
# run makes a new key, so no key is reused or kept. The directory must
# not be in the repository history (.gitignore holds e2e/.tls/).
#
# The script uses the openssl command: LibreSSL on macOS, OpenSSL on
# Linux. It writes its own minimal openssl configuration, so the system
# configuration adds no extension.
set -eu

if [ "$#" -ne 1 ] || [ -z "$1" ]; then
  echo 'usage: e2e/tls-cert.sh <dir>' >&2
  exit 2
fi
dir=$1

umask 077
mkdir -p "$dir"
rm -f "$dir/cert.pem" "$dir/key.pem" "$dir/openssl.cnf"

cat >"$dir/openssl.cnf" <<'EOF'
[req]
distinguished_name = dn
prompt = no
x509_extensions = ext

[dn]
CN = localhost

[ext]
subjectAltName = DNS:localhost, IP:127.0.0.1
basicConstraints = critical, CA:FALSE
keyUsage = critical, digitalSignature
extendedKeyUsage = serverAuth
EOF

openssl ecparam -name prime256v1 -genkey -noout -out "$dir/key.pem"
openssl req -new -x509 -config "$dir/openssl.cnf" -key "$dir/key.pem" \
  -out "$dir/cert.pem" -days 2 -sha256
rm -f "$dir/openssl.cnf"
