#!/bin/sh
# Start command of the ingest image (ADR 0001, "Layout"). It runs as root:
# the image has no USER line, so that the init of Railway (PID 1) and its
# environment belong to root (ADR 0001, "Process tree on Railway").
#
# 1. Copy TLS_CERT and TLS_KEY to shell variables that are not exported,
#    and remove TLS_CERT, TLS_KEY, and MIGRATION_DATABASE_URL from the
#    environment. ingest run needs only DATABASE_URL. The unset of cert
#    and key comes first: a variable with one of these names in the
#    container environment stays exported, and the copy would then export
#    the TLS value (issue #31). From here on, no
#    program that the script starts gets the key or the migrator URL in its
#    environment.
# 2. Write the TLS certificate and the TLS key to files. Railway has no
#    file mounts, and fleet-telemetry reads the TLS material from files.
#    The directory belongs to user 10001 and has mode 0700. The umask gives
#    each file mode 0600. printf is a shell builtin, so no process argument
#    holds the key.
#    - rm -f first: a file from an earlier start in the same container
#      filesystem can be a symbolic link that a process of user 10001
#      made. rm removes the link, not its target. set -C (noclobber) then
#      makes each > create a new file (O_EXCL), so the root shell never
#      writes through a link. No process of user 10001 runs at this time.
#    - chown gives both files to user 10001, which fleet-telemetry runs as.
# 3. Raise the soft open-file limit to the hard limit, as Go does for
#    itself. The gate holds two descriptors for each connection (ADR 0001,
#    gate rule 2).
# 4. Replace this shell with setpriv (exec), and setpriv replaces itself
#    with ingest run (exec): user and group 10001, no supplementary groups,
#    no inheritable capabilities, an empty capability bounding set, and
#    no_new_privs. So ingest takes the process of this script: PID 1 under
#    Docker without an init, PID 2 under the init of Railway.
#
# Fail closed: set -e stops the script at the first failed command, so a
# failed write or chown stops the container before ingest starts. If
# setpriv cannot drop the privileges, it exits with an error and does not
# start ingest; the container stops. setpriv needs root for the drop: if
# the container starts as another user, setgroups fails and the container
# stops (the CI job image-check checks this). No line starts ingest as
# root.
#
# Assumption (Railway does not document it): the pre-deploy command
# (ingest migrate) replaces CMD and does not run this script, because the
# image has no ENTRYPOINT; so it still gets MIGRATION_DATABASE_URL. It now
# runs as root in its own short-lived container (STACK.md section 15). Both
# other outcomes fail safe: if Railway ran this script with the command as
# arguments, the script would ignore them and start ingest run, which does
# not end, and the pre-deploy timeout fails the deployment; if the command
# got CMD as extra arguments, ingest rejects them (exit 2) and the
# deployment stops. The migrated line of the first release showed the
# first outcome (docs/ingest-release.md, section 2).
#
# The vuln task of mise.toml pins the git blob ID of this file. Update the
# pin in the same change as this file.
set -eu

: "${TLS_CERT:?Set TLS_CERT to the full certificate chain in PEM format}"
: "${TLS_KEY:?Set TLS_KEY to the private key in PEM format}"

unset cert key
cert="$TLS_CERT"
key="$TLS_KEY"
unset TLS_CERT TLS_KEY MIGRATION_DATABASE_URL

rm -f /etc/fleet-telemetry/tls/tls.crt /etc/fleet-telemetry/tls/tls.key
umask 077
set -C
printf '%s\n' "$cert" > /etc/fleet-telemetry/tls/tls.crt
printf '%s\n' "$key" > /etc/fleet-telemetry/tls/tls.key
chown 10001:10001 /etc/fleet-telemetry/tls/tls.crt /etc/fleet-telemetry/tls/tls.key

if ! ulimit -S -n "$(ulimit -H -n)"; then
  echo '{"level":"WARN","message":"entrypoint_open_file_limit_not_raised"}' >&2
fi

exec setpriv --reuid 10001 --regid 10001 --clear-groups --inh-caps=-all --bounding-set=-all --no-new-privs /usr/local/bin/ingest run
