//! The words of the in-app tutorial: sections of steps. Every claim here describes what Zest does today (checked against the code when it was written; Zest's own gaps are listed
//! honestly in the last section). A body is paragraphs separated by a blank line; a line starting with "- " is a bullet; `**bold**`; `[[open:key|label]]` opens a dialog and
//! `[[go:section|label]]` / `[[go:section:Step title|label]]` jumps inside the tutorial. `tests` checks every link and the house rules for written text.

pub struct Step { pub title: &'static str, pub body: &'static str }
pub struct Section { pub id: &'static str, pub title: &'static str, pub steps: &'static [Step] }

/// The dialogs a link may open: the menu keys `modal_ui` understands.
pub const OPEN_KEYS: &[&str] = &["add_game", "duplicates", "community", "plugins", "data", "blacklist", "hltb", "settings", "update", "emulators"];

pub const SECTIONS: &[Section] = &[
    Section { id: "getting-started", title: "Getting Started", steps: &[
        Step { title: "Welcome to Zest", body: "Zest is a game library manager that runs on your own computer. It keeps your Steam collection, and games from other stores, in one library you can browse, filter, track and pick from.\n\nThis tour covers what Zest does today. Jump to any section from the list, or read it top to bottom. It is always under the menu button (the three lines, top left) > Tutorial." },
        Step { title: "Three pages", body: "The bar at the top has three pages:\n\n- [[go:home|**Home**]]: shelves of games and a few charts.\n- [[go:library|**Library**]]: everything you own, with filters, sorting, grouping and bulk tools.\n- [[go:pick6|**Pick 6**]]: a \"what should I play next\" picker.\n\nEverything else (adding games, stores, backups, updates) is behind the menu button at the top left." },
        Step { title: "Adding your games", body: "The first time Zest starts it makes your library and adds your Steam games by itself. After that, **POPULATE ZEST** in the menu adds games you bought since. It only adds what is missing, skips anything you removed on purpose, and then fills in details, reviews, covers, ProtonDB ratings and HowLongToBeat times in the background.\n\nWithout a Steam Web API key Zest can only see games Steam knows about on this computer, and it cannot read achievements. Add the key under [[open:settings|Settings]] and press Populate again to get the whole library.\n\nTo add one game by hand, use [[open:add_game|**Add Game**]]: search Steam, or type a name for a game that is not on Steam. Games from other stores come from [[go:stores|Stores]]." },
        Step { title: "Launching a game", body: "Click a cover to play it. A Steam game opens through Steam (an uninstalled one gets Steam's install prompt). Games from other stores start through their store's launcher or from their own folder.\n\nWhat each mouse button does on a cover is a list of rules in your settings file. The defaults are: left click launches, right click opens the menu. Zest reads PlayDate's rules if you copied them over (middle, side buttons and double clicks included), but it has no editor for them yet." },
        Step { title: "The right-click menu", body: "Right-click a game, on any page, for quick actions: **Launch** or **Install**, **Open Store Page**, **Achievements** and **Community Hub** (Steam games), **Edit Game**, **Completion Status** (a submenu), **Uninstall** and **Delete**.\n\nDelete asks whether to blacklist the game too, so a later Populate does not bring it back." },
        Step { title: "Gamepad", body: "A gamepad drives all of Zest, with the same timing as PlayDate. D-pad or left stick moves (a cursor over covers, focus in dialogs), **A** confirms, **B** goes back, **X** opens a game's menu, **Y** edits it, and the bumpers page through the grid.\n\nPads that SDL knows work as they are. There is no layout wizard or button remapping in Zest yet." },
    ] },
    Section { id: "home", title: "Home", steps: &[
        Step { title: "Shelves and widgets", body: "Home shows your library as horizontal shelves, for example Recently Played, Installed, a random pick. Each shelf is a **filter** (which games it can show) plus a **sort**. Between them there can be widgets, such as a clock and a chart of your completion statuses.\n\nA shelf with more games than fit scrolls sideways. Hover a cover to see it grow; click it to launch." },
        Step { title: "Shuffle", body: "A shelf that is sorted randomly has a shuffle button. Press it to draw a fresh set without leaving the page." },
        Step { title: "Installed games follow your installs", body: "The Installed and Not Installed shelves update by themselves when a game is installed or uninstalled while Zest is open, whichever store it comes from." },
        Step { title: "Changing the layout", body: "Zest shows the layout saved in your settings file (the shelves, their filters, sorts, sizes and columns). [[open:settings|**Settings**]] > Home lists the shelves: show or hide each, rename, reorder, add, delete, and change its games filter, sort, number of covers and de-duplication. Row heights and side-by-side groups are not editable yet; see [[go:not-yet|What Zest does not do yet]]." },
    ] },
    Section { id: "library", title: "Library", steps: &[
        Step { title: "Layouts, sorting and grouping", body: "The **VIEW** button changes how the Library looks.\n\n- **Display**: a Vertical or Horizontal cover grid, or **List**, a split pane with rows on the left and a detail and edit pane on the right.\n- **Sort by**: name, hours played, release date, recently played, date added, review scores, total reviews, achievements, tag similarity (how close a game's tags are to games you finished), random, and more. A button flips ascending and descending.\n- **Group by**: installed, completion status, release date, date added, review score or library, as headings in the grid.\n\nSorting and grouping apply at once, without reloading the list." },
        Step { title: "Search", body: "The search box at the top of the Library filters by name as you type. Clear it to see everything again." },
        Step { title: "Building a filter", body: "**FILTERS** opens the filter dialog. **Simple** mode is a list of conditions on tags, genres, groups, status, release date, playtime, reviews, developers and more, joined by AND (all must match) or OR (any can match). **Advanced** mode adds nested groups and a custom WHERE clause for anything the menus cannot say.\n\nThe dialog also hides or shows whole libraries (Steam, GOG, and so on). **APPLY** shows the result, **CLEAR ALL** starts over." },
        Step { title: "Saved filters", body: "**Saved filters...** in the dialog loads, saves, renames and deletes named filters. A saved filter can be a Home shelf's filter, narrow the pool Pick 6 draws from, and be used inside another filter with a Saved Filter condition (games in, or not in, another filter)." },
        Step { title: "Select games and Bulk Ops", body: "**BULK OPS** works on every game matching your filter, or on games you pick (choose **Pick games**: the Library goes into select mode, click covers, then open Bulk Ops again).\n\nIts tabs: **Edit** changes tags, groups, status and other fields on all of them at once, **Re-scrape** re-fetches Steam metadata, store data, artwork, ProtonDB and HowLongToBeat data and fills in missing details, **Delete** removes them (optionally blacklisting), and **Date Importer** handles purchase dates." },
        Step { title: "Review scores", body: "A game can carry several scores. **Review %** is the share of positive Steam reviews. **Weighted %** pulls scores with few reviews toward 50 so a handful of reviews cannot make a game look great. Both can be sorted, filtered and grouped on." },
    ] },
    Section { id: "pick6", title: "Pick 6", steps: &[
        Step { title: "What it does", body: "Pick 6 deals six games to choose from, so you do not have to scan the whole library. Press the **PICK 6** button to deal; press it again to deal a new six." },
        Step { title: "Modes", body: "**MODE** sets how the six are chosen.\n\n- **RANDOM**: any game from the pool.\n- **SMART**: random, but weighted toward games you are likely to enjoy and have neglected (tags you finish, staleness, reviews, playtime, release date).\n- **WEIGHTED**: you set how much each signal counts with the sliders.\n\nSmart and Weighted are still random draws, not a ranked top six, so you will see different games each time." },
        Step { title: "The pool", body: "**POOL** limits what can be picked: by completion status (Never Played, Unfinished and so on), or **Use currently filtered games only** to draw from whatever your Library filter shows." },
    ] },
    Section { id: "editing", title: "Editing a game", steps: &[
        Step { title: "Opening the editor", body: "Choose **Edit Game** in the right-click menu, press **Y** on a gamepad, or use the pencil that appears on a cover. In List mode the detail pane is the editor." },
        Step { title: "What you can change", body: "Title, completion status, installed flag, last played, playtime, release date, developers, publishers, genres, categories, tags, groups, description, free to play, and the review numbers. Tags and groups are comma separated with no spaces after the commas.\n\nButtons: **OPEN FOLDER**, **CHANGE EXECUTABLE** (for installed games of other stores: pick which file starts the game), **FILL MISSING DATA**, **SYNC <STORE> DATA**, and **DELETE GAME**." },
        Step { title: "Which file starts a game", body: "For games Zest starts from a folder (Humble, itch.io, IndieGala, GOG), Zest picks the best file itself. When several files are equally good, it asks once which to use and remembers your answer for that game. **CHANGE EXECUTABLE** lets you choose again later." },
        Step { title: "Games on several stores", body: "When you own a game in more than one library, Zest shows one copy, the one from the library you rank highest. The editor can link a game to its copy elsewhere. See [[go:cleanup|Duplicates and the blacklist]]." },
    ] },
    Section { id: "art", title: "Cover art", steps: &[
        Step { title: "Where covers come from", body: "Zest fetches a vertical cover, a horizontal header and an icon for each game. Steam games use Steam's own art; other games use the store's art when it has any, then Steam's copy of the game, then SteamGridDB. A cover that does not fit the card is drawn with bars colored from its own edges." },
        Step { title: "The art tabs", body: "In the editor, the **Vertical**, **Horizontal** and **Icon** tabs each show the current picture, where it came from, and these buttons:\n\n- **RE-SCRAPE**: fetch it again.\n- **FROM STEAM**: use Steam's art (for other stores' games).\n- **CUSTOM URL**: a web address or a file on your computer.\n- **SHOW FILE** and **CLEAR**.\n- **CROP**: move and zoom the picture inside the card's frame.\n- **BROWSE SGDB**: pick from SteamGridDB (needs a SteamGridDB key in [[open:settings|Settings]])." },
        Step { title: "Many at once", body: "Bulk Ops > Re-scrape > **Artwork** re-fetches covers for many games, from any source, for any of the three kinds." },
    ] },
    Section { id: "status", title: "Completion status", steps: &[
        Step { title: "The five statuses", body: "- **Never Played**\n- **Unfinished**\n- **Beaten**: your own call that you finished it.\n- **Completed**: 100 percent of achievements.\n- **Won't Play**: you decided not to, for any reason.\n\nChange one from the right-click menu, the editor, or in bulk." },
        Step { title: "What happens by itself", body: "At startup Zest reads Steam's own files: games with playtime move from Never Played to Unfinished. When a Steam game reaches 100 percent of its achievements it becomes Completed, and a Completed game that no longer shows 100 percent goes back to Beaten." },
    ] },
    Section { id: "metadata", title: "Details and times", steps: &[
        Step { title: "Fill Missing Data", body: "**FILL MISSING DATA** (in the editor, and in Bulk Ops) completes what a game lacks. For a game from another store it finds the Steam page (through PCGamingWiki, then a Steam search), and uses it for tags, genres, reviews and more. It never overwrites what is already there." },
        Step { title: "HowLongToBeat", body: "[[open:hltb|**HLTB**]] in the menu lists every game's HowLongToBeat match. Matches below your threshold wait under **Needs review**: **CONFIRM** them, pick **OTHER MATCH** (search, or paste a HowLongToBeat address), or **NO PAGE** when the game has none. **CONFIRM ALL ABOVE** confirms every match scoring at least the slider's value, and **RE-SCRAPE UNMATCHED** tries again for games with no match." },
        Step { title: "ProtonDB", body: "Steam games get a ProtonDB rating (platinum to borked) from Bulk Ops > Re-scrape > ProtonDB." },
    ] },
    Section { id: "stores", title: "Stores", steps: &[
        Step { title: "The stores", body: "Open [[open:plugins|**Plugins**]] to connect a store. Each card has its own sign-in and **SYNC LIBRARY**. Built in: GOG, Epic Games, Humble Bundle, itch.io, IndieGala, Amazon Games, Xbox, PlayStation, Nintendo eShop, EA App, Ubisoft Connect, Battle.net, Rockstar Games and Legacy Games. Steam is always there." },
        Step { title: "Signing in", body: "Stores sign in in different ways, and the card explains its own:\n\n- Paste something your browser has (an address, a cookie or a token), or let the **Zest Companion** userscript read it from the browser you are already signed in to (it needs Tampermonkey).\n- For the launcher-based stores on Linux (Battle.net, Rockstar, Legacy Games, Ubisoft Connect), give the Wine prefix their launcher is installed in. **INSTALL LAUNCHER** makes one for you.\n\nZest keeps tokens only in your settings file on this computer." },
        Step { title: "Install, launch and uninstall", body: "What Zest can do for a game depends on the store. It downloads and installs Humble, itch.io and GOG games itself (into the games folder shown on the card), starts games from other stores through their launcher, and removes files only inside its own games folder. Per store details, and what is not done yet, are in the card's notes." },
        Step { title: "Sync at startup", body: "A card's **Sync this library when Zest starts** checkbox syncs that store ten seconds after the window opens, one store at a time, only when you are already signed in." },
        Step { title: "Other plugins", body: "Third-party plugins are folders in the plugins folder (**OPEN PLUGIN FOLDER** shows where). They talk to Zest over a simple protocol, so they can be written in any language." },
    ] },
    Section { id: "community", title: "Community tools", steps: &[
        Step { title: "What they are", body: "[[open:community|**Community Tools**]] in the menu holds helpers for the communities around SteamGifts. PlayDate and Zest are not part of any of them, and nothing is ever submitted for you." },
        Step { title: "PAGYWOSG, Monthly in a Month, Secret Santa", body: "The filter builders read the current month's categories from the community's site and build a Library filter of the games that qualify, with a note on why each does. **Secret Santa / Snowballs** edits your gift list, which counts as wins." },
        Step { title: "BLAEO and SteamGifts", body: "**Sync BLAEO** reads your lists from BLAEO. **Sync SteamGifts Wins** reads your won giveaways in your own browser (through the Companion userscript, because SteamGifts blocks programs), tags those games \"Won on SteamGifts\", and lists wins that match no game. **Full refresh** re-checks every win and drops stale ones." },
    ] },
    Section { id: "cleanup", title: "Duplicates and the blacklist", steps: &[
        Step { title: "Duplicates", body: "A game you own in several stores is shown once, as the copy from the library you rank highest; the others are hidden under it. [[open:duplicates|**Duplicates**]] in the menu lists games that match only when an edition name is ignored (\"Game - Remastered\" and \"Game\"). Nothing is hidden until you approve it, and **Not duplicates** says no for good." },
        Step { title: "The blacklist", body: "Games you delete with **Delete and blacklist** go on the blacklist so a sync never adds them back. [[open:blacklist|**Blacklist / Junk Finder**]] lists them and lets you take one back. **Find Library Junk** there suggests games that are probably not real ones (a beta, demo, soundtrack or dev kit by its title, and Steam games no longer on your account when an API key is set): review the list, then blacklist, delete or whitelist the ones you pick. Nothing changes until you press a button." },
    ] },
    Section { id: "data", title: "Backups, settings and updates", steps: &[
        Step { title: "Backup and restore", body: "[[open:data|**Data**]] in the menu backs up your library, settings and (optionally) every cover into one zip, restores one, and exports your library as a CSV spreadsheet (all games, or what the Library shows). Backups use PlayDate's layout, so each program can restore the other's. Restoring replaces your library: restart Zest afterwards." },
        Step { title: "Settings", body: "[[open:settings|**Settings**]] holds your Steam accounts (add, switch or remove one; a switch takes effect when Zest restarts), the Web API key (what brings your whole library and your achievements) and your SteamGridDB key, then Library, Home, Art, Outlines, Badges, Duplicates, Mouse and Gamepad (stick feel and what each button does), Appearance (theme colors and saved themes, background picture, UI scale) and Tuning. Everything is saved in your data folder." },
        Step { title: "Your data folder", body: "Everything Zest keeps is in one folder: the library database, settings, covers. Start Zest with a folder as its argument (or set ZEST_DATA) to use another. In a Flatpak it is the same folder PlayDate used, so upgrading in place keeps your library." },
        Step { title: "Updates", body: "[[open:update|**Check for Updates**]] looks for a newer Zest, shows what changed and downloads it. A Flatpak install is updated for you; other installs get the file in Downloads. Zest also looks once a day when it starts and says so in the status line (turn that off in the same dialog). Zest does not undo a bad update yet, so keep a backup." },
    ] },
    Section { id: "emulators", title: "Emulators", steps: &[
        Step { title: "Adding an emulator", body: "Menu > [[open:emulators|Emulators]] > **ADD AN EMULATOR**. Pick the one you use from the list (about fifty are known, for most Nintendo, Sony, Sega and arcade systems). Zest looks for its program on your search path, in Flatpak exports, in AppImage folders and in the usual install folders; **DETECT** tries again and **BROWSE** lets you pick it. An emulator that is not listed can be added at the bottom: a name, the system and its program." },
        Step { title: "ROM folders and scanning", body: "Give each system a ROM folder (a system can have several: **ADD FOLDER**), then **SCAN THIS SYSTEM** or **SCAN ALL**. Every file with a matching extension becomes a game, named from the file name without region codes and brackets, and its cover is fetched. A game that is already in the library, or on the blacklist, is skipped. PlayStation 3 folders and Vita3K installs are found in the emulator's own game folder, and Vita titles are named from the game's own data. MAME and FBNeo games are named by MAME when it is installed." },
        Step { title: "Playing, arguments and cores", body: "Launching a ROM game runs the emulator with the arguments shown for it: **{rom}** is the game file, **{rom_dir}** its folder, **{rom_name}** its name without the extension. RetroArch also needs a core for each system (**FIND** lists the ones installed, in the order that suits the system best). If a Flatpak emulator cannot see your ROM folder, the entry says so and shows the one command that fixes it. At start Zest checks that each ROM is still on disk and marks the game not installed when it is gone." },
    ] },
    Section { id: "linux", title: "Windows games on Linux", steps: &[
        Step { title: "Proton and Wine", body: "Windows games from other stores run through Proton (with umu-run when it is installed) or else through Wine. Zest finds them by itself, in the usual Steam and distro folders. Each game, or each launcher, gets its own Wine prefix." },
        Step { title: "Launchers", body: "Some stores need their own launcher (Epic, EA App, Ubisoft Connect, Battle.net, Rockstar, Legacy Games). **INSTALL LAUNCHER** on the store's card downloads and runs its installer in a prefix of its own, then you sign in to it once." },
        Step { title: "In a Flatpak", body: "Inside a Flatpak, Zest runs Wine, Proton and the games on your computer itself, outside the sandbox, so the same Proton and Wine you already have are used." },
    ] },
    Section { id: "not-yet", title: "What Zest does not do yet", steps: &[
        Step { title: "Compared with PlayDate", body: "Zest is a new build, and some of PlayDate is not in it yet:\n\n- Dragging shelves on the Home page, row heights and side-by-side shelf groups.\n- Some of PlayDate's Settings: the gamepad layout wizard with custom drawing and button names (button remapping is in), and a few smaller ones.\n- Updating itself in place and undoing a bad update.\n\nUntil these exist, keep using PlayDate for them: both read the same data folder format, and a backup from one restores in the other." },
    ] },
];

#[cfg(test)]
mod tests {
    use super::*;
    /// `[[kind:target|label]]` links in a body.
    fn links(body: &str) -> Vec<(String, String)> {
        let mut out = vec![]; let mut rest = body;
        while let Some(i) = rest.find("[[") { let Some(j) = rest[i..].find("]]") else { break }; let inner = &rest[i + 2..i + j]; let (spec, _) = inner.split_once('|').unwrap_or((inner, "")); let (k, t) = spec.split_once(':').unwrap_or((spec, "")); out.push((k.to_string(), t.to_string())); rest = &rest[i + j + 2..]; }
        out
    }
    #[test] fn every_link_points_somewhere_and_every_section_and_step_is_there() {
        let ids: Vec<&str> = SECTIONS.iter().map(|s| s.id).collect();
        assert_eq!(ids.len(), { let mut v = ids.clone(); v.sort(); v.dedup(); v.len() }, "section ids are unique");
        for s in SECTIONS {
            assert!(!s.steps.is_empty() && !s.title.is_empty(), "{}", s.id);
            let titles: Vec<&str> = s.steps.iter().map(|x| x.title).collect();
            assert_eq!(titles.len(), { let mut v = titles.clone(); v.sort(); v.dedup(); v.len() }, "step titles in {} are unique", s.id);
            for st in s.steps {
                assert!(!st.body.trim().is_empty(), "{}: {}", s.id, st.title);
                for (k, t) in links(st.body) {
                    match k.as_str() {
                        "open" => assert!(OPEN_KEYS.contains(&t.as_str()), "{}: {} opens an unknown dialog {t}", s.id, st.title),
                        "go" => { let (sec, step) = t.split_once(':').map_or((t.as_str(), None), |(a, b)| (a, Some(b))); let target = SECTIONS.iter().find(|x| x.id == sec).unwrap_or_else(|| panic!("{}: {} goes to unknown section {sec}", s.id, st.title)); if let Some(step) = step { assert!(target.steps.iter().any(|x| x.title == step), "no step {step} in {sec}"); } }
                        o => panic!("unknown link kind {o}"),
                    }
                }
            }
        }
    }
    #[test] fn the_text_follows_the_house_rules() {
        for s in SECTIONS { for t in std::iter::once(s.title).chain(s.steps.iter().flat_map(|x| [x.title, x.body])) {
            assert!(!t.contains('\u{2014}') && !t.contains('\u{2013}'), "no em or en dashes: {t}");
            assert!(t.matches("**").count() % 2 == 0 && t.matches("[[").count() == t.matches("]]").count(), "balanced markup: {t}");
        } }
    }
}
