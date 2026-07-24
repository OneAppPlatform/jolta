#!/bin/sh
# Jolta installer — fetches the source tarball, builds with cargo,
# runs 'jolta setup' (self-contained install into ~/.jolta), and cleans up.
#
#   curl -fsSL https://raw.githubusercontent.com/dave-oneapp/jolta/main/install.sh | sh
#
# Requires a Rust toolchain (https://rustup.rs or `brew install rust`) until
# prebuilt release binaries are published. While the repo is private the
# tarball needs auth, so the script falls back to an authenticated 'gh' CLI.
set -eu

REPO=${JOLTA_REPO:-dave-oneapp/jolta}
REF=${JOLTA_REF:-main}

command -v cargo >/dev/null 2>&1 || {
  echo "jolta installer: cargo not found — install Rust first (https://rustup.rs or 'brew install rust')" >&2
  exit 1
}

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
  echo "  (private repo? authenticate 'gh' or clone manually and run 'cargo build --release')" >&2
  exit 1
fi

mkdir -p "$tmp/src"
tar -xzf "$tmp/jolta.tar.gz" -C "$tmp/src" --strip-components=1
echo "jolta installer: building (release)"
cargo build --release --quiet --manifest-path "$tmp/src/Cargo.toml"
"$tmp/src/target/release/jolta" setup
echo "jolta installer: done — open a new shell and run 'jolta doctor'"
