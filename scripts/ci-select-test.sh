#!/bin/sh
# Tests the job selection of scripts/ci-select.sh with sample change lists,
# and checks that each watch path of STACK.md §15 selects image-check.
#
# `mise run checks` and `mise run light` run this script.
set -eu

root=$(cd "$(dirname "$0")/.." && pwd -P)
script="$root/scripts/ci-select.sh"
work=$(mktemp -d "${TMPDIR:-/tmp}/ci-select-test.XXXXXX")
trap 'rm -rf "$work"' EXIT
failures=0

fail() {
  echo "ci-select-test: FAIL: $*" >&2
  failures=$((failures + 1))
}

# check NAME EVENT EXPECTED [PATH...]: the paths form the change list.
check() {
  name=$1 event=$2 expected=$3
  shift 3
  : > "$work/changed"
  for path in "$@"; do printf '%s\n' "$path" >> "$work/changed"; done
  actual=$(sh "$script" "$event" "$work/changed" | paste -sd ' ' -)
  [ "$actual" = "$expected" ] || fail "$name: expected '$expected', got '$actual'"
}

all='run_image_check=true run_vuln=true'
none='run_image_check=false run_vuln=false'
image='run_image_check=true run_vuln=false'

check 'documents only' pull_request "$none" STACK.md README.md docs/adr/0016-proportionate-delivery.md
check 'web source' pull_request "$image" web/src/main.tsx
check 'server source' pull_request "$image" crates/server/src/app.rs
check 'image-check script' pull_request "$image" scripts/image-check-web.sh
check 'Cargo.lock' pull_request "$all" Cargo.lock
check 'member manifest' pull_request "$all" crates/domain/Cargo.toml
check 'ingest Dockerfile' pull_request "$all" ingest/Dockerfile
check 'package.json of the e2e package' pull_request 'run_image_check=false run_vuln=true' e2e/package.json
check 'ignore file' pull_request 'run_image_check=false run_vuln=true' docs/security/osv-scanner-debian.toml
check 'workflow' pull_request "$all" README.md .github/workflows/ci.yml
check 'mise.toml' pull_request "$all" mise.toml
check 'selection script' pull_request "$all" scripts/ci-select.sh
check 'empty list' pull_request "$all"
check 'quoted path' pull_request "$all" '"docs/\303\244.md"'
check 'blank lines only' pull_request "$all" '' ''
check 'push to main' push "$all" README.md
check 'schedule' schedule "$all"
check 'unknown event' workflow_dispatch "$all" README.md
check 'no event' '' "$all" README.md

# A missing or unreadable list (a failed diff) selects every job.
actual=$(sh "$script" pull_request "$work/missing" | paste -sd ' ' -)
[ "$actual" = "$all" ] || fail "missing list: got '$actual'"
actual=$(sh "$script" pull_request | paste -sd ' ' -)
[ "$actual" = "$all" ] || fail "no list argument: got '$actual'"

# Each watch path of STACK.md §15 is also a watch path of the script, and a
# sample path below it selects image-check. The reverse direction and the
# vuln input list of STACK.md §3 are not compared (STACK.md §3, "CI").
documented=$(grep '^| Watch paths ' "$root/STACK.md" | grep -o '`/[^`]*`' | tr -d '`' | sort -u)
[ "$(grep -c '^| Watch paths ' "$root/STACK.md")" -eq 2 ] || fail "STACK.md must have two Watch paths rows"
[ -n "$documented" ] || fail "no watch path in STACK.md"
script_paths=$(sed -n "s/^[A-Z]*_WATCH='\(.*\)'$/\1/p" "$script" | tr ' ' '\n' | sort -u)
missing=$(printf '%s\n' "$documented" | grep -vxF "$script_paths" || true)
[ -z "$missing" ] || fail "watch paths of STACK.md that the script lacks: $(printf '%s' "$missing" | paste -sd ' ' -)"
set -f
for pattern in $documented; do
  sample=$(printf '%s' "${pattern#/}" | sed 's#\*\*#x/y.txt#; s#\*#x#g')
  printf '%s\n' "$sample" > "$work/changed"
  actual=$(sh "$script" pull_request "$work/changed" | head -1)
  [ "$actual" = run_image_check=true ] || fail "watch path $pattern (sample $sample): got '$actual'"
done

if [ "$failures" -gt 0 ]; then
  echo "ci-select-test: $failures failures" >&2
  exit 1
fi
echo "ci-select-test: all cases pass"
