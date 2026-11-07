#!/bin/sh
# Fetch the acceptance-fixture packages into an ignored cache directory.
#
# The tests are offline by default; the packages below are pinned tarballs verified against
# `fixtures.sha256`. Fetch once, then point `PI_RS_PKG_FIXTURES` at the directory:
#
#   crates/pi-napi/tests/fetch-fixtures.sh
#   PI_RS_PKG_FIXTURES="$PWD/target/pi-napi-fixtures" PI_RS_KEYRING_TEST=1 \
#     cargo test -p pi-napi --test keyring -- --ignored
set -eu

here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
out=${PI_RS_PKG_FIXTURES:-$here/../../../target/pi-napi-fixtures}
mkdir -p "$out"

fetch() {
  url=$1
  file=$2
  if [ -f "$out/$file" ]; then
    return
  fi
  echo "fetching $file"
  curl -sSL -o "$out/$file" "$url"
}

fetch "https://registry.npmjs.org/@napi-rs/keyring/-/keyring-2.1.2.tgz" keyring-2.1.2.tgz
fetch "https://registry.npmjs.org/@napi-rs/keyring-linux-x64-gnu/-/keyring-linux-x64-gnu-2.1.2.tgz" keyring-linux-x64-gnu-2.1.2.tgz
fetch "https://github.com/WiseLibs/better-sqlite3/releases/download/v11.10.0/better-sqlite3-v11.10.0-node-v127-linux-x64.tar.gz" better-sqlite3-v11.10.0-node-v127-linux-x64.tar.gz

# Verify before extracting anything.
(cd "$out" && sha256sum -c "$here/fixtures.sha256")

rm -rf "$out/keyring" "$out/better-sqlite3"
mkdir -p "$out/keyring" "$out/better-sqlite3/node_modules/better-sqlite3/build/Release"
tar xzf "$out/keyring-2.1.2.tgz" -C "$out/keyring" --strip-components=1
tar xzf "$out/keyring-linux-x64-gnu-2.1.2.tgz" -C "$out/keyring" --strip-components=1
# The prebuilt binary is the only thing needed to prove `require()` reaches `dlopen`.
tar xzf "$out/better-sqlite3-v11.10.0-node-v127-linux-x64.tar.gz" -C "$out/better-sqlite3/node_modules/better-sqlite3"
echo "fixtures ready in $out"
