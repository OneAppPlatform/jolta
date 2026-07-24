#!/bin/sh
# Jolta installer — fetches the repo tarball, runs 'jolta setup' (which installs
# a self-contained copy into ~/.jolta), and leaves nothing else behind.
#
#   curl -fsSL https://raw.githubusercontent.com/dave-oneapp/jolta/main/install.sh | sh
#
# While the repo is private the tarball needs auth, so the script falls back to
# an authenticated 'gh' CLI if plain curl can't reach it.
set -eu

REPO=${JOLTA_REPO:-dave-oneapp/jolta}
REF=${JOLTA_REF:-main}

tmp=$(mktemp -d "${TMPDIR:-/tmp}/jolta-installer.XXXXXX")
trap 'rm -rf "$tmp"' EXIT INT TERM

echo "jolta installer: fetching $REPO@$REF"
url="https://codeload.github.com/$REPO/tar.gz/refs/heads/$REF"
if curl -fsSL "$url" -o "$tmp/jolta.tar.gz" 2>/dev/null; then
  :
elif command -v gh >/dev/null 2>&1 && gh api "repos/$REPO/tarball/$REF" > "$tmp/jolta.tar.gz" 2>/dev/null; then
  echo "jolta installer: repo is private; downloaded via gh instead"
else
  echo "jolta installer: could not download $url" >&2
  echo "  (private repo? authenticate 'gh' or clone manually and run ./bin/jolta setup)" >&2
  exit 1
fi

mkdir -p "$tmp/src"
tar -xzf "$tmp/jolta.tar.gz" -C "$tmp/src" --strip-components=1
sh "$tmp/src/bin/jolta" setup
echo "jolta installer: done — open a new shell and run 'jolta doctor'"
