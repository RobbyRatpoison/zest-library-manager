#!/bin/sh
# Build the fast profile and put it where the "Zest (dev)" shortcut runs it from.
# Usage: ./build-dev.sh [release]   (release = the full optimised build, slower to compile)
set -e
# the mold linker is much faster than the default one (about a third of the link time); used when it is installed and RUSTFLAGS is not set by the caller
if [ -z "${RUSTFLAGS:-}" ] && command -v mold >/dev/null 2>&1; then export RUSTFLAGS="-C link-arg=-fuse-ld=mold"; fi
cd "$(dirname "$0")"
PROFILE="${1:-fast}"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$HOME/.cache/zest-target}"
cargo build --profile "$PROFILE" -p grid-own -p zest-login
DIR="$CARGO_TARGET_DIR/$PROFILE"; [ "$PROFILE" = release ] && DIR="$CARGO_TARGET_DIR/release"
cp "$DIR/grid-own" "$HOME/.local/share/zest-dev/zest.new" && mv "$HOME/.local/share/zest-dev/zest.new" "$HOME/.local/share/zest-dev/zest"
# the sign-in window helper, started by Zest from next to itself (see pd-filter/src/weblogin.rs)
cp "$DIR/zest-login" "$HOME/.local/share/zest-dev/zest-login.new" && mv "$HOME/.local/share/zest-dev/zest-login.new" "$HOME/.local/share/zest-dev/zest-login"
echo "installed $PROFILE build"
