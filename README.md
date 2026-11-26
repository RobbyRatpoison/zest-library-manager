# Zest

A local game library manager for people who take their backlog seriously. (ZEST = Zest Enhances Spare Time.)

Zest pulls in your Steam library and the other stores you connect, fills in metadata, cover art and HowLongToBeat times, and gives you one place to browse, filter, track what you have finished and pick what to play next. It is a native desktop app and runs entirely on your computer.

Zest is the successor to [PlayDate](https://github.com/RobbyRatpoison/PlayDate-Library-Manager), rewritten from scratch in Rust. It starts faster and uses much less memory.

---

## Features

- **Library**: grid and list views, live search, sorting, and filters (simple dropdowns or nested AND/OR groups). Bulk edit any field across a selection of games.
- **Home page**: shelves built from any saved filter or preset, side-by-side shelves, widgets and a clock.
- **Pick 6**: find something to play, either at random or scored against the games you have finished.
- **Other stores**: GOG, Epic Games, EA App, Ubisoft Connect, Battle.net, Humble Bundle, IndieGala, itch.io, Amazon Games, Rockstar, Legacy Games, Xbox, PlayStation and Nintendo eShop games appear next to your Steam games.
- **Emulators**: point Zest at your ROM folders and it adds them to your library with cover art.
- **Metadata**: Steam tags, reviews, achievements, release dates and HowLongToBeat times. Playtime and last played stay in sync with your local Steam files.
- **Completion tracking**: Never Played, Unfinished, Beaten, Completed, Won't Play.
- **Gamepad support**: every page and dialog works with a controller, including on the Steam Deck.

---

## Installation

### Windows

Download **Zest-Setup.exe** from the [latest release](https://github.com/RobbyRatpoison/zest-library-manager/releases/latest) and run it. Prefer portable? Download **Zest-Windows-Portable.zip**, extract it anywhere and run `Zest.exe`.

### Linux

**Flatpak (recommended):** download **Zest-\<version\>-Linux.flatpak** from the [latest release](https://github.com/RobbyRatpoison/zest-library-manager/releases/latest) and install it:

```bash
flatpak install --user Zest-<version>-Linux.flatpak
```

**Install script:** installs Zest to `~/.local/share/zest` with a menu entry. Run it again to update.

```bash
curl -fsSL https://raw.githubusercontent.com/RobbyRatpoison/zest-library-manager/main/install.sh | sh
```

### macOS

The same install script puts `Zest.app` in `~/Applications` (Intel and Apple Silicon). Run it again to update.

```bash
curl -fsSL https://raw.githubusercontent.com/RobbyRatpoison/zest-library-manager/main/install.sh | sh
```

---

## Moving from PlayDate

Zest is a new app, installed next to PlayDate rather than over it.

1. Install Zest as above. You do not need to uninstall PlayDate first.
2. On first start, Zest looks for your PlayDate library and offers to **import a copy** of it: your games, settings, theme, shelves, saved filters and cover art. If it can't find it, use **Browse** to point it at PlayDate's folder.
3. PlayDate is left untouched, so you can go back to it at any time. Once you are happy with Zest, you can uninstall PlayDate.

Things to redo by hand: taskbar pins, Steam shortcuts and menu entries that pointed to PlayDate. Store sign-ins may ask you to sign in again.

PlayDate no longer gets updates. Problems moving over can be reported on either repository.

---

## Building from source

For developers. You need a [Rust toolchain](https://rustup.rs) (stable). On Linux you also need the Wayland, xkbcommon, udev and WebKitGTK 4.1 development packages.

```bash
cargo build --release -p grid-own -p zest-login
```

The program is `target/release/grid-own` (shipped as `zest`), with the sign-in window `zest-login` next to it. `package/build-release.sh <version>` builds and packs a Linux or macOS release, and `package/flatpak/` holds the Flatpak manifest.

Run the tests with:

```bash
cargo test -p pd-filter -p grid-core -p grid-own
```

---

## License

MIT. See [LICENSE](LICENSE).
