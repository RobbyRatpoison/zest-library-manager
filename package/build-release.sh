#!/bin/sh
# Builds the optimised Zest and packs it as dist/zest-<version>-<os>-<arch>.tar.gz (Linux and macOS): the program, a desktop entry and the icon.
# Usage: package/build-release.sh [version]   (default: the date, YYYY.M.D)
# The data folder is chosen at run time (see default_data_dir in grid-own/src/main.rs): ZEST_DATA, else ~/.local/share/zest.
set -e
cd "$(dirname "$0")/.."
VERSION="${1:-$(date +%Y.%-m.%-d)}"
# the version the program reports and compares against releases (see pd-filter/src/updater.rs)
[ -n "$1" ] && export ZEST_BUILD_VERSION="$VERSION"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$HOME/.cache/zest-target}"
cargo build --release -p grid-own -p zest-login
OS="$(uname -s | tr 'A-Z' 'a-z')"; ARCH="$(uname -m)"
NAME="zest-$VERSION-$OS-$ARCH"
OUT="package/dist/$NAME"
rm -rf "$OUT"; mkdir -p "$OUT"
cp "$CARGO_TARGET_DIR/release/grid-own" "$OUT/zest"
cp "$CARGO_TARGET_DIR/release/zest-login" "$OUT/zest-login"   # the sign-in window; Zest starts it from next to itself
cp package/zest.desktop "$OUT/zest.desktop"
cp assets/favicon.png "$OUT/zest.png"
cat > "$OUT/README.txt" <<TXT
Zest $VERSION
Run ./zest. Without an argument it keeps its data in \$ZEST_DATA, else ~/.local/share/zest.
On first start it offers to import a copy of your PlayDate library, if it finds one.
For a menu entry: copy zest.desktop to ~/.local/share/applications and zest.png to ~/.local/share/icons, and set Exec= to the full path of zest.
TXT
tar -C package/dist -czf "package/dist/$NAME.tar.gz" "$NAME"
echo "package/dist/$NAME.tar.gz"
