#!/bin/sh
# Jolta installer — grabs a prebuilt release binary when one exists for this
# platform (building from source with cargo otherwise), runs 'jolta setup'
# (self-contained install into ~/.jolta), and leaves nothing else behind.
#
#   curl -fsSL https://raw.githubusercontent.com/OneAppPlatform/jolta/main/install.sh | sh
set -eu

REPO=${JOLTA_REPO:-OneAppPlatform/jolta}
REF=${JOLTA_REF:-main}

tmp=$(mktemp -d "${TMPDIR:-/tmp}/jolta-installer.XXXXXX")
trap 'rm -rf "$tmp"' EXIT INT TERM

case "$(uname -s)/$(uname -m)" in
  Darwin/arm64)  target="aarch64-apple-darwin" ;;
  Darwin/x86_64) target="x86_64-apple-darwin" ;;
  Linux/x86_64)  target="x86_64-unknown-linux-musl" ;;
  Linux/aarch64) target="aarch64-unknown-linux-musl" ;;
  *)             target="" ;;
esac

# 1. Prebuilt binary from the latest release, if one matches this platform
if [ -n "$target" ]; then
  asset="jolta-$target.tar.gz"
  url="https://github.com/$REPO/releases/latest/download/$asset"
  if curl -fsSL -o "$tmp/$asset" "$url" 2>/dev/null; then
    echo "jolta installer: using prebuilt binary ($target)"
    tar -xzf "$tmp/$asset" -C "$tmp"
    "$tmp/jolta" setup
    echo "jolta installer: done — open a new shell and run 'jolta doctor'"
    exit 0
  fi
  echo "jolta installer: no prebuilt binary available; building from source"
fi

# 2. Source build fallback
command -v cargo >/dev/null 2>&1 || {
  echo "jolta installer: cargo not found — install Rust (https://rustup.rs or 'brew install rust'), or download a release binary manually from https://github.com/$REPO/releases" >&2
  exit 1
}
echo "jolta installer: fetching $REPO@$REF"
srcurl="https://codeload.github.com/$REPO/tar.gz/refs/heads/$REF"
curl -fsSL "$srcurl" -o "$tmp/jolta-src.tar.gz" || {
  echo "jolta installer: could not download $srcurl" >&2
  exit 1
}
mkdir -p "$tmp/src"
tar -xzf "$tmp/jolta-src.tar.gz" -C "$tmp/src" --strip-components=1
echo "jolta installer: building (release)"
cargo build --release --quiet --manifest-path "$tmp/src/Cargo.toml"
"$tmp/src/target/release/jolta" setup
echo "jolta installer: done — open a new shell and run 'jolta doctor'"
