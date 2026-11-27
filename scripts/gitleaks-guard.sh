#!/bin/sh
# Checks the two premises of ADR 0011 before the Gitleaks secret scan runs
# (mise.toml, task checks). Gitleaks 8.30.1 contains a Go brotli decoder
# with CVE-2026-111497 (CISA KEV). The decoder runs only when Gitleaks
# scans into archives. ADR 0011 accepts the risk only while both premises
# hold:
#
#  1. No tracked file holds the archive depth flag of Gitleaks. The flag is
#     the only way to enable archive scanning: no config key and no
#     environment variable sets it. The default depth 0 skips each archive
#     before the decoder opens it. The ADR is the only allowed place of the
#     flag name. The check finds the literal flag name only; ADR 0011
#     lists the known limits.
#  2. The Gitleaks binary that runs is version 8.30.1. Another version can
#     reach the decoder in another way (for example, a content check of
#     each archive-like file name), so a version change needs a new reading
#     of the source code and a change of ADR 0011.
#
# The script stops with exit code 1 at the first broken premise or error.
set -eu

adr='docs/adr/0011-gitleaks-brotli-decoder.md'

# `-a` reads each tracked file as text, also a file that Git or
# .gitattributes treats as binary. The bracket stops this line from
# matching its own pattern. git grep exits 0 on a match, 1 on no match,
# and another code on an error; only 1 is clean.
status=0
git grep -n -a -e 'max-archive[-]depth' -- . ":(exclude)$adr" || status=$?
case $status in
  1) ;;
  0)
    echo "gitleaks-guard: a tracked file names the archive depth flag of Gitleaks (above). ADR 0011 allows no archive scan." >&2
    exit 1
    ;;
  *)
    echo "gitleaks-guard: git grep failed with exit code $status, so the tracked files are not checked." >&2
    exit 1
    ;;
esac

version=$(gitleaks version)
if [ "$version" != '8.30.1' ]; then
  echo "gitleaks-guard: Gitleaks is version $version, not 8.30.1. Read the archive path of the new version and change ADR 0011 before you change this check." >&2
  exit 1
fi
