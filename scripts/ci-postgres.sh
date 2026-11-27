#!/bin/sh
# Starts PostgreSQL 18 with TLS for the database tests of the CI job `ci`
# (STACK.md §3). CI only: it needs Docker and the GitHub Actions runner.
#
# The workflow names the image in the job variable `postgres_image`
# (`mise run deps` checks the pin). The script:
#
# 1. makes a test CA and a server certificate for `localhost` in
#    $RUNNER_TEMP, with a new key on each run;
# 2. starts the server with TLS only (`hostssl` and SCRAM; no plain TCP),
#    listening on the loopback interface of the runner;
# 3. creates the role `neondb_owner` (LOGIN, CREATEROLE, CREATEDB, not a
#    superuser) and the database `neondb` that it owns, as on Neon;
# 4. writes TEST_DATABASE_URL to $GITHUB_ENV, with `sslrootcert` set to the
#    test CA. SQLx adds that CA to the Mozilla roots, and the storage keeps
#    `sslmode=verify-full` (crates/ingest/src/storage/connect.rs).
#
# The password is random for each run and masked in the log. The script
# prints no URL.
set -eu

: "${postgres_image:?Set postgres_image to the pinned PostgreSQL image}"
: "${RUNNER_TEMP:?Run this script on a GitHub Actions runner}"
: "${GITHUB_ENV:?Run this script on a GitHub Actions runner}"

dir="$RUNNER_TEMP/postgres-tls"
rm -rf "$dir"
mkdir -m 0700 "$dir"

# The CA and the server certificate are X.509 v3. rustls (webpki) checks
# the subject alternative name, not the common name, and rejects a CA
# certificate as the server certificate.
openssl req -x509 -new -nodes -days 2 -newkey ec -pkeyopt ec_paramgen_curve:P-256 \
  -subj '/CN=telemetry CI test CA' \
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
rm -f "$dir/ca.key" "$dir/server.csr" "$dir/server.ext"

# Local socket connections (inside the container only) use trust for the
# set-up below. A TCP connection must use TLS and a SCRAM password.
printf '%s\n' \
  'local all all trust' \
  'hostssl all all all scram-sha-256' > "$dir/pg_hba.conf"
chmod 0600 "$dir/server.key"

superuser_password="$(openssl rand -hex 24)"
password="$(openssl rand -hex 24)"
echo "::add-mask::$superuser_password"
echo "::add-mask::$password"

# Host network: the server listens on 127.0.0.1 and ::1 of the runner, so
# `localhost` works for both address families. The custom command copies
# the TLS files with the owner and mode that PostgreSQL requires, and then
# runs the entrypoint of the image.
docker run --detach --name postgres --network host \
  --env POSTGRES_PASSWORD="$superuser_password" \
  --volume "$dir:/certs:ro" \
  --entrypoint sh \
  "$postgres_image" -c '
    install -d -o postgres -g postgres -m 0700 /tls
    install -o postgres -g postgres -m 0600 /certs/server.key /tls/server.key
    install -o postgres -g postgres -m 0644 /certs/server.crt /certs/pg_hba.conf /tls/
    exec docker-entrypoint.sh postgres \
      -c listen_addresses=localhost \
      -c ssl=on \
      -c ssl_cert_file=/tls/server.crt \
      -c ssl_key_file=/tls/server.key \
      -c hba_file=/tls/pg_hba.conf' > /dev/null

# The entrypoint first starts a temporary server without TCP for its
# set-up, and then the real one. Wait until the real server accepts a TLS
# session on TCP.
ready=0
for _ in $(seq 1 60); do
  if PGPASSWORD="$superuser_password" docker exec --env PGPASSWORD postgres \
      psql "host=localhost user=postgres dbname=postgres sslmode=require" -tAc 'SELECT 1' > /dev/null 2>&1; then
    ready=1
    break
  fi
  sleep 1
done
if [ "$ready" -ne 1 ]; then
  echo "ci-postgres: the server did not start" >&2
  docker logs postgres >&2 || true
  exit 1
fi

docker exec --interactive postgres psql -v ON_ERROR_STOP=1 -v password="$password" \
  -U postgres -d postgres -q -tA <<'EOF'
CREATE ROLE neondb_owner LOGIN CREATEROLE CREATEDB NOSUPERUSER PASSWORD :'password';
CREATE DATABASE neondb OWNER neondb_owner;
SELECT 'server ' || version();
EOF

echo "TEST_DATABASE_URL=postgresql://neondb_owner:$password@localhost:5432/neondb?sslrootcert=$dir/ca.crt" >> "$GITHUB_ENV"
echo "ci-postgres: PostgreSQL is ready with TLS; TEST_DATABASE_URL is set"
