#!/bin/sh
# Checks the initial bundle size of the built SPA (design §9, budget
# "Initial bundle"): the gzip size of each JS and CSS file that index.html
# references, summed per type.
#
# Usage: scripts/bundle-size.sh <dist directory>   (default: web/dist)
#
# Limits: JS 200 KB and CSS 30 KB after gzip (1 KB = 1000 bytes). The
# script measures `gzip -9 -n` of each file, the same compression as the
# precompressed .gz files that the server sends. Files that the SPA loads
# later (dynamic imports) are not part of the initial bundle.
# The script fails when index.html references no JS file, when a
# referenced file is missing, or when a sum is over its limit.
set -eu

dist=${1:-web/dist}
js_limit=200000
css_limit=30000

index="$dist/index.html"
if [ ! -f "$index" ]; then
  echo "bundle-size: $index does not exist. Run the build first." >&2
  exit 1
fi

# The src and href values that point into /assets/. Vite writes each
# reference on one line.
refs=$(grep -oE '(src|href)="/assets/[^"]+"' "$index" | sed -E 's/^(src|href)="\/(.*)"$/\2/' | sort -u)

js_total=0
css_total=0
js_count=0
css_count=0
for ref in $refs; do
  file="$dist/$ref"
  if [ ! -f "$file" ]; then
    echo "bundle-size: index.html references $ref, but $file does not exist." >&2
    exit 1
  fi
  size=$(gzip -9 -n -c "$file" | wc -c | tr -d ' ')
  case "$ref" in
    *.js)
      js_total=$((js_total + size))
      js_count=$((js_count + 1))
      echo "bundle-size: $ref $size bytes gzip"
      ;;
    *.css)
      css_total=$((css_total + size))
      css_count=$((css_count + 1))
      echo "bundle-size: $ref $size bytes gzip"
      ;;
    *) ;;
  esac
done

if [ "$js_count" -eq 0 ]; then
  echo "bundle-size: index.html references no JS file under /assets/." >&2
  exit 1
fi

echo "bundle-size: initial JS $js_total bytes gzip in $js_count files (limit $js_limit)"
echo "bundle-size: initial CSS $css_total bytes gzip in $css_count files (limit $css_limit)"

status=0
if [ "$js_total" -gt "$js_limit" ]; then
  echo "bundle-size: the initial JS is over the limit." >&2
  status=1
fi
if [ "$css_total" -gt "$css_limit" ]; then
  echo "bundle-size: the initial CSS is over the limit." >&2
  status=1
fi
exit "$status"
