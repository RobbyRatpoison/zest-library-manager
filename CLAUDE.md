# CLAUDE.md

Guidance for Claude Code when working in this repository.

## What this is

Zest is a local game library manager: a native desktop app in Rust (egui drawn into a software buffer, no web view). It is the successor to PlayDate (`~/Projects/PlayDate`, Python/Flask + pywebview), rewritten from scratch. Zest is a **separate app**, not an in-place update: it installs beside PlayDate and, on first start, offers to import a *copy* of the PlayDate library (`pd-filter/src/import_playdate.rs`).

The design history, decisions and per-feature notes live in PlayDate's `NATIVE_UI_PLAN.md` (section 0 = current state, section 16 "CURRENT PLAN" = the move to this repo and the release plan). They are deliberately not in this repository.

## Layout

| Path | Role |
|------|------|
| `grid-own/` | The app (binary `grid-own`, shipped as `zest` / `Zest.exe` / `Zest.app`): window, pages, dialogs, gamepad. `src/main.rs` is the hub; `tutorial_text.rs` is the in-app tutorial |
| `grid-core/` | Cover pipeline: decode off the UI thread, byte-budgeted cache, prefetch |
| `pd-filter/` | Everything that isn't drawing: database schema, filter tree -> SQL, Home shelves, Pick 6, scrapers, store integrations (`src/sources/`), updater, self-update, Wine/sandbox, backup, import |
| `zest-login/` | The sign-in window helper (wry + WebKitGTK 4.1 on Linux); Zest starts it from next to its own program file (`pd-filter/src/weblogin.rs`) |
| `plugin-sdk/` | JSON-RPC protocol and Python SDK for third-party plugins (`PROTOCOL.md`, toy example) |
| `vendor/egui`, `patches/` | Patched egui (see the patch files) |
| `assets/` | Embedded at compile time: favicon (`.png`, `.icns`), platform badge PNGs, the two PAGYWOSG JSON files |
| `userscript/` | Zest Companion Tampermonkey script |
| `package/` | `build-release.sh` (Linux tarball), `make-macos-app.sh` (universal ad-hoc-signed Zest.app), `zest.iss` (Windows Inno Setup installer), `flatpak/` (app id `io.github.robbyratpoison.Zest`, GNOME 50 runtime) |
| `install.sh` | User installer for Linux (`~/.local/share/zest/app`) and macOS (`~/Applications/Zest.app`); checks the release's `SHA256SUMS`; `ZEST_UNINSTALL=1` removes the program only |
| `.github/workflows/release.yml` | On a `v*` tag: tests, Linux tarball (Ubuntu 22.04 for an older glibc), Flatpak, Windows exe + installer + portable zip, universal Mac app, `SHA256SUMS`; `-beta`/`-rc` tags become prereleases |

## Building and testing

```bash
./build-dev.sh                  # fast profile, installs to ~/.local/share/zest-dev (Rob's "Zest (dev)" shortcut)
cargo test -p pd-filter -p grid-core -p grid-own
cargo build --release -p grid-own -p zest-login
```

Set `CARGO_TARGET_DIR` outside the repo (`build-dev.sh` uses `~/.cache/zest-target`). Release builds use full LTO and are slow (a universal Mac build takes over an hour on the Intel MacBook); CI does real releases.

**Golden tests:** many modules are pinned by golden files generated from the PlayDate Python code. The generators (`gen_*.py`) live in the PlayDate repo, not here. `pd-filter/tests/golden.json` is built from a real library, so it is gitignored; `tests/golden.rs` skips without it (and finds the real database via `ZEST_GOLDEN_DB`, else `../PlayDate/games_*.db`).

**Windows** compiles from Linux with the mingw target (see the `reference_zest_cross_platform_builds` memory); Windows lib tests don't compile there, use `examples/` in the test VM.

## Data and versions

- Data folder: `ZEST_DATA`, else Windows = next to `Zest.exe`, macOS = `~/Library/Application Support/Zest`, Linux = `~/.local/share/zest`, Flatpak = `~/.var/app/io.github.robbyratpoison.Zest/data/zest` (`default_data_dir` in `grid-own/src/main.rs`).
- Version: `ZEST_BUILD_VERSION` at compile time (CI sets it from the tag), else a dev placeholder (`pd-filter/src/updater.rs`). Calendar versions `YYYY.M.N`; the first release is **2026.11.1**. A release build's Linux window id is `zest`, a dev build's `zest-dev`.
- Updates (`updater.rs`, `selfupdate.rs`): Flatpak reinstalls the bundle; Linux/macOS/portable Windows swap the program file with a rollback watchdog; a Windows copy installed by `Zest-Setup.exe` (detected by `unins000.exe` beside the exe) runs the new installer silently (no rollback for that path yet).
- `CHANGELOG.md` feeds the in-app What's New (`pd-filter/src/whats_new.rs`); it lists differences from the last Python PlayDate release, not dev-build changes.

## Conventions

- Never use em dashes in code comments, UI text or docs. American spelling (color, not colour).
- User-facing text is plain and short; buttons in dialogs are uppercase like the rest of the UI.
- Every new clickable in a dialog must be reachable by gamepad.
- Never commit personal data: `config.json`, `state.json`, `games*.db`, logs and `static/` are gitignored for that reason.
- Commit by logical unit; flag untracked files before committing.
