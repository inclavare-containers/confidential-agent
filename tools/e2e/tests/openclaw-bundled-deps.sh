#!/usr/bin/env bash
set -euo pipefail
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
source <(sed -n '/^normalize_openclaw_bundled_manifest()/,/^install_dingtalk_extension()/p' \
    "$ROOT_DIR/examples/openclaw/files/install-openclaw-runtime.sh" | sed '$d')
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
extension="$work/openclaw/dist/extensions/workboard"
mkdir -p "$extension" "$work/ordinary"
printf '{"name":"ordinary-runtime","version":"1.0.0","main":"index.js"}\n' > "$work/ordinary/package.json"
printf 'module.exports = 42;\n' > "$work/ordinary/index.js"
cat > "$extension/package.json" <<'MANIFEST'
{
  "name": "published-extension",
  "version": "1.0.0",
  "openclaw": {"extensions": ["./index.js"]},
  "dependencies": {"@openclaw/bundled-contract": "workspace:*", "ordinary-runtime": "file:../../../../ordinary"},
  "devDependencies": {"@openclaw/development": "workspace:*"}
}
MANIFEST
cp "$extension/package.json" "$work/original.json"
printf 'module.exports = require("@openclaw/bundled-contract/runtime");\n' > "$extension/index.js"
if normalize_openclaw_bundled_manifest "$extension/package.json" "$work/openclaw" > "$work/rejection.log" 2>&1; then
    echo 'unbundled workspace runtime dependency was accepted' >&2
    exit 1
fi
cmp "$work/original.json" "$extension/package.json"
grep -q 'still references workspace dependency' "$work/rejection.log"
printf 'module.exports = require("ordinary-runtime");\n' > "$extension/index.js"
normalize_openclaw_bundled_manifest "$extension/package.json" "$work/openclaw"
(
    cd "$extension"
    npm install --offline --omit=dev --ignore-scripts --no-audit --no-fund > "$work/npm.log" 2>&1
    node -e 'if (require("./index.js") !== 42) process.exit(1)'
)
node - "$extension/package.json" <<'NODE'
const manifest = require(process.argv[2]);
if (manifest.dependencies['@openclaw/bundled-contract'] !== undefined
    || !manifest.dependencies['ordinary-runtime'] || manifest.devDependencies) process.exit(1);
NODE
printf '%s\n' 'OpenClaw bundled workspace metadata and required runtime dependencies: PASS'
