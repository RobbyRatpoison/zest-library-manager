# Changelog

Notes shown in Zest's "What's new" window after an update. One `## vX.Y.Z` section per release, newest first; bullets under `### New`, `### Improvements` or `### Fixes`.

## v2026.11.0
### New
- Zest is a native application written in Rust, not a web page inside a window. It starts faster and uses less memory and CPU.
- Sign in with Steam (Settings > Account, and the first-run window): Steam's own login page opens in a sign-in window, and Zest fills in your SteamID and, when your account has one, your Web API key. Your password is typed on Steam's page and never reaches Zest.
- Date Added from Steam no longer needs the browser script when the sign-in window is installed: Zest reads each game's Steam Help page itself with that sign-in (the Companion script stays as the fallback).
- New Steam games get their real purchase date as Date Added when Steam is already signed in (after Sign in with Steam, or one Fetch Dates run, in this session): after a sync that adds games and after Add Game. Nothing opens by itself, and a big batch (over 100 games) is left to Bulk Ops.
- Your Steam sign-in is kept between runs: the long-lived part is saved in your system keyring (not in config.json or the log) and traded for fresh sessions when Zest starts reading dates, so one sign-in is enough for months. It is kept in the system's own vault: the keyring on Linux, Windows' per-user data protection on Windows, the Keychain on macOS. Without a vault it lasts until Zest closes.
- Edit Home Page shows a map of the page: see where each shelf sits, add and move shelves on it. De-duplication is set with a simple priority list.
- Covers that are missing download first for the games you are looking at (on screen, then just off screen), instead of in list order.
- Cover Quality filter (Filters): find covers that are low resolution, the wrong shape for the card, missing, or saved at JPEG quality 95, then re-fetch them with Bulk Ops.
- The Home clock has settings (Edit Home Page, select the clock): 12 or 24 hour time, seconds on or off, and whether the date shows. A clock with seconds shrinks to fit its shelf.
- Sign in with Steam fills the account's nickname with your Steam display name when the nickname is empty.
- Xbox with Xodus Nexus: SYNC WITH NEXUS asks every time what to do with the list of games you own: group them only, add the missing ones, or add them and remove the Xbox games Nexus does not report. Nothing is deleted without showing you the list and the count first.
- Windows: Uninstall Game runs the game's own uninstaller (found in Windows' list of installed programs) and the game leaves the Installed shelf when it is gone.

### Improvements
- New games get their covers while their details are still being read, and the details are read in two passes: the store page first (name, developer, genres, date, description), then reviews, tags and achievements. The first pass is paced to stay under Steam's request limit.
