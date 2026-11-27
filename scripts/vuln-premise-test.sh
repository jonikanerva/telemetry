#!/bin/sh
# Mutation test of the premise checks of the vuln task (mise.toml, STACK.md
# section 3). The checks task runs it. It needs jq and no network.
#
#   scripts/vuln-premise-test.sh
#
# For each case: copy the inputs of the premises (ingest/Dockerfile,
# ingest/entrypoint.sh, ingest/server_config.json, rust-toolchain.toml) to
# a new directory, change one file, run the premise part of the task, and
# compare the exit code. The unchanged files must pass; each change must
# fail. A premise check that a case does not make fail is too weak.
set -u
repo="$(pwd)"
work="$(mktemp -d "${TMPDIR:-/tmp}/vuln-premise.XXXXXX")" || exit 1
trap 'rm -rf "$work"' EXIT
# The premise part: the task script up to the first scan.
awk '
  /^\[tasks\.vuln\]/ { task = 1 }
  task && /^run = '"'''"'$/ { body = 1; next }
  body && /^cargo deny --locked check advisories/ { exit }
  body { print }
' "$repo/mise.toml" > "$work/premises.sh"
echo 'exit "$status"' >> "$work/premises.sh"

pass=0
total=0
# run <name> <expected exit> <shell command that changes files in the copy>
# A case fails as a setup failure when its command fails, when it changes
# no input, or when a "config:" case leaves invalid JSON: such a case would
# pass for the wrong reason.
run() {
  total=$((total + 1))
  dir="$work/case-$total"
  mkdir -p "$dir/ingest"
  cp "$repo/ingest/Dockerfile" "$repo/ingest/entrypoint.sh" "$repo/ingest/server_config.json" "$dir/ingest/"
  cp "$repo/rust-toolchain.toml" "$dir/"
  (cd "$dir" && eval "$3") || { echo "SETUP FAILED (command): $1"; return; }
  if [ "$2" -ne 0 ] && cmp -s "$repo/ingest/Dockerfile" "$dir/ingest/Dockerfile" &&
    cmp -s "$repo/ingest/entrypoint.sh" "$dir/ingest/entrypoint.sh" &&
    cmp -s "$repo/ingest/server_config.json" "$dir/ingest/server_config.json" &&
    cmp -s "$repo/rust-toolchain.toml" "$dir/rust-toolchain.toml"; then
    echo "SETUP FAILED (no input changed): $1"
    return
  fi
  case "$1" in
    config:*)
      jq empty "$dir/ingest/server_config.json" 2>/dev/null ||
        { echo "SETUP FAILED (invalid JSON): $1"; return; } ;;
  esac
  (cd "$dir" && sh "$work/premises.sh" > "$dir/out" 2>&1)
  code=$?
  if [ "$code" -eq "$2" ]; then
    pass=$((pass + 1))
    echo "ok   ($code) $1: $(grep '^vuln:' "$dir/out" | head -1 | cut -c1-110)"
  else
    echo "FAIL (got $code, want $2) $1"
    cat "$dir/out"
  fi
}

D=ingest/Dockerfile
E=ingest/entrypoint.sh
C=ingest/server_config.json
# jq edits keep the file valid JSON.
jqedit() { printf "jq '%s' $C > c.tmp && mv c.tmp $C" "$1"; }
# Insert one line (from the variable L, without escape processing) before
# the exec line of the entrypoint.
before_exec() {
  L="$1" awk '/^exec / { print ENVIRON["L"] } { print }' "$E" > e.tmp && mv e.tmp "$E"
}
# Put the exec line inside a block that never runs.
exec_in_dead_block() {
  awk '/^exec / { print "if false; then"; print; print "fi"; next } { print }' "$E" > e.tmp && mv e.tmp "$E"
}
# End the exec line with a carriage return.
exec_with_cr() {
  awk '/^exec / { printf "%s\r\n", $0; next } { print }' "$E" > e.tmp && mv e.tmp "$E"
}
# A second "zmq" object in the root, before "tls". jq keeps the last one;
# Go merges both into one field.
repeat_zmq() {
  awk '/^  "tls": [{]$/ { print "  \"zmq\": {\"addr\": \"tcp://0.0.0.0:5284\"}," } { print }' "$C" > c.tmp &&
    mv c.tmp "$C"
}

run "baseline" 0 "true"
run "Dockerfile: USER root" 1 "sed -i.bak 's/^USER 10001:10001$/USER root/' $D"
run "Dockerfile: extra COPY . ." 1 "printf 'COPY . .\n' >> $D"
run "Dockerfile: builder tag with another Rust version" 1 "sed -i.bak 's/^FROM rust:1.100.0-slim-trixie/FROM rust:1.99.0-slim-trixie/' $D"
run "Dockerfile: builder tag on another Debian release" 1 "sed -i.bak 's/^FROM rust:1.100.0-slim-trixie/FROM rust:1.100.0-slim-bookworm/' $D"
run "Dockerfile: final stage on another Debian release" 1 "sed -i.bak 's/^FROM debian:trixie-slim$/FROM debian:bookworm-slim/' $D"
run "rust-toolchain.toml: another channel" 1 "sed -i.bak 's/^channel = .*/channel = \"1.101.0\"/' rust-toolchain.toml"
run "Dockerfile: other child config path" 1 "sed -i.bak 's|^ENV INGEST_CHILD_CONFIG=.*|ENV INGEST_CHILD_CONFIG=/tmp/other.json|' $D"
run "Dockerfile: parser directive" 1 "{ printf '# syntax=docker/dockerfile:1\n'; cat $D; } > d.tmp && mv d.tmp $D"
run "Dockerfile: byte outside ASCII" 1 "printf '# caf\303\251\n' >> $D"
run "Dockerfile: ENTRYPOINT in place of CMD" 1 "sed -i.bak 's/^CMD /ENTRYPOINT /' $D"
run "entrypoint: starts the server itself" 1 "sed -i.bak 's|^exec /usr/local/bin/ingest run$|exec /fleet-telemetry -config /etc/fleet-telemetry/config.json|' $E"
run "entrypoint: a second exec" 1 "printf 'exec /usr/local/bin/ingest migrate\n' >> $E"
run "entrypoint: ingest with another argument" 1 "sed -i.bak 's|^exec /usr/local/bin/ingest run$|exec /usr/local/bin/ingest run -config /x|' $E"
run "entrypoint: starts the server in the background" 1 "sed -i.bak 's|^exec /usr/local/bin/ingest run$|/fleet-telemetry \\& exec /usr/local/bin/ingest run|' $E"
# Bypasses of the earlier denylist (review round 1 of PR #23, R26b, R28,
# R29) and other changes that the line allowlist must reject.
run "entrypoint: server through PATH before exec (R26b)" 1 "before_exec 'PATH=/:\$PATH fleet-telemetry &'"
run "entrypoint: server name built from parts (R29)" 1 "before_exec 'f=/fleet-tele; \"\${f}metry\" &'"
run "entrypoint: exec inside a block that never runs (R28)" 1 "exec_in_dead_block"
run "entrypoint: source of another file before exec" 1 "before_exec '. /tmp/start.sh'"
run "entrypoint: trace mode prints the key" 1 "sed -i.bak 's/^set -eu\$/set -eux/' $E"
run "entrypoint: migrator URL stays in the environment" 1 "sed -i.bak 's/^unset TLS_CERT TLS_KEY MIGRATION_DATABASE_URL\$/unset TLS_CERT TLS_KEY/' $E"
run "entrypoint: another shell in the shebang" 1 "sed -i.bak '1s|^#!/bin/sh\$|#!/bin/bash|' $E"
run "entrypoint: indented comment line" 1 "before_exec '  # a comment'"
run "entrypoint: carriage return" 1 "exec_with_cr"
# Review round 2 of PR #23, note 1: the byte rule and the exact line
# comparison each need a case that only they catch.
run "entrypoint: byte outside ASCII in a comment line" 1 "printf '# caf\303\251\n' >> $E"
run "entrypoint: trailing space on the exec line" 1 "sed -i.bak 's|^exec /usr/local/bin/ingest run\$|exec /usr/local/bin/ingest run |' $E"
run "config: zmq.addr on all interfaces" 1 "$(jqedit '.zmq.addr = "tcp://0.0.0.0:5284"')"
run "config: zmq.addr with a wildcard" 1 "$(jqedit '.zmq.addr = "tcp://*:5284"')"
run "config: zmq.addr with localhost" 1 "$(jqedit '.zmq.addr = "tcp://localhost:5284"')"
run "config: zmq.addr on IPv6 any" 1 "$(jqedit '.zmq.addr = "tcp://[::]:5284"')"
run "config: zmq.addr with a longer host name" 1 "$(jqedit '.zmq.addr = "tcp://127.0.0.1.example:5284"')"
run "config: zmq.addr with a trailing space" 1 "$(jqedit '.zmq.addr = "tcp://127.0.0.1:5284 "')"
run "config: zmq.addr missing" 1 "$(jqedit 'del(.zmq.addr)')"
run "config: zmq key Addr in other case" 1 "$(jqedit '.zmq.Addr = "tcp://0.0.0.0:5284"')"
run "config: root key ZMQ in other case" 1 "$(jqedit '.ZMQ = {"addr": "tcp://0.0.0.0:5284"}')"
run "config: repeated zmq object" 1 "repeat_zmq"
run "config: repeated addr key" 1 "sed -i.bak 's|\"addr\": \"tcp://127.0.0.1:5284\"|\"addr\": \"tcp://127.0.0.1:5284\", \"addr\": \"tcp://0.0.0.0:5284\"|' $C"
run "config: kafka dispatcher" 1 "$(jqedit '.records.V = ["kafka"]')"
run "config: metrics on all interfaces" 1 "$(jqedit '.monitoring.prometheus_metrics_host = "0.0.0.0"')"
run "config: status port" 1 "$(jqedit '.status_port = 8081')"
echo "$pass of $total cases as expected"
[ "$pass" -eq "$total" ]
