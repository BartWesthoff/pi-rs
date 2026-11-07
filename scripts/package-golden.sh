#!/usr/bin/env bash
# Regenerate the package smoke golden from TS Pi 1.4.0 under node.
#
#   scripts/package-fixtures.sh    # once, if target/pkg-fixtures is missing
#   scripts/package-golden.sh
#
# Installs the published @earendil-works/pi-coding-agent@1.4.0 (the build of the v1.4.0 tag) into
# target/pkg-fixtures/ts-pi and runs the package harness (crates/pi-coding-agent/tests/packages)
# with that CLI as the host, recording what it registers into
# crates/pi-coding-agent/tests/fixtures/packages/golden/ts-pi-1.4.0.json. The scenarios, sandbox
# and mock model are exactly the ones the pi-rs run uses.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
repo="$(cd "$here/.." && pwd)"
out="${PI_RS_PKG_FIXTURES_DIR:-$repo/target/pkg-fixtures}"
ts_version="1.4.0"
ts_dir="$out/ts-pi"
golden="$repo/crates/pi-coding-agent/tests/fixtures/packages/golden/ts-pi-$ts_version.json"
ui_golden="$repo/crates/pi-coding-agent/tests/fixtures/packages/golden/ts-pi-$ts_version-ui.json"

command -v node >/dev/null || { echo "error: node is required" >&2; exit 1; }
[ -f "$out/manifest.sha256" ] || { echo "error: run scripts/package-fixtures.sh first" >&2; exit 1; }

: >"$out/.npmrc-empty"
: >"$out/.npmrc-global-empty"
export NPM_CONFIG_USERCONFIG="$out/.npmrc-empty"
export NPM_CONFIG_GLOBALCONFIG="$out/.npmrc-global-empty"
export npm_config_registry="https://registry.npmjs.org/"
export npm_config_cache="$out/.npm-cache"

cli="$ts_dir/node_modules/@earendil-works/pi-coding-agent/dist/bundle/cli.js"
if [ ! -f "$cli" ] || [ "$(node -p "require('$ts_dir/node_modules/@earendil-works/pi-coding-agent/package.json').version")" != "$ts_version" ]; then
	echo "==> installing @earendil-works/pi-coding-agent@$ts_version"
	rm -rf "$ts_dir"
	mkdir -p "$ts_dir"
	(cd "$ts_dir" && npm init -y >/dev/null && npm install --ignore-scripts --no-audit --no-fund --save-exact "@earendil-works/pi-coding-agent@$ts_version" >/dev/null)
fi

rm -f "$golden" "$ui_golden"
echo "==> recording golden with TS Pi $ts_version ($(node --version))"
cd "$repo"
PI_RS_PKG_FIXTURES=1 \
	PI_RS_PKG_GOLDEN_WRITE=1 \
	PI_RS_PKG_HOST="$(command -v node) $cli" \
	PI_RS_PKG_GOLDEN_GENERATOR="TS Pi $ts_version (@earendil-works/pi-coding-agent@$ts_version, node $(node --version))" \
	cargo test --locked -p pi-coding-agent --test packages -- "$@"
echo "==> wrote $golden and $ui_golden"
