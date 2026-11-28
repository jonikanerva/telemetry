#!/bin/sh
# Tests the clean-up rules of scripts/test-postgres.sh with stub programs.
# No real container starts: a stub `container` on PATH logs each call.
#
# 1. A `name` and a `dir` from the environment of the caller never reach a
#    delete, also when the run stops early (a failed openssl).
# 2. A run that fails to start removes its own temporary directory.
# 3. The clean-up of a gone owner removes only a container with the label
#    of this project, and only a run directory with the name form of the
#    script.
#
# `mise run checks` runs this script. It needs jq and openssl.
set -eu

root=$(cd "$(dirname "$0")/.." && pwd -P)
script="$root/scripts/test-postgres.sh"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
mkdir "$work/bin" "$work/tmp"
log="$work/calls.log"
real_openssl=$(command -v openssl)
failures=0

fail() {
  echo "test-postgres-test: FAIL: $*" >&2
  failures=$((failures + 1))
}

# The stub `container`. STUB_LIST is the JSON of `container list`; `run`
# always fails, so no test passes the start.
cat > "$work/bin/container" << 'EOF'
#!/bin/sh
echo "container $*" >> "$STUB_LOG"
case $1 in
  --version) echo "container CLI version 1.7.1 (build: release)" ;;
  system) exit 0 ;;
  list) printf '%s\n' "${STUB_LIST:-[]}" ;;
  delete) exit 0 ;;
  run) echo "stub: run refused" >&2; exit 1 ;;
  *) exit 0 ;;
esac
EOF
# The stub `openssl` fails when STUB_OPENSSL_FAIL is set.
cat > "$work/bin/openssl" << EOF
#!/bin/sh
[ -z "\${STUB_OPENSSL_FAIL:-}" ] || exit 1
exec "$real_openssl" "\$@"
EOF
chmod +x "$work/bin/container" "$work/bin/openssl"

run_script() {
  : > "$log"
  env PATH="$work/bin:$PATH" STUB_LOG="$log" TMPDIR="$work/tmp" \
    TEST_POSTGRES_SERVER= "$@" sh "$script" local -- true > "$work/out.log" 2>&1 && return 1
  return 0
}

# 1. Inherited name and dir, early stop.
foreign_dir="$work/foreign-dir"
mkdir "$foreign_dir"
run_script name=foreign-container dir="$foreign_dir" STUB_OPENSSL_FAIL=1 ||
  fail "case 1: the run did not fail"
if grep -q 'foreign-container' "$log"; then fail "case 1: a call names the inherited container"; fi
[ -d "$foreign_dir" ] || fail "case 1: the inherited directory was removed"

# 2. A failed start removes the directory of the run.
run_script name=foreign-container dir="$foreign_dir" || fail "case 2: the run did not fail"
if grep -q 'delete.*foreign-container' "$log"; then fail "case 2: a delete names the inherited container"; fi
[ -d "$foreign_dir" ] || fail "case 2: the inherited directory was removed"
if ls "$work/tmp" | grep -q '^telemetry-test-postgres\.'; then
  fail "case 2: the run directory stays"
fi

# 3. A gone owner (PID 1 with another start time): only labelled
#    containers are deleted, and only a run directory of the script form.
stale_dir="$work/tmp/telemetry-test-postgres.abc123"
other_dir="$work/other"
mkdir "$stale_dir" "$other_dir"
list=$(jq -n --arg s "$stale_dir" --arg o "$other_dir" '[
  {configuration: {id: "ours-stale", labels: {"telemetry.test-postgres": "1@gone", "telemetry.test-postgres.dir": $s}}, status: {state: "running"}},
  {configuration: {id: "ours-other-dir", labels: {"telemetry.test-postgres": "1@gone", "telemetry.test-postgres.dir": $o}}, status: {state: "running"}},
  {configuration: {id: "not-ours", labels: {"other.project": "1@gone"}}, status: {state: "running"}}
]')
run_script STUB_LIST="$list" || fail "case 3: the run did not fail"
grep -q 'delete --force ours-stale$' "$log" || fail "case 3: the stale container was not deleted"
grep -q 'delete --force ours-other-dir$' "$log" || fail "case 3: the second stale container was not deleted"
if grep -q 'not-ours' "$log"; then fail "case 3: a call names the container of another project"; fi
[ ! -e "$stale_dir" ] || fail "case 3: the run directory of the stale container stays"
[ -d "$other_dir" ] || fail "case 3: a directory outside the name form was removed"

if [ "$failures" -ne 0 ]; then
  cat "$work/out.log" >&2
  exit 1
fi
echo "test-postgres-test: 3 cases passed"
