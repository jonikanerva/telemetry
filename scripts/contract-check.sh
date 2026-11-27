#!/bin/sh
# Checks that the committed API contract and the committed client agree
# with the server (STACK.md section 3). The script:
#
# 1. exports the OpenAPI document of the server
#    (`telemetry-web-server export-openapi`) into a new scratch directory;
# 2. generates the client from that document into the same scratch
#    directory, with the generation entry of the package
#    telemetry-web-api-client;
# 3. compares the export with contract/openapi.json, and the generated
#    tree with packages/api-client/src/generated (diff -r: changed, added,
#    and deleted files);
# 4. removes the scratch directory, also after a failure.
#
# The script never writes a tracked file or a file of the two compared
# paths; it fails when the git status of these paths changes during the
# run. That guard sees a new write only to a clean, tracked file, as in a
# fresh CI checkout; a file that is already changed or untracked keeps its
# status. Cargo writes its build output to target/, as for each build. A
# difference fails the check: run `mise run contract` and commit the
# result. The script needs Cargo, pnpm with the installed workspace, diff,
# and git.
set -eu

contract=contract/openapi.json
generated=packages/api-client/src/generated

for path in "$contract" "$generated"; do
  if [ ! -e "$path" ]; then
    echo "contract-check: $path is missing. Run mise run contract and commit the result." >&2
    exit 1
  fi
done

paths_status() {
  git status --porcelain --untracked-files=all -- "$contract" "$generated"
}
before="$(paths_status)"

scratch="$(mktemp -d)"
trap 'rm -rf "$scratch"' EXIT
trap 'exit 1' INT TERM

# The paths are absolute: pnpm runs the script in the directory of the
# package.
cargo run --locked --quiet -p telemetry-web-server -- export-openapi "$scratch/openapi.json"
pnpm --filter telemetry-web-api-client run generate -- --input "$scratch/openapi.json" --output "$scratch/generated"

status=0
if ! diff -u "$contract" "$scratch/openapi.json"; then
  echo "contract-check: $contract differs from the export of the server." >&2
  status=1
fi
if ! diff -r -u "$generated" "$scratch/generated"; then
  echo "contract-check: $generated differs from the generated client." >&2
  status=1
fi
if [ "$(paths_status)" != "$before" ]; then
  echo "contract-check: the check changed $contract or $generated in the working tree." >&2
  status=1
fi
if [ "$status" -ne 0 ]; then
  echo "contract-check: FAIL. Run mise run contract and commit the result." >&2
  exit 1
fi
echo "contract-check: the contract and the generated client agree with the server"
