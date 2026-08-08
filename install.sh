#!/bin/sh
# Jolta installer — grabs a prebuilt release binary when one exists for this
# platform (building from source with cargo otherwise), runs 'jolta setup'
# (self-contained install into ~/.jolta), and leaves nothing else behind.
#
#   curl -fsSL https://raw.githubusercontent.com/OneAppPlatform/jolta/main/install.sh | sh
set -eu

# banner — cyan→violet gradient on a color terminal, plain otherwise
if [ -t 1 ] && [ -z "${NO_COLOR:-}" ] && [ "${TERM:-}" != "dumb" ]; then
  printf '\033[38;5;81m%s\033[0m\n'  '   __     ______     __         ______   ______'
  printf '\033[38;5;75m%s\033[0m\n'  '  /\ \   /\  __ \   /\ \       /\__  _\ /\  __ \'
  printf '\033[38;5;69m%s\033[0m\n'  ' _\_\ \  \ \ \/\ \  \ \ \____  \/_/\ \/ \ \  __ \'
  printf '\033[38;5;63m%s\033[0m\n'  '/\_____\  \ \_____\  \ \_____\    \ \_\  \ \_\ \_\'
  printf '\033[38;5;57m%s\033[0m\n\n' '\/_____/   \/_____/   \/_____/     \/_/   \/_/\/_/'
else
  # quoted heredoc: backslashes stay literal
  cat <<'EOF'
   __     ______     __         ______   ______
  /\ \   /\  __ \   /\ \       /\__  _\ /\  __ \
 _\_\ \  \ \ \/\ \  \ \ \____  \/_/\ \/ \ \  __ \
/\_____\  \ \_____\  \ \_____\    \ \_\  \ \_\ \_\
\/_____/   \/_____/   \/_____/     \/_/   \/_/\/_/

EOF
fi

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
    JOLTA_NO_BANNER=1 "$tmp/jolta" setup
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
JOLTA_NO_BANNER=1 "$tmp/src/target/release/jolta" setup
echo "jolta installer: done — open a new shell and run 'jolta doctor'"
