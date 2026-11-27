#!/bin/sh
# Start command of the ingest image (ADR 0001, "Layout").
#
# 1. Write the TLS certificate and the TLS key from the variables TLS_CERT
#    and TLS_KEY to files. Railway has no file mounts, and fleet-telemetry
#    reads the TLS material from files. The directory belongs to the user
#    of the process and has mode 0700. The umask gives each file mode 0600.
#    printf is a shell builtin, so no process argument holds the key.
# 2. Raise the soft open-file limit to the hard limit, as Go does for
#    itself. The gate holds two descriptors for each connection (ADR 0001,
#    gate rule 2).
# 3. Remove the TLS variables and MIGRATION_DATABASE_URL, and replace this
#    shell with ingest (exec), so that ingest is PID 1 and its environment
#    (/proc/1/environ) holds neither the TLS key nor the migrator URL.
#    ingest run needs only DATABASE_URL. fleet-telemetry runs as the same
#    user and could read /proc/1/environ.
#
# Assumption (Railway does not document it): the pre-deploy command
# (ingest migrate) replaces CMD and does not run this script, because the
# image has no ENTRYPOINT; so it still gets MIGRATION_DATABASE_URL. Both
# other outcomes fail safe: if Railway ran this script with the command as
# arguments, the script would ignore them and start ingest run, which does
# not end, and the pre-deploy timeout fails the deployment; if the command
# got CMD as extra arguments, ingest rejects them (exit 2) and the
# deployment stops. The migrated line of the first release shows which
# outcome holds (docs/ingest-release.md, section 2).
#
# The vuln task of mise.toml compares the lines of this file with a strict
# allowlist. Change the allowlist in the same change as this file.
set -eu

: "${TLS_CERT:?Set TLS_CERT to the full certificate chain in PEM format}"
: "${TLS_KEY:?Set TLS_KEY to the private key in PEM format}"

umask 077
printf '%s\n' "$TLS_CERT" > /etc/fleet-telemetry/tls/tls.crt
printf '%s\n' "$TLS_KEY" > /etc/fleet-telemetry/tls/tls.key
unset TLS_CERT TLS_KEY MIGRATION_DATABASE_URL

if ! ulimit -S -n "$(ulimit -H -n)"; then
  echo '{"level":"WARN","message":"entrypoint_open_file_limit_not_raised"}' >&2
fi

exec /usr/local/bin/ingest run
