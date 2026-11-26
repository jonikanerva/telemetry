#!/bin/sh
# Write the TLS certificate and the TLS key from variables to files.
# Railway has no file mounts. The server reads the TLS material from files.
set -eu

: "${TLS_CERT:?Set TLS_CERT to the full certificate chain in PEM format}"
: "${TLS_KEY:?Set TLS_KEY to the private key in PEM format}"

mkdir -p /etc/certs/server
printf '%s\n' "$TLS_CERT" > /etc/certs/server/tls.crt
printf '%s\n' "$TLS_KEY" > /etc/certs/server/tls.key
chmod 600 /etc/certs/server/tls.key

exec /fleet-telemetry -config /etc/fleet-telemetry/config.json
