#!/bin/sh
# Installs or updates Zest on Linux and macOS from the latest GitHub release. Run it again to update.
#   curl -fsSL https://raw.githubusercontent.com/RobbyRatpoison/zest-library-manager/main/install.sh | sh
# Linux: the program goes to ~/.local/share/zest/app, with a menu entry, an icon and ~/.local/bin/zest.
# macOS: Zest.app goes to ~/Applications.
# Your library is never touched by this script (Linux: ~/.local/share/zest; macOS: ~/Library/Application Support/Zest).
# Options (environment): ZEST_VERSION=2026.11.1 installs that version instead of the latest; ZEST_UNINSTALL=1 removes the program (not your library).
set -eu

REPO="RobbyRatpoison/zest-library-manager"

say() { printf '%s\n' "$*"; }
die() { printf 'Zest install: %s\n' "$*" >&2; exit 1; }

command -v curl >/dev/null 2>&1 || die "curl is needed."
command -v tar >/dev/null 2>&1 || die "tar is needed."

OS="$(uname -s)"
ARCH="$(uname -m)"
case "$OS" in
  Linux)
    case "$ARCH" in
      x86_64|amd64) ASSET_SUFFIX="linux-x86_64" ;;
      *) die "there is no Linux build for $ARCH yet (only x86_64)." ;;
    esac ;;
  Darwin) ASSET_SUFFIX="macos-universal" ;;
  *) die "this script is for Linux and macOS. On Windows, download Zest-Setup.exe from https://github.com/$REPO/releases/latest" ;;
esac

APP_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/zest/app"
BIN_LINK="$HOME/.local/bin/zest"
DESKTOP_FILE="${XDG_DATA_HOME:-$HOME/.local/share}/applications/zest.desktop"
ICON_FILE="${XDG_DATA_HOME:-$HOME/.local/share}/icons/hicolor/512x512/apps/zest.png"
MAC_APP="$HOME/Applications/Zest.app"

if [ "${ZEST_UNINSTALL:-}" = 1 ]; then
  if [ "$OS" = Darwin ]; then
    rm -rf "$MAC_APP"
  else
    rm -rf "$APP_DIR"
    [ -L "$BIN_LINK" ] && rm -f "$BIN_LINK"
    rm -f "$DESKTOP_FILE" "$ICON_FILE"
    command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database -q "$(dirname "$DESKTOP_FILE")" 2>/dev/null || true
  fi
  say "Zest removed. Your library was left where it was."
  exit 0
fi

# which release
if [ -n "${ZEST_VERSION:-}" ]; then
  TAG="v${ZEST_VERSION#v}"
else
  TAG="$(curl -fsSL -H 'Accept: application/vnd.github+json' "https://api.github.com/repos/$REPO/releases/latest" \
    | sed -n 's/^[[:space:]]*"tag_name":[[:space:]]*"\([^"]*\)".*/\1/p' | head -n 1)"
  [ -n "$TAG" ] || die "could not find the latest release (GitHub may be limiting requests; try again in a few minutes)."
fi
VERSION="${TAG#v}"
ASSET="zest-$VERSION-$ASSET_SUFFIX.tar.gz"
# ZEST_DOWNLOAD_BASE is for testing the script against a local copy of a release
BASE="${ZEST_DOWNLOAD_BASE:-https://github.com/$REPO/releases/download/$TAG}"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT INT TERM

say "Downloading Zest $VERSION..."
curl -fL --progress-bar -o "$TMP/$ASSET" "$BASE/$ASSET" || die "download failed: $BASE/$ASSET"
curl -fsSL -o "$TMP/SHA256SUMS" "$BASE/SHA256SUMS" || die "could not download the checksums for $TAG."

# check the download against the release's checksum list
WANT="$(awk -v f="$ASSET" '$2 == f || $2 == "*"f { print $1 }' "$TMP/SHA256SUMS")"
[ -n "$WANT" ] || die "$ASSET is not in the release's checksum list."
if command -v sha256sum >/dev/null 2>&1; then GOT="$(sha256sum "$TMP/$ASSET" | awk '{print $1}')"
else GOT="$(shasum -a 256 "$TMP/$ASSET" | awk '{print $1}')"; fi
[ "$WANT" = "$GOT" ] || die "the download is damaged (checksum mismatch). Nothing was installed."

mkdir -p "$TMP/x"
tar -xzf "$TMP/$ASSET" -C "$TMP/x"

# only this install's own program counts (a dev build or a Flatpak named zest is another copy), and only when there was one to replace
running() {
  if [ "$OS" = Darwin ]; then [ -d "$MAC_APP" ] && pgrep -f "^$MAC_APP/Contents/MacOS/Zest" >/dev/null 2>&1
  else
    [ -e "$APP_DIR/zest" ] || return 1
    for p in $(pgrep -x zest 2>/dev/null); do
      [ "$(readlink "/proc/$p/exe" 2>/dev/null)" = "$APP_DIR/zest" ] && return 0
    done
    return 1
  fi
}
if running; then
  say "Zest is running: it keeps the old version until you close and reopen it."
fi

if [ "$OS" = Darwin ]; then
  [ -d "$TMP/x/Zest.app" ] || die "the download does not contain Zest.app."
  mkdir -p "$HOME/Applications"
  # replace in one step, so a failed copy never leaves half an app
  rm -rf "$MAC_APP.new"
  cp -R "$TMP/x/Zest.app" "$MAC_APP.new"
  xattr -dr com.apple.quarantine "$MAC_APP.new" 2>/dev/null || true
  rm -rf "$MAC_APP"
  mv "$MAC_APP.new" "$MAC_APP"
  say "Zest $VERSION is installed in ~/Applications. Open it from Launchpad, Spotlight or Finder."
  exit 0
fi

# Linux
SRC="$TMP/x/zest-$VERSION-$ASSET_SUFFIX"
[ -x "$SRC/zest" ] || die "the download does not contain the zest program."
mkdir -p "$(dirname "$APP_DIR")"
rm -rf "$APP_DIR.new"
mkdir -p "$APP_DIR.new"
cp "$SRC/zest" "$SRC/zest-login" "$SRC/zest.png" "$APP_DIR.new/"
rm -rf "$APP_DIR"
mv "$APP_DIR.new" "$APP_DIR"

mkdir -p "$(dirname "$BIN_LINK")" "$(dirname "$DESKTOP_FILE")" "$(dirname "$ICON_FILE")"
ln -sfn "$APP_DIR/zest" "$BIN_LINK"
cp "$APP_DIR/zest.png" "$ICON_FILE"
cat > "$DESKTOP_FILE" <<EOF
[Desktop Entry]
Type=Application
Name=Zest
Comment=Your personal game library manager
Exec="$APP_DIR/zest"
Icon=zest
Terminal=false
Categories=Game;
StartupWMClass=zest
EOF
command -v update-desktop-database >/dev/null 2>&1 && update-desktop-database -q "$(dirname "$DESKTOP_FILE")" 2>/dev/null || true
command -v gtk-update-icon-cache >/dev/null 2>&1 && gtk-update-icon-cache -q -t "$(dirname "$(dirname "$(dirname "$ICON_FILE")")")" 2>/dev/null || true

say "Zest $VERSION is installed. Start it from your applications menu, or run: zest"

# the sign-in window (store logins) needs WebKitGTK 4.1; Zest itself runs without it
if command -v ldconfig >/dev/null 2>&1 && ! ldconfig -p 2>/dev/null | grep -q 'libwebkit2gtk-4\.1\.so'; then
  say ""
  say "Note: store sign-in windows need WebKitGTK 4.1, which was not found. Install it with your package manager:"
  say "  Debian/Ubuntu: sudo apt install libwebkit2gtk-4.1-0"
  say "  Fedora:        sudo dnf install webkit2gtk4.1"
  say "  Arch:          sudo pacman -S webkit2gtk-4.1"
  say "Or use the Flatpak, which includes it."
fi
case ":$PATH:" in
  *":$HOME/.local/bin:"*) ;;
  *) say ""; say "(~/.local/bin is not in your PATH, so the 'zest' command needs the full path: $APP_DIR/zest)" ;;
esac
