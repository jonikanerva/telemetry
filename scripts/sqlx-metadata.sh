#!/bin/sh
# Checks or writes the SQLx query metadata against a migrated, isolated
# database (STACK.md §3 and §5): on the Neon branch `test` locally, on the
# PostgreSQL server of the runner in CI. Each crate that depends on SQLx
# keeps its own metadata in `crates/<crate>/.sqlx/`. The script runs
# `cargo sqlx prepare [--check]` once in the directory of each such crate,
# without `--workspace`, so that each image build reads only the metadata
# of its own crate.
#
#   scripts/sqlx-metadata.sh check     # used by `mise run verify`
#   scripts/sqlx-metadata.sh prepare   # writes the metadata after a query change
#
# TEST_DATABASE_URL must point to the test database (README.md,
# "Verification"; in CI, `scripts/ci-postgres.sh` sets it). The script:
#
# 1. refuses the production endpoint;
# 2. refuses a `.sqlx/` directory at the workspace root: the macros read
#    it when a crate has no metadata of its own, and the ingest image does
#    not hold it;
# 3. creates a database with a unique name on the same server, migrates
#    it with `migrations/`, and runs `cargo sqlx prepare` against it in
#    each crate that depends on SQLx;
# 4. drops the database at the end, also after a failure.
#
# The URL keeps every query parameter except `sslmode` and
# `channel_binding`, and gets `sslmode=verify-full`. Every `sqlx` call
# uses `--no-dotenv`, so no `.env` file of a parent directory is read.
# The script never prints the URL.
set -eu

mode=${1:-}
case $mode in
  check) check_flag=--check ;;
  prepare) check_flag= ;;
  *) echo "usage: scripts/sqlx-metadata.sh check|prepare" >&2; exit 2 ;;
esac

if [ -z "${TEST_DATABASE_URL:-}" ]; then
  echo "sqlx-metadata: TEST_DATABASE_URL is not set. Export the URL of the test database (README.md, Verification)." >&2
  exit 1
fi
# Host names are case-insensitive, so compare in lower case.
case $(printf '%s' "$TEST_DATABASE_URL" | tr '[:upper:]' '[:lower:]') in
  *ep-young-dawn-bgyaj83l*)
    echo "sqlx-metadata: TEST_DATABASE_URL names the production endpoint. Refused." >&2
    exit 1 ;;
esac

if [ -e .sqlx ]; then
  echo "sqlx-metadata: .sqlx/ exists at the workspace root. Each crate keeps its metadata in crates/<crate>/.sqlx/. Remove the root directory." >&2
  exit 1
fi
# The crates with a SQLx dependency: a line that starts with `sqlx` in the
# dependency tables of the crate manifest.
crates=$(grep -l -E '^sqlx[[:space:]]*(=|\.)' crates/*/Cargo.toml | sed 's|/Cargo.toml$||')
if [ -z "$crates" ]; then
  echo "sqlx-metadata: no crate depends on sqlx" >&2
  exit 1
fi

base=${TEST_DATABASE_URL%%\?*}
case $TEST_DATABASE_URL in
  *\?*) query=${TEST_DATABASE_URL#*\?} ;;
  *) query= ;;
esac
server=${base%/*}
params=$(printf '%s' "$query" | tr '&' '\n' | grep -v -E '^(sslmode|channel_binding)=' | paste -s -d '&' - || true)
name="sqlx_meta_$(date +%s)_$$"
DATABASE_URL="$server/$name?sslmode=verify-full${params:+&$params}"
export DATABASE_URL

cleanup() {
  if ! sqlx database drop --no-dotenv -y --force >/dev/null 2>&1; then
    echo "sqlx-metadata: could not drop database $name; the test sweep drops it after 6 hours" >&2
  fi
}
trap cleanup EXIT
trap 'exit 1' INT TERM

sqlx database setup --no-dotenv --source migrations >/dev/null
# `cargo sqlx prepare` sets SQLX_OFFLINE=false
# for its own `cargo check`. Without `--workspace`, it checks only the
# crate of the current directory and uses `<crate>/.sqlx/`.
for crate in $crates; do
  echo "sqlx-metadata: $mode $crate"
  # shellcheck disable=SC2086
  (cd "$crate" && cargo sqlx prepare --no-dotenv $check_flag -- --all-targets --locked)
done
