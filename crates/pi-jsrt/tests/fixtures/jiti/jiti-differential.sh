#!/usr/bin/env bash
# Differential check of the embedded jiti against npm jiti 2.7.0 under Node.
#
#   jiti-differential.sh <jiti-2.7.0 package dir> [pi-jsrt-node binary]
#
# Copies the fixtures to a temp dir, runs run.mjs under `node` (with the npm package linked as
# node_modules/jiti) and under pi-jsrt (whose builtin `jiti` beats node_modules), and diffs both
# outputs against expected.json (the recorded Node output). Exit status 0 means all three agree.
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
jiti_dir="${1:?usage: jiti-differential.sh <jiti-2.7.0 package dir> [pi-jsrt-node]}"
pi_node="${2:-$here/../../../../../target/debug/pi-jsrt-node}"
version="$(node -p "require('$jiti_dir/package.json').version")"
[ "$version" = "2.7.0" ] || { echo "expected jiti 2.7.0, got $version" >&2; exit 2; }
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
cp -r "$here/." "$work/"
mkdir -p "$work/node_modules"
ln -s "$(cd "$jiti_dir" && pwd)" "$work/node_modules/jiti"
(cd "$work" && JITI_FS_CACHE=false node run.mjs) > "$work/node.json"
(cd "$work" && "$pi_node" run.mjs) > "$work/pi-jsrt.json"
status=0
diff -u "$here/expected.json" "$work/node.json" || { echo "node output differs from expected.json" >&2; status=1; }
diff -u "$work/node.json" "$work/pi-jsrt.json" || { echo "pi-jsrt output differs from node" >&2; status=1; }
[ "$status" = 0 ] && echo "jiti differential: node (npm jiti $version) and pi-jsrt agree on $(grep -c '^  "' "$work/node.json") scenarios"
exit "$status"
