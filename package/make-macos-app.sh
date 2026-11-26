#!/bin/sh
# Builds Zest.app for macOS as one universal program (Intel and Apple Silicon) and packs it as package/dist/zest-<version>-macos-universal.tar.gz.
# Usage: package/make-macos-app.sh <version>   (on a Mac with both rust targets: rustup target add x86_64-apple-darwin aarch64-apple-darwin)
# The app is not signed with a developer certificate, only ad-hoc (Apple Silicon refuses unsigned code). install.sh fetches it with curl, which sets no
# quarantine flag, so Gatekeeper does not stop it. Its data is in ~/Library/Application Support/Zest (default_data_dir in grid-own/src/main.rs).
set -e
cd "$(dirname "$0")/.."
VERSION="${1:?usage: make-macos-app.sh <version>}"
export ZEST_BUILD_VERSION="$VERSION"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$PWD/target}"
# Apple Silicon needs macOS 11; keep the Intel half at the same floor so both halves agree
export MACOSX_DEPLOYMENT_TARGET=11.0
for T in x86_64-apple-darwin aarch64-apple-darwin; do
  cargo build --release --target "$T" -p grid-own -p zest-login
done
APP="package/dist/Zest.app"
rm -rf "$APP"; mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
for B in grid-own zest-login; do
  OUTNAME="$B"; [ "$B" = grid-own ] && OUTNAME=Zest
  lipo -create -output "$APP/Contents/MacOS/$OUTNAME" \
    "$CARGO_TARGET_DIR/x86_64-apple-darwin/release/$B" "$CARGO_TARGET_DIR/aarch64-apple-darwin/release/$B"
done
cp assets/favicon.icns "$APP/Contents/Resources/zest.icns"
# no version keys: the in-app updater (pd-filter/src/selfupdate.rs) swaps only the program file, so a version written here would go stale, and a changed
# Info.plist would no longer match the bundle's ad-hoc signature. Zest reports its own version (the update dialog).
cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>Zest</string>
  <key>CFBundleDisplayName</key><string>Zest</string>
  <key>CFBundleIdentifier</key><string>io.github.robbyratpoison.Zest</string>
  <key>CFBundleExecutable</key><string>Zest</string>
  <key>CFBundleIconFile</key><string>zest</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
  <key>LSApplicationCategoryType</key><string>public.app-category.games</string>
  <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
PLIST
codesign --force --deep --sign - "$APP"
NAME="zest-$VERSION-macos-universal"
tar -C package/dist -czf "package/dist/$NAME.tar.gz" Zest.app
echo "package/dist/$NAME.tar.gz"
