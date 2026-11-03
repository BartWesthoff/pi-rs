#!/usr/bin/env bash
# Fetch the pinned third-party Pi packages used by the package smoke harness
# (crates/pi-coding-agent/tests/packages/) and install them into an agent-dir layout.
#
#   scripts/package-fixtures.sh            fetch, verify against the checked-in pins, install
#   scripts/package-fixtures.sh --record   also rewrite the checked-in pins (lockfile + sha256 list)
#
# This is the only step that needs the network (npm registry and GitHub). Afterwards the harness
# runs offline: `PI_RS_PKG_FIXTURES=1 cargo test -p pi-coding-agent --test packages`.
#
# Output (default target/pkg-fixtures, override with PI_RS_PKG_FIXTURES_DIR):
#   tarballs/          the exact upstream package tarballs (`npm pack`, `git archive`)
#   agent/npm/         <agentDir>/npm as TS Pi lays it out (package.json, lockfile, node_modules)
#   agent/git/...      <agentDir>/git/github.com/DietrichGebert/ponytail
#   manifest.sha256    sha256 of every tarball and of the npm root's package.json and lockfile
#
# Pins live in crates/pi-coding-agent/tests/fixtures/packages/:
#   npm/package.json        exact versions of the seven npm packages
#   npm/package-lock.json   the full dependency tree, with registry integrity hashes
#   tarballs.sha256         sha256 of each package tarball
#
# Like TS Pi (`npm install --omit=dev --legacy-peer-deps`), peer dependencies are not installed:
# packages get `@earendil-works/*` from the host.
#
# Packages are installed with `--ignore-scripts`: lifecycle scripts would run third-party code
# against the real home directory (context-mode's postinstall edits other agents' settings). The
# only effect on the tree is that better-sqlite3 has no compiled addon, which pi-rs cannot load in
# any case (V8 ABI); context-mode uses node:sqlite on Node >= 22.5 and on pi-rs.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
repo="$(cd "$here/.." && pwd)"
pins="$repo/crates/pi-coding-agent/tests/fixtures/packages"
out="${PI_RS_PKG_FIXTURES_DIR:-$repo/target/pkg-fixtures}"

record=0
case "${1:-}" in
	"") ;;
	--record) record=1 ;;
	*) echo "usage: $0 [--record]" >&2; exit 2 ;;
esac

ponytail_url="https://github.com/DietrichGebert/ponytail"
ponytail_commit="96608e078b2d5ea607c5c3b8b5a54d1c1fba6846"
ponytail_tar="ponytail-${ponytail_commit:0:7}.tar"

for tool in npm git sha256sum openssl tar; do
	command -v "$tool" >/dev/null || { echo "error: $tool is required" >&2; exit 1; }
done

# Public registry only; never the user's npmrc (it may carry tokens) or npm cache.
export NPM_CONFIG_USERCONFIG="$out/.npmrc-empty"
export NPM_CONFIG_GLOBALCONFIG="$out/.npmrc-global-empty"
export npm_config_registry="https://registry.npmjs.org/"
export npm_config_cache="$out/.npm-cache"
export npm_config_audit=false
export npm_config_fund=false
export npm_config_update_notifier=false

mkdir -p "$out"
: >"$out/.npmrc-empty"; : >"$out/.npmrc-global-empty"
work="$(mktemp -d "$out/.work.XXXXXX")"
trap 'rm -rf "$work"' EXIT

# 1. Lockfile: recorded once, then reused so the transitive tree is pinned too.
if [ "$record" = 1 ] || [ ! -f "$pins/npm/package-lock.json" ]; then
	echo "==> resolving dependency tree"
	mkdir -p "$work/lock"
	cp "$pins/npm/package.json" "$work/lock/"
	(cd "$work/lock" && npm install --package-lock-only --ignore-scripts --omit=dev --legacy-peer-deps >/dev/null)
	cp "$work/lock/package-lock.json" "$pins/npm/package-lock.json"
fi

# 2. Package tarballs.
echo "==> fetching package tarballs"
rm -rf "$out/tarballs"
mkdir -p "$out/tarballs"
specs=$(node -e 'const d=require(process.argv[1]).dependencies;for(const[n,v]of Object.entries(d))console.log(n+"@"+v)' "$pins/npm/package.json")
for spec in $specs; do
	(cd "$out/tarballs" && npm pack "$spec" --silent >/dev/null)
done
git clone --quiet --filter=blob:none --no-checkout "$ponytail_url" "$work/ponytail"
git -C "$work/ponytail" archive --format=tar --prefix=ponytail/ "$ponytail_commit" >"$out/tarballs/$ponytail_tar"

# Each npm tarball must be the one the lockfile pins (sha512 integrity).
for spec in $specs; do
	name="${spec%@*}"
	file="$(echo "${name#@}" | tr / -)-${spec##*@}.tgz"
	want=$(node -e 'const l=require(process.argv[1]);console.log(l.packages["node_modules/"+process.argv[2]].integrity)' "$pins/npm/package-lock.json" "$name")
	got="sha512-$(openssl dgst -sha512 -binary "$out/tarballs/$file" | openssl base64 -A)"
	[ "$want" = "$got" ] || { echo "error: $file does not match the lockfile integrity" >&2; exit 1; }
done

(cd "$out/tarballs" && sha256sum -- * | sort -k2) >"$work/tarballs.sha256"
if [ "$record" = 1 ] || [ ! -f "$pins/tarballs.sha256" ]; then
	cp "$work/tarballs.sha256" "$pins/tarballs.sha256"
fi
if ! diff -u "$pins/tarballs.sha256" "$work/tarballs.sha256"; then
	echo "error: fetched tarballs differ from tests/fixtures/packages/tarballs.sha256" >&2
	exit 1
fi

# 3. Agent-dir layout, installed offline from the npm cache filled above.
echo "==> installing into $out/agent"
rm -rf "$out/agent"
mkdir -p "$out/agent/npm" "$out/agent/git/github.com/DietrichGebert"
cp "$pins/npm/package.json" "$pins/npm/package-lock.json" "$out/agent/npm/"
(cd "$out/agent/npm" && npm ci --omit=dev --legacy-peer-deps --ignore-scripts --no-audit --no-fund >/dev/null)
tar -xf "$out/tarballs/$ponytail_tar" -C "$out/agent/git/github.com/DietrichGebert"

{
	(cd "$out/tarballs" && sha256sum -- * | sed 's|  |  tarballs/|')
	(cd "$out/agent" && sha256sum npm/package.json npm/package-lock.json | sed 's|  |  agent/|')
} | sort -k2 >"$out/manifest.sha256"

echo "==> done: $out ($(du -sh "$out/agent" | cut -f1) installed)"
