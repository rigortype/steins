#!/usr/bin/env bash
# Install this working tree's Composer package into a scratch project and run it.
#
# The shim resolves its version from Composer metadata and fetches the matching
# GitHub Release, so a commit under test has no release of its own to fetch.
# The fixture closes that gap honestly: take exactly what `git archive` would
# publish, put it in a throwaway repository, and tag it with a version that HAS
# been released. Composer then resolves a real version, and the shim under test
# downloads a real binary — only the packaging origin is local.
#
#     composer/tests/smoke.sh <released-tag> [tree-ish]
#     composer/tests/smoke.sh v0.1.0
set -euo pipefail

tag="${1:?usage: smoke.sh <released-tag> [tree-ish]}"
tree="${2:-HEAD}"
version="${tag#v}"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

echo "Building a package fixture from $tree, tagged $tag"
mkdir -p "$work/pkg" "$work/app"
git archive "$tree" | tar x -C "$work/pkg"
git -C "$work/pkg" init -q .
git -C "$work/pkg" add -A
git -C "$work/pkg" -c user.email=ci@localhost -c user.name=ci commit -qm "package fixture"
git -C "$work/pkg" tag "$tag"

cat > "$work/app/composer.json" <<EOF
{
    "repositories": [{"type": "vcs", "url": "$work/pkg"}],
    "require-dev": {"rigortype/steins": "$version"}
}
EOF

cd "$work/app"
composer install --no-interaction --quiet

fail=0
check() {
  if [ "$2" = "$3" ]; then
    echo "  ok      $1"
  else
    echo "  FAIL    $1: got '$2', expected '$3'"
    fail=1
  fi
}

echo
echo "Cold run — downloads, verifies, execs"
set +e
output="$(./vendor/bin/steins doctor --no-php 2>&1)"
status=$?
set -e
check "exits 0" "$status" "0"
# `doctor` runs no checks and exits 0 even on a degraded environment (ADR-0054
# §10), which is what makes it a liveness test rather than an analysis run. The
# release workflow and the Homebrew formula assert on the same command.
if printf '%s' "$output" | grep -q "sound subset"; then
  echo "  ok      the binary produced its posture report"
else
  echo "  FAIL    no posture report in the output:"
  printf '%s\n' "$output" | sed 's/^/          /'
  fail=1
fi
if printf '%s' "$output" | grep -q "Downloading steins"; then
  echo "  ok      it fetched the binary"
else
  echo "  FAIL    nothing was downloaded, so nothing under test ran"
  fail=1
fi

echo
echo "Warm run — the cached binary, with no route to the network"
set +e
warm="$(ALL_PROXY=http://127.0.0.1:1 HTTPS_PROXY=http://127.0.0.1:1 \
        https_proxy=http://127.0.0.1:1 ./vendor/bin/steins doctor --no-php 2>&1)"
status=$?
set -e
check "exits 0" "$status" "0"
if printf '%s' "$warm" | grep -q "Downloading steins"; then
  echo "  FAIL    it downloaded again instead of reusing the cache"
  fail=1
else
  echo "  ok      no second download"
fi

echo
echo "Exit codes reach the caller (ADR-0050 §7 — CI reads them)"
binary="$(find vendor/rigortype/steins/composer/bin -name steins -type f -not -path '*/bin/steins' | head -1)"
# A non-zero status is the point of this section, so `set -e` has to stand down
# for it — `--version` is not a command steins has, and exits 2 saying so.
set +e
for args in "--version" "doctor --no-php"; do
  # shellcheck disable=SC2086
  "$binary" $args >/dev/null 2>&1; direct=$?
  # shellcheck disable=SC2086
  ./vendor/bin/steins $args >/dev/null 2>&1; shim=$?
  check "steins $args" "$shim" "$direct"
  if [ "$args" = "--version" ] && [ "$direct" = "0" ]; then
    echo "  FAIL    '$args' exits 0, so this comparison proves nothing about non-zero passthrough"
    fail=1
  fi
done
set -e

echo
echo "Nothing was left dirty"
if [ -n "$(git -C vendor/rigortype/steins status --porcelain)" ]; then
  echo "  FAIL    the installed package has local modifications:"
  git -C vendor/rigortype/steins status --porcelain | sed 's/^/          /'
  fail=1
else
  echo "  ok      the installed package is unmodified"
fi
leftovers="$(find vendor/rigortype/steins/composer/bin -maxdepth 2 \( -name '*.tar.gz' -o -name '*.sha256' \) )"
if [ -n "$leftovers" ]; then
  echo "  FAIL    download scratch survived: $leftovers"
  fail=1
else
  echo "  ok      the archive and its sidecar were cleaned up"
fi

echo
echo "Concurrent cold starts converge on one binary"
rm -rf "vendor/rigortype/steins/composer/bin/$version"
for _ in 1 2 3 4; do ./vendor/bin/steins doctor --no-php >/dev/null 2>&1 & done
wait
count="$(find "vendor/rigortype/steins/composer/bin/$version" -name steins -type f | wc -l | tr -d ' ')"
check "exactly one extracted binary" "$count" "1"

exit "$fail"
