#!/bin/sh
# Builds the web image (webapp/Dockerfile) and checks the running
# container (STACK.md section 3, task image-check). The CI job image-check
# runs it on linux/amd64 after scripts/image-check.sh. It needs Docker,
# curl, jq, and gzip. It pushes no image and uses no secret.
#
# The container gets the variables of a Railway start: PORT, a commit SHA
# in RAILWAY_GIT_COMMIT_SHA, WEB_PUBLIC_ORIGIN, a DATABASE_URL with a dummy
# password, and a WEB_PASSWORD_HASH that the `hash-password` command of
# the image makes from a test-only password (the server checks the hash
# at its start, ADR 0012). The log must show neither the dummy password
# nor the hash. No database answers. The checks:
#
#  1. The server is PID 1 and runs as user and group 10001 (real,
#     effective, saved, and file system), with no permitted and no
#     effective capabilities. The script prints the supplementary groups,
#     the bounding and ambient capabilities, and NoNewPrivs, but does not
#     check them: the image has no setpriv step, so Docker sets them.
#     The server binary links only glibc and libgcc_s
#     (docs/security/image-advisories.md, "Web image").
#  2. /health answers 200 with the JSON status ok, the version of
#     RAILWAY_GIT_COMMIT_SHA, and Cache-Control no-store, without a
#     database.
#  3. Each security header of the server has its value on /, on /health,
#     on an unknown /api/ path, and on an asset.
#  4. An unknown /api/v1/ path answers 404 with application/problem+json,
#     the code not_found, and Cache-Control no-store.
#  5. A missing asset answers 404, not the index.html body.
#  6. / and a deep link answer 200 with text/html, Cache-Control no-store,
#     and the version meta with the commit SHA in place of the
#     placeholder. /robots.txt disallows every path.
#  7. An asset answers with Content-Encoding gzip and the immutable
#     Cache-Control when the client accepts gzip, and without
#     Content-Encoding when the client does not.
#  8. Each log line is a JSON value, and the log holds neither dummy
#     secret.
#  9. On SIGTERM (docker stop), the server ends with exit code 0 within
#     the 15 s of the Railway draining time.
# 10. A second container with an init as PID 1 (docker run --init), as on
#     Railway: the server is a child of the init, /health answers 200, and
#     SIGTERM through the init ends the server within 15 s with exit
#     code 0. The init of Docker (tini) is not the init of Railway
#     (podman-init).
# 11. The compressed size of the image (docker save | gzip) is at most the
#     budget of 100 MB (STACK.md section 4).
#
# The container publishes its port on 127.0.0.1:18081 of the host, so the
# port must be free.
set -eu

name=web-image-check
all_names="web-image-check web-image-check-init"
tag=telemetry-web:check
host_port=18081
container_port=8080
base="http://127.0.0.1:$host_port"
# A fixed commit SHA of 40 lowercase hex digits; only the format matters.
sha=0123456789abcdef0123456789abcdef01234567
database_password=image-check-web-password
# A test-only password, never a real one. The hash comes after the build.
test_password='image-check-web test-only password'
password_hash=
budget_bytes=100000000
work="$(mktemp -d)"

show_log() {
  echo "image-check-web: the log of the container:" >&2
  docker logs "$name" >&2 2>&1 || true
}

fail() {
  echo "image-check-web: FAIL: $1" >&2
  show_log
  logs_shown=1
  exit 1
}

# An unexpected error (set -e) also shows the log of the container.
cleanup() {
  status=$?
  if [ "$status" -ne 0 ] && [ -z "${logs_shown:-}" ]; then
    echo "image-check-web: FAIL: exit status $status" >&2
    show_log
  fi
  for container in $all_names; do
    docker rm -f "$container" >/dev/null 2>&1 || true
  done
  rm -rf "$work"
}
trap cleanup EXIT

# docker exec runs as the USER of the image, 10001.
in_container() {
  docker exec "$name" "$@"
}

start_container() {
  docker run -d --name "$name" "$@" \
    -p "127.0.0.1:$host_port:$container_port" \
    -e PORT="$container_port" \
    -e RAILWAY_GIT_COMMIT_SHA="$sha" \
    -e DATABASE_URL="postgresql://telemetry_web:$database_password@127.0.0.1:1/neondb" \
    -e WEB_PASSWORD_HASH="$password_hash" \
    -e WEB_PUBLIC_ORIGIN=https://tesla.calmdonut.com \
    "$tag" >/dev/null
}

# Waits until /health answers 200. $1 is the limit in seconds.
wait_for_health() {
  i=0
  until [ "$(curl -s -o /dev/null -w '%{http_code}' "$base/health" || true)" = 200 ]; do
    i=$((i + 1))
    [ "$i" -le "$1" ] || fail "/health does not answer 200 within $1 s"
    sleep 1
  done
}

# Requests a path. Writes the headers to $work/headers and the body to
# $work/body, and prints the status code. Extra arguments go to curl.
request() {
  path="$1"
  shift
  curl -s -D "$work/headers" -o "$work/body" -w '%{http_code}' "$@" "$base$path"
}

# The value of a response header of the last request (the name in any
# letter case), or nothing.
header() {
  tr -d '\r' < "$work/headers" | awk -v n="$1" '
    index($0, ":") { name = tolower(substr($0, 1, index($0, ":") - 1)); if (name == tolower(n)) { v = substr($0, index($0, ":") + 1); sub(/^[ \t]+/, "", v); print v } }'
}

expect_header() {
  value="$(header "$1")"
  [ "$value" = "$2" ] || fail "$3: $1 is '$value', expected '$2'"
}

check_security_headers() {
  expect_header Content-Security-Policy "default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self' data:; font-src 'self'; connect-src 'self'; manifest-src 'self'; base-uri 'none'; form-action 'self'; frame-ancestors 'none'" "$1"
  expect_header Strict-Transport-Security "max-age=31536000" "$1"
  expect_header X-Content-Type-Options nosniff "$1"
  expect_header Referrer-Policy no-referrer "$1"
  expect_header Permissions-Policy "geolocation=(), camera=(), microphone=()" "$1"
  expect_header Cross-Origin-Opener-Policy same-origin "$1"
  expect_header Cross-Origin-Resource-Policy same-origin "$1"
  expect_header X-Robots-Tag noindex "$1"
}

# docker stop sends SIGTERM to PID 1 and SIGKILL after 15 s.
check_sigterm() {
  start="$(date +%s)"
  docker stop -t 15 "$name" >/dev/null
  elapsed=$(($(date +%s) - start))
  code="$(docker inspect -f '{{.State.ExitCode}}' "$name")"
  [ "$code" = 0 ] || fail "exit code $code after SIGTERM, expected 0 (137 is the SIGKILL of docker stop)"
  [ "$elapsed" -lt 15 ] || fail "the shutdown took $elapsed s"
  echo "shutdown after SIGTERM: exit code $code in $elapsed s"
}

check_log() {
  docker logs "$name" > "$work/log" 2>&1
  if grep -q -F -e "$database_password" -e "$password_hash" "$work/log"; then
    fail "the log holds a dummy secret"
  fi
  lines="$(grep -c . "$work/log" || true)"
  [ "$lines" -gt 0 ] || fail "the log is empty"
  grep . "$work/log" | jq -e -R 'fromjson? // error("not JSON")' > /dev/null ||
    fail "a log line is not JSON"
  echo "log: $lines JSON lines, no dummy secret"
}

echo "image-check-web: build"
docker build --pull -f webapp/Dockerfile -t "$tag" .
# The hash of the test-only password, made by the server binary of the
# image (README, "Web password").
password_hash="$(printf '%s' "$test_password" |
  docker run --rm -i --entrypoint /usr/local/bin/telemetry-web-server "$tag" hash-password)"
case "$password_hash" in
  '$argon2id$v=19$m=65536,t=3,p=1$'*) ;;
  *) fail "hash-password of the image gave no Argon2id hash" ;;
esac

echo "image-check-web: start"
started_at="$(date +%s)"
start_container
wait_for_health 30
echo "/health 200 after about $(($(date +%s) - started_at)) s"

echo "image-check-web: 1. PID 1, user, capabilities, libraries"
[ "$(in_container cat /proc/1/cmdline | tr '\0' ' ')" = "/usr/local/bin/telemetry-web-server serve " ] ||
  fail "PID 1 is not telemetry-web-server serve"
status="$(in_container cat /proc/1/status)"
for field in Uid Gid; do
  value="$(printf '%s\n' "$status" | awk -v f="$field:" '$1 == f { print $2, $3, $4, $5 }')"
  [ "$value" = "10001 10001 10001 10001" ] || fail "the server has $field $value, not 10001"
done
for field in CapPrm CapEff; do
  value="$(printf '%s\n' "$status" | awk -v f="$field:" '$1 == f { print $2 }')"
  [ "$value" = 0000000000000000 ] || fail "the server has $field $value"
done
echo "server: Uid and Gid 10001, CapPrm and CapEff 0; not checked: $(printf '%s\n' "$status" | awk '$1 == "Groups:" || $1 == "CapBnd:" || $1 == "CapAmb:" || $1 == "NoNewPrivs:" { printf "%s ", $0 }')"
libraries="$(in_container ldd /usr/local/bin/telemetry-web-server | awk '{ print $1 }' | sed 's|^.*/||' | sort | tr '\n' ' ')"
echo "telemetry-web-server links: $libraries"
for library in $libraries; do
  case "$library" in
    linux-vdso.so.1 | libc.so.6 | libm.so.6 | libgcc_s.so.1 | ld-linux-x86-64.so.2) ;;
    *) fail "the server links $library, which docs/security/image-advisories.md does not list" ;;
  esac
done

echo "image-check-web: 2. /health"
[ "$(request /health)" = 200 ] || fail "/health does not answer 200"
jq -e --arg sha "$sha" '.status == "ok" and .version == $sha' "$work/body" > /dev/null ||
  fail "/health body: $(head -c 200 "$work/body")"
expect_header Cache-Control no-store /health
check_security_headers /health

echo "image-check-web: 3 and 6. / and a deep link"
for path in / /battery; do
  [ "$(request "$path")" = 200 ] || fail "$path does not answer 200"
  expect_header Content-Type "text/html; charset=utf-8" "$path"
  expect_header Cache-Control no-store "$path"
  check_security_headers "$path"
  # Vite writes the element as <meta ... />.
  grep -q "<meta name=\"telemetry-version\" content=\"$sha\"" "$work/body" ||
    fail "$path has no version meta with the commit SHA"
  if grep -q __TELEMETRY_VERSION__ "$work/body"; then
    fail "$path holds the version placeholder"
  fi
done
request / > /dev/null
asset="$(grep -o '/assets/[^"]*\.js' "$work/body" | head -1)"
[ -n "$asset" ] || fail "index.html names no JavaScript asset"
[ "$(request /robots.txt)" = 200 ] || fail "/robots.txt does not answer 200"
[ "$(cat "$work/body")" = "$(printf 'User-agent: *\nDisallow: /')" ] ||
  fail "/robots.txt does not disallow every path"

echo "image-check-web: 3 and 4. unknown /api/ path"
[ "$(request /api/v1/nope)" = 404 ] || fail "/api/v1/nope does not answer 404"
expect_header Content-Type application/problem+json /api/v1/nope
expect_header Cache-Control no-store /api/v1/nope
check_security_headers /api/v1/nope
jq -e '.code == "not_found" and .status == 404' "$work/body" > /dev/null ||
  fail "/api/v1/nope body: $(head -c 200 "$work/body")"

echo "image-check-web: 4. the session API without a cookie"
[ "$(request /api/v1/session)" = 401 ] || fail "/api/v1/session does not answer 401"
expect_header Content-Type application/problem+json /api/v1/session
jq -e '.code == "unauthenticated" and .status == 401' "$work/body" > /dev/null ||
  fail "/api/v1/session body: $(head -c 200 "$work/body")"

echo "image-check-web: 5. missing asset"
[ "$(request /assets/nope.js)" = 404 ] || fail "/assets/nope.js does not answer 404"
if grep -q 'telemetry-version' "$work/body"; then
  fail "/assets/nope.js answers with the index.html body"
fi

echo "image-check-web: 3 and 7. asset $asset"
[ "$(request "$asset" -H 'Accept-Encoding: gzip')" = 200 ] || fail "$asset does not answer 200"
expect_header Content-Encoding gzip "$asset with gzip"
expect_header Cache-Control "public, max-age=31536000, immutable" "$asset"
check_security_headers "$asset"
gzip -t "$work/body" || fail "$asset with Content-Encoding gzip is not gzip data"
[ "$(request "$asset")" = 200 ] || fail "$asset without gzip does not answer 200"
[ -z "$(header Content-Encoding)" ] || fail "$asset has a Content-Encoding without Accept-Encoding"
echo "asset: gzip when accepted, plain otherwise"

echo "image-check-web: 8. log"
check_log

echo "image-check-web: 9. SIGTERM"
check_sigterm
check_log

echo "image-check-web: 10. a container with an init as PID 1 (docker run --init)"
name=web-image-check-init
start_container --init
wait_for_health 30
server="$(in_container sh -c 'for d in /proc/[0-9]*; do case "$(tr "\0" " " < "$d/cmdline" 2>/dev/null)" in "/usr/local/bin/telemetry-web-server serve ") echo "${d#/proc/}";; esac; done')"
[ -n "$server" ] || fail "the server does not run under the init"
[ "$server" != 1 ] || fail "the server is PID 1 under an init"
[ "$(in_container awk '/^PPid:/ { print $2 }' "/proc/$server/status")" = 1 ] ||
  fail "the parent of the server is not the init"
echo "the server is PID $server, a child of PID 1"
check_sigterm
check_log

echo "image-check-web: 11. image size"
size="$(docker save "$tag" | gzip -c | wc -c | tr -d ' ')"
echo "compressed image size: $size bytes (budget $budget_bytes)"
[ "$size" -le "$budget_bytes" ] || fail "the compressed image is $size bytes, above the budget of $budget_bytes"

echo "image-check-web: PASS"
