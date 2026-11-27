#!/bin/sh
# Builds the ingest image (ingest/Dockerfile) and checks the running
# container (STACK.md section 3, task image-check). The CI job image-check
# runs it on linux/amd64. It needs Docker, curl, jq, and openssl. It pushes
# no image and uses no secret.
#
# The container gets a self-signed TLS certificate that the script makes,
# and a DATABASE_URL that no database answers (port 1 of the loopback
# interface refuses the connection). The checks follow ADR 0001 ("To verify
# at first deployment") where a container without a vehicle and without a
# database can show the result:
#
#  1. ingest is PID 1. The container starts as root, and the entrypoint
#     drops to user 10001 with setpriv (issue #29): ingest runs as user and
#     group 10001, with no supplementary groups, no capabilities (CapInh,
#     CapPrm, CapEff, CapBnd, and CapAmb are 0), and NoNewPrivs 1. Its
#     environment holds DATABASE_URL but no TLS variable and no
#     MIGRATION_DATABASE_URL. The soft open-file limit equals the hard
#     limit. The ingest binary links only glibc and libgcc_s.
#  2. fleet-telemetry is a child of PID 1 with an empty environment and the
#     same credentials as ingest, and it holds no listening socket of
#     ingest.
#  3. The TLS files have mode 0600 in a directory with mode 0700.
#  4. /health answers 503: the start-up check has no answer.
#  5. The SUB socket connects to the libzmq PUB socket of the real server
#     (subscriber_settled), and the metrics of the server answer on
#     127.0.0.1:9090.
#  6. After 120 s without an answer, the gate opens in degraded mode, and a
#     TLS client through the gate gets the certificate of the server.
#     /health still answers 503. The container also gets
#     INGEST_TEST_QUEUE_BYTES=1 and INGEST_TEST_DEGRADED_AFTER_MS=1000; a debug build would stop at once or enter degraded
#     mode after 1 s, so the 120 s prove a release build.
#     Until then, every metrics read of the real binary succeeded (no
#     shortfall_read_failed, no metrics_unreadable, no subscriber_stalled).
#  7. After each of three forced child exits, the supervisor starts the
#     child again, the reader ends its socket (subscriber_disconnected or
#     subscriber_rebuilt with child_stopped), makes a new socket to the new
#     child, settles, and the gate opens again. No process is a zombie.
#  8. On SIGTERM, the shutdown sequence of ADR 0001 ends within the 30 s of
#     drainingSeconds with exit code 1 (the final flush has no database),
#     with no task_stop_timeout and no gate_connections_open line.
#  9. The log holds neither the TLS key nor the database password.
# 10. A second container with an init as PID 1 (docker run --init), as on
#     Railway: PID 1 runs as root (UID 0). ingest is not PID 1; it is a
#     child of PID 1 and has the credentials of check 1. A process of user
#     10001 cannot read /proc/1/environ, the environment of the init with
#     every variable of the start. Two positive controls come first: user
#     10001 can read the environment of ingest, and root can read
#     /proc/1/environ. The reads use exit codes only and print no
#     environment. Checks 1 (credentials, environment, open-file limit),
#     2 (child credentials and empty environment), 3, 8 (SIGTERM through
#     the init), and 9 run again in this container. Then the same container
#     starts again with its file system: the entrypoint replaces the TLS
#     files of the first start, and ingest listens again.
# 11. Fail closed: a third container that starts as user 10001 (docker run
#     --user) stops with an error from setpriv, and ingest does not start.
#
# Docker starts the first container without an init, so ingest is PID 1
# there. On Railway an init that Railway adds is PID 1 and ingest is PID 2
# (ADR 0001, "Process tree on Railway"). The init of check 10 is the init
# of Docker (tini), not the init of Railway (podman-init), so check 10
# shows the process tree and the credentials, not the behaviour of the
# Railway init. The script does not prove the signal forward or the
# reaping of the Railway init, or a forced end of ingest on Railway.
#
# The container uses the network of the host, so that the script reaches
# the loopback ports. The ports 4443, 4444, 5284, 9090, and 18080 of the
# host must be free.
set -eu

# The container of the checks that run now; checks 10 and 11 change it.
name=ingest-image-check
all_names="ingest-image-check ingest-image-check-init ingest-image-check-user"
tag=telemetry-ingest:check
health_port=18080
work="$(mktemp -d)"

show_log() {
  echo "image-check: the log of the container:" >&2
  docker logs "$name" >&2 2>&1 || true
}

fail() {
  echo "image-check: FAIL: $1" >&2
  show_log
  logs_shown=1
  exit 1
}

# An unexpected error (set -e) also shows the log of the container.
cleanup() {
  status=$?
  if [ "$status" -ne 0 ] && [ -z "${logs_shown:-}" ]; then
    echo "image-check: FAIL: exit status $status" >&2
    show_log
  fi
  for container in $all_names; do
    docker rm -f "$container" >/dev/null 2>&1 || true
  done
  rm -rf "$work"
}
trap cleanup EXIT

# The image has no USER line, so docker exec runs as root by default. The
# checks run as user 10001, the user of ingest and fleet-telemetry, as they
# did when the image named that user. Root in a container has no
# CAP_SYS_PTRACE and cannot read the environment or the descriptors of a
# process of user 10001.
in_container() {
  docker exec --user 10001:10001 "$name" "$@"
}

as_root() {
  docker exec --user 0:0 "$name" "$@"
}

# The log lines of ingest are JSON. Prints the lines that match a jq filter.
log_lines() {
  docker logs "$name" 2>&1 | jq -R -c "fromjson? | select($1)"
}

# Waits until a log line matches the filter. $2 is the limit in seconds.
wait_for_log() {
  i=0
  until [ -n "$(log_lines "$1")" ]; do
    i=$((i + 1))
    [ "$i" -le "$2" ] || fail "no log line matches $1 within $2 s"
    sleep 1
  done
}

health_status() {
  curl -s -o /dev/null -w '%{http_code}' "http://127.0.0.1:$health_port/health" || true
}

# The process ID of fleet-telemetry in the container, or nothing. The same
# command as in docs/ingest-release.md, so that this job also tests it.
child_pid() {
  in_container sh -c 'for d in /proc/[0-9]*; do case "$(tr "\0" " " < "$d/cmdline" 2>/dev/null)" in "/fleet-telemetry -config "*) echo "${d#/proc/}";; esac; done'
}

# The process ID of ingest run in the container, or nothing.
ingest_pid() {
  in_container sh -c 'for d in /proc/[0-9]*; do case "$(tr "\0" " " < "$d/cmdline" 2>/dev/null)" in "/usr/local/bin/ingest run ") echo "${d#/proc/}";; esac; done'
}

# The credentials of a process after the drop of the entrypoint (issue
# #29): user and group 10001 (real, effective, saved, and file system), no
# supplementary groups, no capabilities, and no_new_privs. /proc/<pid>/status
# holds no secret, and every user can read it.
check_credentials() {
  status="$(in_container cat "/proc/$1/status")" || fail "cannot read the status of $2 (PID $1)"
  for field in Uid Gid; do
    value="$(printf '%s\n' "$status" | awk -v f="$field:" '$1 == f { print $2, $3, $4, $5 }')"
    [ "$value" = "10001 10001 10001 10001" ] || fail "$2 has $field $value, not 10001"
  done
  groups="$(printf '%s\n' "$status" | awk '$1 == "Groups:" { print NF - 1 }')"
  [ "$groups" = 0 ] || fail "$2 has $groups supplementary groups"
  for field in CapInh CapPrm CapEff CapBnd CapAmb; do
    value="$(printf '%s\n' "$status" | awk -v f="$field:" '$1 == f { print $2 }')"
    [ "$value" = 0000000000000000 ] || fail "$2 has $field $value"
  done
  value="$(printf '%s\n' "$status" | awk '$1 == "NoNewPrivs:" { print $2 }')"
  [ "$value" = 1 ] || fail "$2 has NoNewPrivs $value"
  echo "credentials of $2 (PID $1): Uid and Gid 10001, no groups, no capabilities, NoNewPrivs 1"
}

# The environment of ingest: first a positive check that the read works
# (DATABASE_URL and PORT are there), then the absent names. fleet-telemetry
# runs as the same user and could read this file. The values stay in a
# variable of this script and are never printed.
check_ingest_environment() {
  environ="$(in_container cat "/proc/$1/environ" | tr '\0' '\n')"
  printf '%s\n' "$environ" | grep -q '^DATABASE_URL=' || fail "cannot read the environment of ingest (no DATABASE_URL)"
  printf '%s\n' "$environ" | grep -qx "PORT=$health_port" || fail "cannot read the environment of ingest (no PORT)"
  echo "environment of ingest: $(printf '%s\n' "$environ" | grep -c .) entries, DATABASE_URL present"
  if printf '%s\n' "$environ" | grep -qE '^(TLS_CERT|TLS_KEY|MIGRATION_DATABASE_URL)='; then
    fail "the environment of ingest holds a TLS variable or the migrator URL"
  fi
  in_container awk '/^Max open files/ { print "open files: soft " $4 ", hard " $5; exit ($4 != $5) }' "/proc/$1/limits" ||
    fail "the soft open-file limit is not the hard limit"
}

# fleet-telemetry: a child of ingest, an empty environment, and the
# credentials of ingest.
check_child() {
  [ "$(in_container awk '/^PPid:/ { print $2 }' "/proc/$1/status")" = "$2" ] ||
    fail "the parent of fleet-telemetry is not ingest (PID $2)"
  [ "$(in_container sh -c "wc -c < /proc/$1/environ")" = 0 ] ||
    fail "the environment of fleet-telemetry is not empty"
  check_credentials "$1" fleet-telemetry
}

check_tls_files() {
  [ "$(in_container stat -c '%a %u' /etc/fleet-telemetry/tls)" = "700 10001" ] ||
    fail "the TLS directory is not 700 and owned by 10001"
  for file in tls.crt tls.key; do
    [ "$(in_container stat -c '%a %u %g %F' "/etc/fleet-telemetry/tls/$file")" = "600 10001 10001 regular file" ] ||
      fail "$file is not a regular file with mode 600, owned by 10001:10001"
  done
}

# The shutdown after docker stop (SIGTERM): the sequence of ADR 0001 within
# the 30 s of drainingSeconds, exit code 1 (the final flush has no
# database).
check_sigterm() {
  start="$(date +%s)"
  docker stop -t 30 "$name" >/dev/null
  elapsed=$(($(date +%s) - start))
  code="$(docker inspect -f '{{.State.ExitCode}}' "$name")"
  [ "$code" = 1 ] || fail "exit code $code after SIGTERM, expected 1 (137 is the SIGKILL of docker stop)"
  [ "$elapsed" -lt 30 ] || fail "the shutdown took $elapsed s"
  messages="$(log_lines '.message != null' | jq -r '.message' | tr '\n' ' ')"
  for wanted in shutdown_started gate_stopped child_killed flush_incomplete shutdown_complete; do
    case " $messages " in
      *" $wanted "*) ;;
      *) fail "no $wanted line after SIGTERM" ;;
    esac
  done
  [ -n "$(log_lines '.message == "shutdown_started" and .reason == "SIGTERM"')" ] ||
    fail "shutdown_started does not name SIGTERM"
  [ -n "$(log_lines '.message == "child_killed" and .exit_signal == 9')" ] ||
    fail "child_killed does not show signal 9"
  # A task_stop_timeout of the gate would mean that the gate stop missed
  # its bound (since PR #25 it ends 300 ms before the abort of shutdown
  # step 1, ADR 0001 "Shutdown"); report it, do not loosen this check.
  [ -z "$(log_lines '.message == "task_stop_timeout"')" ] || fail "a task_stop_timeout line exists"
  # Shutdown step 1 waits until every slot of the gate is free (PR #24). A
  # normal stop leaves no slot taken.
  [ -z "$(log_lines '.message == "gate_connections_open"')" ] || fail "a gate_connections_open line exists"
  echo "shutdown after SIGTERM: exit code $code in $elapsed s"
}

check_no_secret_in_log() {
  docker logs "$name" > "$work/log" 2>&1
  if grep -q -e 'PRIVATE KEY' -e 'image-check-password' -e 'image-check-migrator' "$work/log"; then
    fail "the log holds the TLS key or the database password"
  fi
}

# Starts a container of the image with the variables of a Railway start.
# $@ are extra options of docker run.
start_container() {
  TLS_CERT="$(cat "$work/tls.crt")"
  TLS_KEY="$(cat "$work/tls.key")"
  export TLS_CERT TLS_KEY
  docker run -d --name "$name" --network host "$@" \
    -e PORT="$health_port" \
    -e DATABASE_URL="postgresql://telemetry_ingest:image-check-password@127.0.0.1:1/neondb" \
    -e MIGRATION_DATABASE_URL="postgresql://telemetry_migrator:image-check-migrator@127.0.0.1:1/neondb" \
    -e TLS_CERT -e TLS_KEY \
    "$tag" >/dev/null
  unset TLS_CERT TLS_KEY
}

echo "image-check: build"
docker build --pull -f ingest/Dockerfile -t "$tag" .

echo "image-check: start"
openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:prime256v1 -nodes \
  -days 1 -subj "/CN=image-check.invalid" \
  -addext "subjectAltName=DNS:image-check.invalid" \
  -keyout "$work/tls.key" -out "$work/tls.crt" 2>/dev/null
# The image must hold a release build, which never reads INGEST_TEST_*
# (ADR 0001, "Test overrides"). A debug build would stop at once with
# invalid_configuration (INGEST_TEST_QUEUE_BYTES=1 is below its minimum),
# and INGEST_TEST_DEGRADED_AFTER_MS would shorten the 120 s before degraded
# mode to 1 s: check 6 measures the 120 s.
started_at="$(date +%s)"
start_container -e INGEST_TEST_QUEUE_BYTES=1 -e INGEST_TEST_DEGRADED_AFTER_MS=1000

wait_for_log '.message == "listening"' 30
wait_for_log '.message == "child_started"' 30

echo "image-check: 1. PID 1, credentials, environment, open-file limit"
[ "$(in_container cat /proc/1/cmdline | tr '\0' ' ')" = "/usr/local/bin/ingest run " ] ||
  fail "PID 1 is not ingest run"
check_credentials 1 ingest
check_ingest_environment 1
# docs/security/image-advisories.md relies on this list of libraries.
libraries="$(in_container ldd /usr/local/bin/ingest | awk '{ print $1 }' | sed 's|^.*/||' | sort | tr '\n' ' ')"
echo "ingest links: $libraries"
for library in $libraries; do
  case "$library" in
    linux-vdso.so.1 | libc.so.6 | libm.so.6 | libgcc_s.so.1 | ld-linux-x86-64.so.2) ;;
    *) fail "ingest links $library, which docs/security/image-advisories.md does not list" ;;
  esac
done

echo "image-check: 2. fleet-telemetry is a child of PID 1 with an empty environment"
pid="$(child_pid)"
[ -n "$pid" ] || fail "fleet-telemetry does not run"
check_child "$pid" 1
# The inodes of the listening TCP sockets of ingest: the gate (4443, hex
# 115B) and /health. None may be open in the child.
health_hex="$(printf '%04X' "$health_port")"
listeners="$(in_container awk -v h="$health_hex" '$4 == "0A" && ($2 ~ /:115B$/ || $2 ~ (":" h "$")) { print $10 }' /proc/net/tcp)"
[ -n "$listeners" ] || fail "no listening socket of ingest found"
# Positive check first: the descriptors of the child hold its own
# listener on 127.0.0.1:4444 (hex 115C), so the read of the descriptors
# works. The child binds it a moment after its start.
i=0
while :; do
  own="$(in_container awk '$4 == "0A" && $2 ~ /:115C$/ { print $10 }' /proc/net/tcp)"
  child_fds="$(in_container sh -c "for f in /proc/$pid/fd/*; do readlink \"\$f\"; done")"
  [ -n "$own" ] && printf '%s\n' "$child_fds" | grep -qx "socket:\[$own\]" && break
  i=$((i + 1))
  [ "$i" -le 30 ] || fail "the descriptors of fleet-telemetry do not show its own listener on port 4444"
  sleep 1
done
echo "descriptors of fleet-telemetry: $(printf '%s\n' "$child_fds" | grep -c .), with its own listener socket:[$own]"
for inode in $listeners; do
  if printf '%s\n' "$child_fds" | grep -qx "socket:\[$inode\]"; then
    fail "fleet-telemetry holds a listening socket of ingest"
  fi
done

echo "image-check: 3. TLS files"
check_tls_files

echo "image-check: 4. /health without a database"
[ "$(health_status)" = 503 ] || fail "/health does not answer 503"

echo "image-check: 5. SUB socket and metrics of the real server"
wait_for_log '.message == "subscriber_settled"' 30
# The read of docs/ingest-release.md, inside the container with perl-base.
# Without records, the series dispatch_total does not exist yet, so the
# output is empty; the read must still connect and exit with 0.
metrics="$(in_container perl -MIO::Socket::INET -e '$s = IO::Socket::INET->new("127.0.0.1:9090") or die; print $s "GET /metrics HTTP/1.0\r\n\r\n"; print grep { /^dispatch_total/ } <$s>')" ||
  fail "the metrics read of the runbook failed on 127.0.0.1:9090"
if printf '%s\n' "$metrics" | grep -v '^dispatch_total' | grep -q .; then
  fail "the metrics read printed a line other than dispatch_total"
fi
curl -fsS "http://127.0.0.1:9090/metrics" | grep -q '^# TYPE ' ||
  fail "the metrics of fleet-telemetry do not answer on 127.0.0.1:9090"

echo "image-check: 6. degraded mode opens the gate (about 120 s)"
wait_for_log '.message == "ingest_event" and .kind == "degraded_mode"' 180
degraded_after=$(($(date +%s) - started_at))
echo "degraded mode after ${degraded_after} s"
# A debug build would log test_overrides and enter degraded mode after
# about 1 s. The release build waits 120 s.
[ "$degraded_after" -ge 110 ] ||
  fail "degraded mode after ${degraded_after} s: the binary read INGEST_TEST_DEGRADED_AFTER_MS (not a release build)"
[ -z "$(log_lines '.message == "test_overrides"')" ] ||
  fail "the binary logged test_overrides (not a release build)"
wait_for_log '.message == "ingest_event" and .kind == "gate_opened"' 10
# The server asks for a client certificate of the Tesla CA, so the
# handshake can end with an alert after the server certificate. The check
# needs only the verified server certificate: "verify return:1" for the
# certificate, and no "verify error".
handshake="$(openssl s_client -connect 127.0.0.1:4443 -servername image-check.invalid \
  -CAfile "$work/tls.crt" -verify_return_error < /dev/null 2>&1 || true)"
if ! printf '%s\n' "$handshake" | grep -q '^verify return:1$' ||
  printf '%s\n' "$handshake" | grep -q 'verify error'; then
  fail "no verified TLS handshake through the gate: $(printf '%s\n' "$handshake" | head -5)"
fi
[ "$(health_status)" = 503 ] || fail "/health does not answer 503 in degraded mode"
# The shortfall task has read the metrics of the real binary every 15 s
# since the start (about 8 reads by now) with the parser of
# crates/ingest/src/pipeline/metrics.rs. No read failed, so the gate never
# closed with metrics_unreadable, and the stall rule never fired.
for message in shortfall_read_failed metrics_unreadable subscriber_stalled; do
  [ -z "$(log_lines ".message == \"$message\"")" ] ||
    fail "a $message line before any child exit"
done
[ -z "$(log_lines '.message == "ingest_event" and .kind == "gate_closed" and ((.reason // "") | test("metrics_unreadable|subscriber_stalled"))')" ] ||
  fail "the gate closed with metrics_unreadable or subscriber_stalled"

echo "image-check: 7. three child exits: new socket, settle, gate open again; no zombie"
# ADR 0001, "To verify at first deployment": an exit of fleet-telemetry
# ends the SUB socket of the reader (subscriber_disconnected through
# libzmq, or subscriber_rebuilt with the reason child_stopped). After each
# exit the reader makes a new socket to the new child, settles, and the
# gate opens again (degraded mode, metrics readable).
count() {
  log_lines "$1" | wc -l | tr -d ' '
}
for round in 1 2 3; do
  old="$(child_pid)"
  [ -n "$old" ] || fail "fleet-telemetry does not run before exit $round"
  settled_before="$(count '.message == "subscriber_settled"')"
  opened_before="$(count '.message == "ingest_event" and .kind == "gate_opened"')"
  # The image has no kill program (no procps); kill is a builtin of sh.
  in_container sh -c "kill -9 $old" || fail "cannot end fleet-telemetry (PID $old)"
  i=0
  while :; do
    new="$(child_pid)"
    [ -n "$new" ] && [ "$new" != "$old" ] && break
    i=$((i + 1))
    [ "$i" -le 60 ] || fail "fleet-telemetry did not start again after exit $round"
    sleep 1
  done
  i=0
  until [ "$(count '.message == "subscriber_settled"')" -gt "$settled_before" ] &&
    [ "$(count '.message == "ingest_event" and .kind == "gate_opened"')" -gt "$opened_before" ]; do
    i=$((i + 1))
    [ "$i" -le 90 ] || fail "no new settled socket or no open gate after exit $round"
    sleep 1
  done
done
echo "after 3 exits: $(count '.message == "subscriber_disconnected"') subscriber_disconnected, $(count '.message == "subscriber_rebuilt" and .reason == "child_stopped"') subscriber_rebuilt child_stopped, $(count '.message == "subscriber_settled"') subscriber_settled"
[ "$(count '(.message == "subscriber_disconnected") or (.message == "subscriber_rebuilt" and .reason == "child_stopped")')" -ge 3 ] ||
  fail "fewer than three ends of the SUB socket after three child exits"
# The command of docs/ingest-release.md: it prints the stat file of each
# zombie. grep exits with 1 when it finds none.
# The scan must read the stat files (at least PID 1, the child, and the
# scanning shell) and end with the grep status 1: no match and no error.
scan="$(in_container sh -c 'set -- /proc/[0-9]*/stat; echo "files=$#"; grep -l ") Z " "$@"; echo "status=$?"' 2>/dev/null || true)"
files="$(printf '%s\n' "$scan" | sed -n 's/^files=//p')"
grep_status="$(printf '%s\n' "$scan" | sed -n 's/^status=//p')"
echo "zombie scan: $files stat files, grep status $grep_status"
[ "${files:-0}" -ge 3 ] || fail "the zombie scan read fewer than 3 stat files"
[ "$grep_status" = 1 ] || fail "zombie processes or a failed scan: $scan"
[ "$(log_lines '.message == "child_exited"' | wc -l)" -ge 3 ] || fail "fewer than three child_exited lines"

echo "image-check: 8. SIGTERM"
check_sigterm
first_elapsed="$elapsed"

echo "image-check: 9. no secret in the log"
check_no_secret_in_log

echo "image-check: 10. a container with an init as PID 1 (docker run --init)"
name=ingest-image-check-init
start_container --init
wait_for_log '.message == "listening"' 30
wait_for_log '.message == "child_started"' 30
# The init and its environment belong to root.
[ "$(in_container awk '/^Uid:/ { print $2, $3, $4, $5 }' /proc/1/status)" = "0 0 0 0" ] ||
  fail "PID 1 does not run as root"
echo "PID 1: $(in_container cat /proc/1/cmdline | tr '\0' ' '), UID 0"
ingest="$(ingest_pid)"
[ -n "$ingest" ] || fail "ingest does not run"
[ "$ingest" != 1 ] || fail "ingest is PID 1 under an init"
[ "$(in_container awk '/^PPid:/ { print $2 }' "/proc/$ingest/status")" = 1 ] ||
  fail "the parent of ingest is not the init"
echo "ingest is PID $ingest, a child of PID 1"
check_credentials "$ingest" ingest
check_ingest_environment "$ingest"
child="$(child_pid)"
[ -n "$child" ] || fail "fleet-telemetry does not run under the init"
check_child "$child" "$ingest"
check_tls_files
# Exit codes only: each read writes to /dev/null, and no environment is
# printed. Positive control 1: user 10001 can read the environment of a
# process of its own user (ingest). Positive control 2: the file
# /proc/1/environ exists and root can read it. Then the check: user 10001
# cannot read it.
in_container sh -c "cat /proc/$ingest/environ > /dev/null" ||
  fail "positive control: user 10001 cannot read the environment of ingest"
as_root sh -c 'cat /proc/1/environ > /dev/null' ||
  fail "positive control: root cannot read /proc/1/environ"
if in_container sh -c 'cat /proc/1/environ > /dev/null 2>&1'; then
  fail "a process of user 10001 can read /proc/1/environ"
fi
echo "user 10001 cannot read /proc/1/environ (both positive controls passed)"
check_sigterm
# A start of the same container again keeps its file system: the TLS
# files of the first start exist and belong to user 10001. The entrypoint
# removes them and writes new files with noclobber (issue #29). Without
# the removal, noclobber would stop the start.
docker start "$name" >/dev/null
i=0
until [ "$(log_lines '.message == "listening"' | wc -l | tr -d ' ')" -ge 2 ]; do
  i=$((i + 1))
  [ "$i" -le 30 ] || fail "no second listening line after a start of the same container"
  sleep 1
done
check_tls_files
in_container cat /etc/fleet-telemetry/tls/tls.crt | cmp -s - "$work/tls.crt" ||
  fail "the certificate after the second start differs from TLS_CERT"
echo "second start of the same container: the entrypoint replaced the old TLS files, and ingest listens again"
docker stop -t 30 "$name" >/dev/null
check_no_secret_in_log

echo "image-check: 11. fail closed: a start as user 10001 stops before ingest"
name=ingest-image-check-user
start_container --user 10001:10001
i=0
while [ "$(docker inspect -f '{{.State.Running}}' "$name")" = true ]; do
  i=$((i + 1))
  [ "$i" -le 30 ] || fail "the container that started as user 10001 still runs after 30 s"
  sleep 1
done
code="$(docker inspect -f '{{.State.ExitCode}}' "$name")"
[ "$code" != 0 ] || fail "the container that started as user 10001 exited with 0"
check_no_secret_in_log
grep -q '^setpriv: ' "$work/log" || fail "no error line of setpriv in the log"
[ -z "$(log_lines '.message != null and .message != "entrypoint_open_file_limit_not_raised"')" ] ||
  fail "ingest wrote a log line, so it started"
echo "start as user 10001: exit code $code; $(grep '^setpriv: ' "$work/log" | head -1)"

echo "image-check: PASS in ${first_elapsed} s of shutdown"
