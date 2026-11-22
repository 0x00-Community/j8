#!/bin/sh
# Copy tests from Node's own suite at the pinned tag into parallel/ or sequential/.
# usage: tests/node/fetch.sh test-path-join.js [test-…]
set -eu
tag=v25.9.0
here=$(cd "$(dirname "$0")" && pwd)
for name in "$@"; do
  for dir in parallel sequential; do
    if curl -fsSL "https://raw.githubusercontent.com/nodejs/node/$tag/test/$dir/$name" -o "$here/$dir/$name.part"; then
      mv "$here/$dir/$name.part" "$here/$dir/$name"
      echo "$dir/$name"
      continue 2
    fi
    rm -f "$here/$dir/$name.part"
  done
  echo "not found at $tag: $name" >&2
  exit 1
done
