#!/bin/sh
# Selects the CI jobs of one workflow run (STACK.md §3, "CI").
#
# Usage: scripts/ci-select.sh EVENT [CHANGED]
#
# EVENT is the GitHub event name. CHANGED is a file with one changed path
# on each line (`git diff --name-only`). The script writes
# `run_image_check=true|false` and `run_vuln=true|false` to standard output,
# for $GITHUB_OUTPUT. The job ci always runs and has no output.
#
# The script fails safe: every job runs for an event other than
# pull_request, for a missing, unreadable, or empty list, for a quoted path
# (git quotes a path with special bytes), and for a change of the selection
# itself. A pattern can match more paths than the
# documented input (a `*` also matches `/`); a larger match only runs a job
# more often.
#
# scripts/ci-select-test.sh tests the decisions and compares the watch
# paths below with STACK.md §15.
set -u
set -f

# Inputs of every job: the selection itself.
ALL='/.github/workflows/ci.yml /mise.toml /scripts/ci-select.sh'
# Image inputs: the watch paths of the two Railway services (STACK.md §15,
# one line each) and the scripts of the job image-check.
INGEST_WATCH='/ingest/** /crates/ingest/** /crates/*/Cargo.toml /db/** /migrations/** /.cargo/** /Cargo.toml /Cargo.lock /rust-toolchain.toml /.dockerignore'
WEB_WATCH='/webapp/** /web/** /packages/** /crates/server/** /crates/domain/** /crates/*/Cargo.toml /Cargo.toml /Cargo.lock /.cargo/** /rust-toolchain.toml /package.json /pnpm-lock.yaml /pnpm-workspace.yaml /.npmrc /tsconfig.base.json /.dockerignore'
IMAGE_SCRIPTS='/scripts/image-check.sh /scripts/image-check-web.sh'
# Inputs of vuln (STACK.md §3, "Result rules"): the manifests, the
# lockfiles that osv-scanner reads, the scanner configuration, the
# Dockerfiles, and the deployment facts of the ignore files.
VULN='*Cargo.toml *.lock *-lock.yaml *-lock.json *lock.json *.lockfile *go.mod *go.sum *requirements*.txt *pom.xml *package.json /pnpm-workspace.yaml /deny.toml *osv-scanner.toml /ingest/Dockerfile /webapp/Dockerfile /ingest/entrypoint.sh /ingest/server_config.json /docs/security/*'

every_job() {
  echo run_image_check=true
  echo run_vuln=true
  exit 0
}

# matches PATH PATTERNS: true when one of the patterns matches the path.
matches() {
  for pattern in $2; do
    # shellcheck disable=SC2254
    case "$1" in $pattern) return 0 ;; esac
  done
  return 1
}

[ "${1:-}" = pull_request ] || every_job
changed="${2:-}"
{ [ -n "$changed" ] && [ -r "$changed" ]; } || every_job

image=false
vuln=false
count=0
while IFS= read -r file || [ -n "$file" ]; do
  [ -n "$file" ] || continue
  count=$((count + 1))
  case "$file" in \"*) every_job ;; esac
  path="/$file"
  if matches "$path" "$ALL"; then every_job; fi
  if matches "$path" "$INGEST_WATCH $WEB_WATCH $IMAGE_SCRIPTS"; then image=true; fi
  if matches "$path" "$VULN"; then vuln=true; fi
done < "$changed"
[ "$count" -gt 0 ] || every_job

echo "run_image_check=$image"
echo "run_vuln=$vuln"
