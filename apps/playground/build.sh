#!/bin/sh
# Build the playground's wasm module and place it beside index.html for local
# development. Deployment (issue #58) fetches a published artifact instead.
#
# The second half vendors the optional PHP engine (issue #64 S2): php-wasm at an
# EXACT pin, plus the sidecar runner, copied under apps/playground/vendor/. Both
# are build products and gitignored; the license texts they oblige are tracked
# in apps/playground/vendor-licenses/ and are checked against the packed tarball
# on every fresh vendoring.
set -eu
cd "$(dirname "$0")/../.."

cargo build -p steins-wasm --target wasm32-unknown-unknown --release
cp target/wasm32-unknown-unknown/release/steins_wasm.wasm apps/playground/steins_wasm.wasm
ls -la apps/playground/steins_wasm.wasm

# ── php-wasm (issue #64) ──────────────────────────────────────────────────────
# Pinned, not ranged: the esm.sh version-resolution trap is already on record,
# and a vendored copy also removes any future COEP interaction.
PHP_WASM_VERSION=0.1.0
# The 8.5 runtime binary. Its name is baked into the glue modules (both the web
# and the node one name the SAME file), so it keeps the upstream hash name and
# must stay adjacent to them — the glue resolves it via import.meta.url.
PHP_WASM_BINARY=5eec04f740c83548a49d4dfa5f4ad074383cc188.wasm
# Exactly the browser-ready flat ESM graph, plus the Node entry the smoke suite
# drives (`smoke-replay.mjs` runs the very binary the browser gets, which is why
# it is vendored here instead of npm-installed separately). The package's
# PhpWorker.mjs does NOT exist — only a .d.mts stub ships — so the playground
# brings its own worker (php-worker.js).
PHP_WASM_FILES="PhpWeb.mjs PhpBase.mjs webTransactions.mjs OutputBuffer.mjs _Event.mjs fsOps.mjs resolveDependencies.mjs php8.5-web.mjs PhpNode.mjs php8.5-node.mjs $PHP_WASM_BINARY"

VENDOR=apps/playground/vendor
LICENSES=apps/playground/vendor-licenses
STAMP="$VENDOR/php-wasm/.pinned-version"

needed=no
[ -f "$STAMP" ] && [ "$(cat "$STAMP")" = "$PHP_WASM_VERSION" ] || needed=yes
for f in $PHP_WASM_FILES; do
  [ -f "$VENDOR/php-wasm/$f" ] || needed=yes
done

if [ "$needed" = yes ]; then
  echo "vendoring php-wasm@$PHP_WASM_VERSION"
  tmp="$(mktemp -d)"
  (cd "$tmp" && npm pack "php-wasm@$PHP_WASM_VERSION" >/dev/null && tar xzf "php-wasm-$PHP_WASM_VERSION.tgz")

  # The license texts are tracked, so a version bump that changes them has to be
  # a deliberate commit, not a silent build product.
  cmp -s "$tmp/package/LICENSE" "$LICENSES/php-wasm-LICENSE" \
    || { echo "php-wasm LICENSE drifted from $LICENSES/php-wasm-LICENSE" >&2; rm -rf "$tmp"; exit 1; }
  cmp -s "$tmp/package/NOTICE" "$LICENSES/php-wasm-NOTICE" \
    || { echo "php-wasm NOTICE drifted from $LICENSES/php-wasm-NOTICE" >&2; rm -rf "$tmp"; exit 1; }

  mkdir -p "$VENDOR/php-wasm"
  for f in $PHP_WASM_FILES; do
    [ -f "$tmp/package/$f" ] \
      || { echo "php-wasm@$PHP_WASM_VERSION does not ship $f" >&2; rm -rf "$tmp"; exit 1; }
    cp "$tmp/package/$f" "$VENDOR/php-wasm/$f"
  done
  printf '%s\n' "$PHP_WASM_VERSION" > "$STAMP"
  rm -rf "$tmp"
else
  echo "php-wasm@$PHP_WASM_VERSION already vendored"
fi

# The sidecar runner is COPIED, never forked: the browser engine answers with the
# same `steins_handle` dispatch core the native sidecar embeds (ADR-0066 §2).
mkdir -p "$VENDOR"
cp crates/steins-sidecar/runner.php "$VENDOR/runner.php"
ls -la "$VENDOR/runner.php" "$VENDOR/php-wasm/$PHP_WASM_BINARY"
