#!/bin/sh
# Starts the PostgreSQL server of the database tests (STACK.md §3, ADR 0017).
#
#   scripts/test-postgres.sh setup           # local: check the tool, pull the image
#   scripts/test-postgres.sh local -- CMD... # local: a new server for CMD
#   scripts/test-postgres.sh ci              # CI: a server on the runner
#
# Both back ends use the same server set-up:
#
# - the image of the `postgres_image` line of .github/workflows/ci.yml, the
#   one pin of the test server (`mise run deps` checks it);
# - a new test CA and a server certificate for `localhost` (X.509 v3) for
#   each run. rustls checks the subject alternative name and rejects a CA
#   certificate as the server certificate;
# - TLS only on TCP (`hostssl` and SCRAM); the set-up uses the socket in
#   the container;
# - the role `neondb_owner` (LOGIN, CREATEROLE, CREATEDB, not a superuser)
#   and the database `neondb` that it owns, as on Neon;
# - TEST_DATABASE_URL with `sslrootcert` set to the test CA. SQLx adds that
#   CA to the Mozilla roots, and the storage keeps `sslmode=verify-full`
#   (crates/ingest/src/storage/connect.rs).
#
# `local` uses Apple `container` on the Mac. Each run gets a server of its
# own: data in tmpfs, published on 127.0.0.1 with a random port in
# 10000-18999, and labels with the owner process. The server goes away
# when CMD ends. Each start removes the servers whose owner process is
# gone, for example after `kill -9`. At most 4 servers of this project run
# at the same time on the Mac; a new run waits for a free place. A nested
# call inside CMD (for example `mise run test` inside `mise run verify`)
# uses the server of the outer call.
#
# `ci` uses Docker on the GitHub Actions runner with the host network and
# writes TEST_DATABASE_URL to $GITHUB_ENV.
#
# The passwords are random for each run. The script prints no URL.
set -eu

root=$(cd "$(dirname "$0")/.." && pwd -P)
label=telemetry.test-postgres
max_servers=4

say() { echo "test-postgres: $*" >&2; }
die() {
  say "$*"
  exit 1
}

pinned_image() {
  image=$(sed -n 's/^ *postgres_image: //p' "$root/.github/workflows/ci.yml")
  [ "$(printf '%s\n' "$image" | grep -c .)" -eq 1 ] ||
    die "cannot read the one postgres_image line of .github/workflows/ci.yml"
}

# Writes the test CA, the server certificate and key, and pg_hba.conf to $1.
make_tls() {
  dir=$1
  openssl req -x509 -new -nodes -days 2 -newkey ec -pkeyopt ec_paramgen_curve:P-256 \
    -subj '/CN=telemetry test CA' \
    -addext 'basicConstraints=critical,CA:TRUE' \
    -addext 'keyUsage=critical,keyCertSign,cRLSign' \
    -keyout "$dir/ca.key" -out "$dir/ca.crt" 2>/dev/null
  openssl req -new -nodes -newkey ec -pkeyopt ec_paramgen_curve:P-256 \
    -subj '/CN=localhost' -keyout "$dir/server.key" -out "$dir/server.csr" 2>/dev/null
  printf '%s\n' \
    'basicConstraints=critical,CA:FALSE' \
    'keyUsage=critical,digitalSignature' \
    'extendedKeyUsage=serverAuth' \
    'subjectAltName=DNS:localhost' > "$dir/server.ext"
  openssl x509 -req -days 2 -in "$dir/server.csr" -CA "$dir/ca.crt" -CAkey "$dir/ca.key" \
    -CAcreateserial -extfile "$dir/server.ext" -out "$dir/server.crt" 2>/dev/null
  rm -f "$dir/ca.key" "$dir/ca.srl" "$dir/server.csr" "$dir/server.ext"
  # Socket connections (inside the container only) use trust for the
  # set-up. A TCP connection must use TLS and a SCRAM password.
  printf '%s\n' \
    'local all all trust' \
    'hostssl all all all scram-sha-256' > "$dir/pg_hba.conf"
  chmod 0600 "$dir/server.key"
}

# The command of the container: copy the TLS files with the owner and mode
# that PostgreSQL requires, then run the entrypoint of the image. $1 is the
# value of listen_addresses; the other arguments are more server settings.
server_command() {
  listen=$1
  shift
  printf '%s' "
    install -d -o postgres -g postgres -m 0700 /tls
    install -o postgres -g postgres -m 0600 /certs/server.key /tls/server.key
    install -o postgres -g postgres -m 0644 /certs/server.crt /certs/pg_hba.conf /tls/
    exec docker-entrypoint.sh postgres \
      -c listen_addresses=$listen \
      -c ssl=on \
      -c ssl_cert_file=/tls/server.crt \
      -c ssl_key_file=/tls/server.key \
      -c hba_file=/tls/pg_hba.conf $*"
}

# Waits until the real server accepts a TLS session on TCP in the
# container. The entrypoint first starts a temporary server without TCP
# for its set-up. $1 is the runtime (docker or container), $2 the name.
wait_inside() {
  for _ in $(seq 1 60); do
    # The password goes by name through the environment, not on argv.
    if PGPASSWORD=$superuser_password "$1" exec --env PGPASSWORD "$2" \
        psql "host=localhost user=postgres dbname=postgres sslmode=require" -tAc 'SELECT 1' > /dev/null 2>&1; then
      return 0
    fi
    sleep 1
  done
  "$1" logs "$2" >&2 || true
  die "the server $2 did not start in 60 s"
}

# The password reaches psql on standard input, not on argv. It is hex, so
# it needs no quoting.
create_owner() { # $1 = runtime, $2 = name
  "$1" exec --interactive "$2" psql -v ON_ERROR_STOP=1 \
    -U postgres -d postgres -q -tA > /dev/null <<EOF
CREATE ROLE neondb_owner LOGIN CREATEROLE CREATEDB NOSUPERUSER PASSWORD '$password';
CREATE DATABASE neondb OWNER neondb_owner;
EOF
}

# --- CI ------------------------------------------------------------------

cmd_ci() {
  : "${RUNNER_TEMP:?Run this mode on a GitHub Actions runner}"
  : "${GITHUB_ENV:?Run this mode on a GitHub Actions runner}"
  pinned_image
  dir="$RUNNER_TEMP/postgres-tls"
  rm -rf "$dir"
  mkdir -m 0700 "$dir"
  make_tls "$dir"
  superuser_password=$(openssl rand -hex 24)
  password=$(openssl rand -hex 24)
  echo "::add-mask::$superuser_password"
  echo "::add-mask::$password"
  # Host network: the server listens on 127.0.0.1 and ::1 of the runner.
  POSTGRES_PASSWORD=$superuser_password docker run --detach --name postgres --network host \
    --env POSTGRES_PASSWORD \
    --volume "$dir:/certs:ro" \
    --entrypoint sh "$image" -c "$(server_command localhost)" > /dev/null
  wait_inside docker postgres
  create_owner docker postgres
  docker exec postgres psql -U postgres -tAc 'SELECT version()'
  echo "TEST_DATABASE_URL=postgresql://neondb_owner:$password@localhost:5432/neondb?sslrootcert=$dir/ca.crt" >> "$GITHUB_ENV"
  say "PostgreSQL is ready with TLS; TEST_DATABASE_URL is set"
}

# --- local (Apple container) ---------------------------------------------

require_container() {
  command -v container > /dev/null ||
    die "the Apple container tool is missing. Install version 1 or later from https://github.com/apple/container/releases (README.md, Verification)."
  version=$(container --version | sed -n 's/.*version \([0-9][0-9.]*\).*/\1/p')
  case $version in
    '' | 0.*) die "Apple container ${version:-of an unknown version} is too old; version 1 or later is required" ;;
  esac
  # An agent sandbox that blocks the XPC service of the tool fails here too.
  container system status > /dev/null 2>&1 ||
    die "the Apple container service does not answer. Run 'container system start' (or run outside a sandbox that blocks it), then run the command again."
}

# A process identity that survives PID reuse: the PID and the start time.
owner_of() { # $1 = pid
  started=$(LC_ALL=C TZ=UTC ps -o lstart= -p "$1" 2> /dev/null | tr -s ' ' '_' | sed 's/_$//') || true
  [ -n "$started" ] || return 1
  printf '%s@%s' "$1" "$started"
}

# An owner is gone only when no process has its PID, or the process with
# that PID has another start time. Any unclear result counts as alive: a
# removal of a live server would break that run.
owner_gone() { # $1 = owner token
  pid=${1%%@*}
  case $pid in '' | *[!0-9]*) return 1 ;; esac
  if current=$(owner_of "$pid"); then
    [ "$current" != "$1" ]
    return
  fi
  case $(LC_ALL=C kill -0 "$pid" 2>&1) in
    *"No such process"*) return 0 ;;
    *) return 1 ;;
  esac
}

# Lines of "name<TAB>state<TAB>owner<TAB>directory" for each container
# with the label of this project.
list_servers() {
  container list --all --format json |
    jq -r --arg l "$label" '.[] | select(.configuration.labels[$l] != null)
      | [.configuration.id, .status.state, .configuration.labels[$l],
         (.configuration.labels[$l + ".dir"] // "-")] | @tsv'
}

# Deletes the container $1 only when it has the label of this project with
# the owner $2. Every delete of this script goes through here, so the
# script never removes a container that another project or run owns.
delete_server() { # $1 = name, $2 = owner token
  if [ -n "$1" ] && list_servers | awk -F '\t' -v n="$1" -v o="$2" '$1 == n && $3 == o { f = 1 } END { exit !f }'; then
    container delete --force "$1" > /dev/null 2>&1
  fi
}

# Removes the temporary directory $1 of a run (it holds a test key), only
# when its name has the form that this script gives it.
remove_run_dir() { # $1 = directory
  case ${1##*/} in
    telemetry-test-postgres.??????) if [ -d "$1" ]; then rm -rf "$1"; fi ;;
  esac
}

remove_stale() {
  list_servers | while IFS="$(printf '\t')" read -r stale _ stale_owner stale_dir; do
    if owner_gone "$stale_owner"; then
      say "removing $stale: its owner process is gone"
      delete_server "$stale" "$stale_owner" || true
      remove_run_dir "$stale_dir"
    fi
  done
}

# Waits until fewer than $max_servers servers of this project run. Two
# runs that start at the same time can both pass the count, so the limit
# can be passed by a few servers for a short time.
wait_for_place() {
  waited=0
  while [ "$(list_servers | grep -c .)" -ge "$max_servers" ]; do
    [ "$waited" -eq 0 ] && say "waiting: $max_servers test servers of this project run"
    waited=$((waited + 1))
    [ "$waited" -le 900 ] || die "no free test server place after 30 minutes"
    sleep 2
    remove_stale
  done
}

# Removes only what this run made: cmd_local sets `name` and `dir` to empty
# before it sets the trap, so a value from the environment of the caller
# never reaches a delete.
local_exit() {
  status=$?
  trap - EXIT INT TERM HUP
  if [ -n "$name" ]; then
    delete_server "$name" "$owner" ||
      say "could not remove $name; the next run removes it"
  fi
  if [ -n "$dir" ]; then remove_run_dir "$dir"; fi
  exit "$status"
}

cmd_local() {
  [ "${1:-}" = -- ] && shift
  [ $# -gt 0 ] || die "usage: scripts/test-postgres.sh local -- CMD..."
  require_container
  # A nested call uses the server of the outer call while it runs.
  if [ -n "${TEST_POSTGRES_SERVER:-}" ] && [ -n "${TEST_DATABASE_URL:-}" ] &&
    [ "$(list_servers | awk -F '\t' -v n="$TEST_POSTGRES_SERVER" '$1 == n { print $2 }')" = running ]; then
    exec "$@"
  fi
  # SQLx reads the PG* variables when the URL leaves a setting out.
  unset PGHOST PGHOSTADDR PGPORT PGDATABASE PGUSER PGPASSWORD PGPASSFILE PGSERVICE \
    PGSERVICEFILE PGSSLMODE PGSSLROOTCERT PGSSLCERT PGSSLKEY PGOPTIONS PGAPPNAME
  pinned_image
  owner=$(owner_of $$) || die "cannot read the start time of this process"
  remove_stale
  wait_for_place
  worktree=$(printf '%s' "$root" | shasum -a 256 | cut -c 1-8)
  name=
  dir=
  trap local_exit EXIT
  trap 'exit 130' INT
  trap 'exit 143' TERM
  trap 'exit 129' HUP
  dir=$(mktemp -d "${TMPDIR:-/tmp}/telemetry-test-postgres.XXXXXX")
  make_tls "$dir"
  superuser_password=$(openssl rand -hex 24)
  password=$(openssl rand -hex 24)
  log="$dir/run.log"
  attempt=0
  while :; do
    attempt=$((attempt + 1))
    port=$((10000 + $(od -An -N2 -tu2 /dev/urandom | tr -d ' ') % 9000))
    name="telemetry-test-$worktree-$(openssl rand -hex 4)"
    # Speed settings for data that the run throws away.
    if POSTGRES_PASSWORD=$superuser_password container run --detach --progress none --name "$name" \
        --platform linux/arm64 --cpus 2 --memory 1G \
        --label "$label=$owner" --label "$label.dir=$dir" --label "telemetry.worktree=$root" \
        --publish "127.0.0.1:$port:5432" \
        --tmpfs /var/lib/postgresql \
        --volume "$dir:/certs:ro" \
        --env POSTGRES_PASSWORD \
        --entrypoint sh "$image" -c "$(server_command "'*'" \
          -c fsync=off -c synchronous_commit=off -c full_page_writes=off -c max_connections=200)" \
        > "$log" 2>&1; then
      break
    fi
    delete_server "$name" "$owner" || true
    if [ "$attempt" -ge 20 ] || ! grep -qi 'in use' "$log"; then
      cat "$log" >&2
      name=
      die "container run failed"
    fi
  done
  rm -f "$log"
  wait_inside container "$name"
  # The published port must answer on the host too.
  ready=0
  for _ in $(seq 1 60); do
    if nc -z -w 2 127.0.0.1 "$port" 2> /dev/null; then
      ready=1
      break
    fi
    sleep 0.5
  done
  [ "$ready" -eq 1 ] || die "the port 127.0.0.1:$port of $name does not answer"
  create_owner container "$name"
  say "server $name ready on 127.0.0.1:$port"
  TEST_DATABASE_URL="postgresql://neondb_owner:$password@localhost:$port/neondb?sslrootcert=$dir/ca.crt"
  TEST_POSTGRES_SERVER=$name
  export TEST_DATABASE_URL TEST_POSTGRES_SERVER
  # No `exec`: this process owns the server, and the trap removes it.
  "$@"
}

cmd_setup() {
  require_container
  say "Apple container $version"
  pinned_image
  # Only the arm64 image: a pull of every platform takes minutes.
  container image pull --platform linux/arm64 --progress none "$image" > /dev/null
  say "image ready: $image"
}

mode=${1:-}
[ $# -gt 0 ] && shift
case $mode in
  setup) cmd_setup ;;
  local) cmd_local "$@" ;;
  ci) cmd_ci ;;
  *) die "usage: scripts/test-postgres.sh setup | local -- CMD... | ci" ;;
esac
