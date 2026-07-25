#!/usr/bin/env bash
# What `composer require typedduck/steins` actually downloads.
#
# Packagist's dist archive is `git archive`, so this is not a proxy for the
# published package — it IS the published package. Two ways to get it wrong,
# both silent and both caught here:
#
#   an over-broad export-ignore drops LICENSE, and the distribution stops
#   satisfying Apache-2.0 §4(a);
#   an under-broad one ships crates/, and every PHP project installing a linter
#   pulls a Rust workspace.
#
#     composer/tests/package-contents.sh [tree-ish]
set -euo pipefail

tree="${1:-HEAD}"
contents="$(git archive "$tree" | tar t)"

fail=0

require() {
  if printf '%s\n' "$contents" | grep -qx "$1"; then
    echo "  ok      ships $1"
  else
    echo "  FAIL    missing $1"
    fail=1
  fi
}

refuse() {
  if printf '%s\n' "$contents" | grep -q "^$1"; then
    echo "  FAIL    ships $1, which must be export-ignored"
    fail=1
  else
    echo "  ok      excludes $1"
  fi
}

echo "Files the distribution is not allowed to omit"
# Apache-2.0 §4(a) attaches to a distribution, and this is one.
require "LICENSE"
# The binary the shim fetches is statically linked against MIT/BSD/ISC
# dependencies whose notices must accompany it.
require "THIRD-PARTY-LICENSES.md"
require "README.md"
require "composer.json"
require "composer/bin/steins"
require "composer/src/internal.php"

echo
echo "Files no PHP project should be made to download"
refuse "crates/"
refuse "xtask/"
refuse "harness/"
refuse "spike/"
refuse "docs/"
refuse "Cargo.toml"
refuse "Cargo.lock"
refuse ".github/"
refuse "composer/tests/"
# The release page already carries each version's notes verbatim.
refuse "CHANGELOG.md"

echo
size="$(git archive "$tree" --format=tar.gz | wc -c | tr -d ' ')"
echo "Published archive: $(printf '%s\n' "$contents" | grep -cv '/$') files, ${size} bytes gzipped"

# The shim is executed straight out of vendor/. Composer sets the bit on install
# either way, but a 644 file in the tree shows up as a modified vendor/ tree
# afterwards, which is a confusing thing to hand someone.
mode="$(git ls-tree "$tree" -- composer/bin/steins | awk '{print $1}')"
if [ "$mode" = "100755" ]; then
  echo "  ok      composer/bin/steins is committed executable"
else
  echo "  FAIL    composer/bin/steins is committed $mode, expected 100755"
  fail=1
fi

exit "$fail"
