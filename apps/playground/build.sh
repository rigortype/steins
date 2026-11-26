#!/bin/sh
# Build the playground's wasm module and place it beside index.html for local
# development. Deployment (issue #58) fetches a published artifact instead.
#
# The second half builds the optional PHP engine (issue #64 S2): php-src
# compiled to WebAssembly by php-wasm-builder at an EXACT commit, plus the
# sidecar runner, under apps/playground/vendor/. Both are build products and
# gitignored; the license texts the engine obliges are tracked in
# apps/playground/vendor-licenses/ and are checked against the builder's
# checkout on every fresh build.
#
# The engine build needs Docker with buildx. php-wasm-builder publishes no
# prebuilt binaries: its Dockerfile pins Emscripten and every library, and the
# build fetches php-src at a verified commit.
set -eu
cd "$(dirname "$0")/../.."

cargo build -p steins-wasm --target wasm32-unknown-unknown --release
cp target/wasm32-unknown-unknown/release/steins_wasm.wasm apps/playground/steins_wasm.wasm
ls -la apps/playground/steins_wasm.wasm

# ── the PHP engine (issue #64) ────────────────────────────────────────────────
# Pinned by commit, not by tag: a tag can move, and the commit is what decides
# every byte of the Dockerfile below.
BUILDER_REPO=https://github.com/lyrixx/php-wasm-builder.git
BUILDER_COMMIT=59301bdddfd3d7387e8d7ae3b5e73ff633779a70 # v1.0.0
# The builder knows this version's php-src commit and verifies it.
PHP_VERSION=8.5.13
# What the engine defines is what the absence family calls defined, so a
# missing extension is a `call.undefined-function` on every call into it. The
# set is every extension whose linked libraries are php-src's own or carry a
# permissive notice tracked in vendor-licenses/: libxml2 (dom, simplexml, xml,
# xmlreader, xmlwriter), Oniguruma (mbstring), SQLite (public domain) and zlib.
# Left out: iconv (GNU libiconv is LGPL), openssl (Apache-2.0 NOTICE), gd, zip,
# sodium and intl (more libraries to attribute, and intl alone is +14 MB gz).
EXTENSIONS="bcmath calendar ctype dom exif fileinfo filter libxml mbstring pdo pdo_sqlite phar session simplexml sqlite3 tokenizer xml xmlreader xmlwriter zlib"
# The module's memory is fixed at link time (no growth). runner.php sets
# `memory_limit = 256M` so that a fold's outcome does not depend on the host;
# below about 300 MB the heap, not that limit, would end a large fold.
MEMORY=320mb
# The library versions the tracked notices were taken from. A builder bump that
# moves one has to update its notice in the same commit.
PINNED_LIBS="EMSDK_VERSION=6.0.13 LIBXML_VERSION=2.13.5 ONIGURUMA_VERSION=6.9.10 SQLITE_VERSION=3.54.1 ZLIB_VERSION=1.3.2"
# The builder's outputs, kept under their own names: each .mjs resolves its
# .wasm next to it via import.meta.url.
ENGINE_FILES="php-web.mjs php-web.wasm php-node.mjs php-node.wasm"

VENDOR=apps/playground/vendor
LICENSES=apps/playground/vendor-licenses
STAMP="$VENDOR/php-wasm/.pinned-build"
WANT="$BUILDER_COMMIT php-$PHP_VERSION memory=$MEMORY extensions=$EXTENSIONS"

needed=no
[ -f "$STAMP" ] && [ "$(cat "$STAMP")" = "$WANT" ] || needed=yes
for f in $ENGINE_FILES; do
  [ -f "$VENDOR/php-wasm/$f" ] || needed=yes
done

if [ "$needed" = yes ]; then
  echo "building PHP $PHP_VERSION with php-wasm-builder@$BUILDER_COMMIT"
  command -v docker >/dev/null || { echo "the PHP engine build needs Docker (with buildx)" >&2; exit 1; }
  tmp="$(mktemp -d)"
  trap 'rm -rf "$tmp"' EXIT
  git -C "$tmp" init -q builder
  git -C "$tmp/builder" fetch -q --depth 1 "$BUILDER_REPO" "$BUILDER_COMMIT"
  git -C "$tmp/builder" checkout -q FETCH_HEAD

  # The license texts are tracked, so a builder bump that changes them has to be
  # a deliberate commit, not a silent build product.
  cmp -s "$tmp/builder/LICENSE" "$LICENSES/php-wasm-builder-LICENSE" \
    || { echo "php-wasm-builder LICENSE drifted from $LICENSES/php-wasm-builder-LICENSE" >&2; exit 1; }
  for pin in $PINNED_LIBS; do
    grep -qx "ARG $pin" "$tmp/builder/docker/Dockerfile" \
      || { echo "php-wasm-builder no longer pins $pin: re-take its notice in $LICENSES" >&2; exit 1; }
  done

  # The output stays inside the checkout: bake asks for an fs.write entitlement
  # for any destination outside its own directory.
  (cd "$tmp/builder" && docker buildx bake --progress=plain \
    --set default.args.PHP_VERSION="$PHP_VERSION" \
    --set default.args.EXTENSIONS="$EXTENSIONS" \
    --set default.args.MEMORY="$MEMORY" \
    --set default.output=type=local,dest=./build/engine)

  rm -rf "$VENDOR/php-wasm"
  mkdir -p "$VENDOR/php-wasm"
  for f in $ENGINE_FILES; do
    [ -f "$tmp/builder/build/engine/$f" ] \
      || { echo "php-wasm-builder@$BUILDER_COMMIT did not produce $f" >&2; exit 1; }
    cp "$tmp/builder/build/engine/$f" "$VENDOR/php-wasm/$f"
  done
  printf '%s\n' "$WANT" > "$STAMP"
else
  echo "PHP $PHP_VERSION engine already built at php-wasm-builder@$BUILDER_COMMIT"
fi

# The sidecar runner is COPIED, never forked: the browser engine answers with the
# same `steins_handle` dispatch core the native sidecar embeds (ADR-0066 §2).
mkdir -p "$VENDOR"
cp crates/steins-sidecar/runner.php "$VENDOR/runner.php"
ls -la "$VENDOR/runner.php" "$VENDOR/php-wasm/php-web.wasm"
