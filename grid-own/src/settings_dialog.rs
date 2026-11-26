//! Settings: the accounts and keys, the Library switches (completion sync, artwork shape) and the Tuning sliders (tag similarity, review scores, Pick 6 scoring), as in PlayDate.
//! Keys are saved in `config.json`, the file PlayDate uses, in the same shape; every other setting goes into `state.json` through the app (`SettingsOut::Apply`), which also rescored
//! the library when a Tuning section asks for it. The rest of PlayDate's settings are still to come.
use crate::card_actions::{self, Button, Click};
use crate::ui_kit::{self, Kind};
use pd_filter::account::{self, Account};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Clone, Copy, PartialEq)]
enum Tab { Accounts, Library, Art, Outlines, Badges, Duplicates, Mouse, Gamepad, Appearance, Tuning, Advanced }

/// Appearance's own dialogs, opened from its list.
#[derive(Clone, Copy, PartialEq)]
enum AppSub { Background, Theme, Editor, Outlines, Badges }
impl AppSub { fn title(self) -> &'static str { match self { AppSub::Background => "Background & Opacity", AppSub::Theme => "Choose Theme", AppSub::Editor => "Theme Editor", AppSub::Outlines => "Card Outlines", AppSub::Badges => "Card Badges" } } }

/// The Settings list as PlayDate shows it (Account, Appearance, Library, Gamepad, Tuning): each entry opens its own dialog, here the tabs it owns.
#[derive(Clone, Copy, PartialEq)]
enum Group { Account, Appearance, Library, Gamepad, Tuning, Advanced }
impl Group {
    const ALL: [Group; 6] = [Group::Account, Group::Appearance, Group::Library, Group::Gamepad, Group::Tuning, Group::Advanced];
    fn title(self) -> &'static str { match self { Group::Account => "Account", Group::Appearance => "Appearance", Group::Library => "Library", Group::Gamepad => "Gamepad", Group::Tuning => "Tuning", Group::Advanced => "Advanced" } }
    fn tabs(self) -> &'static [(Tab, &'static str)] { match self {
        Group::Account => &[(Tab::Accounts, "ACCOUNTS")],
        Group::Appearance => &[(Tab::Appearance, "APPEARANCE")],
        Group::Library => &[(Tab::Library, "LIBRARY"), (Tab::Art, "ART"), (Tab::Duplicates, "DUPLICATES"), (Tab::Mouse, "MOUSE")],
        Group::Gamepad => &[(Tab::Gamepad, "GAMEPAD")],
        Group::Tuning => &[(Tab::Tuning, "TUNING")],
        Group::Advanced => &[(Tab::Advanced, "ADVANCED")],
    } }
}

/// One Tuning slider (PlayDate's `TUNE_SECTIONS`). `log`: the slider moves along log10 of the value (reviews needed to trust a score: 2 to 1000).
struct Knob { key: &'static str, label: &'static str, min: f64, max: f64, step: f64, def: f64, unit: &'static str, hint: &'static str, log: bool }
struct Section { id: &'static str, title: &'static str, desc: &'static str, knobs: &'static [Knob] }

const CURVE: &str = "Shapes how the value ramps up: 1 is a straight line, lower rises fast then flattens, higher stays low then climbs.";
const SECTIONS: &[Section] = &[
    Section { id: "tags", title: "Tag Similarity", desc: "Scores every game by tag similarity to your Beaten/Completed games (the taste profile Pick 6 uses too), so it works as a sort in Library and Home. Saving rescores the whole library.", knobs: &[
        Knob { key: "tag_similarity_min_library_rate", label: "Rare-tag floor", min: 0.0, max: 10.0, step: 0.5, def: 2.0, unit: "%", log: false, hint: "Tags in fewer than this % of your Steam library are ignored (too small a sample to trust). Lower to let rarer tags count; raise if the disliked-tags list looks noisy." },
        Knob { key: "tag_similarity_smoothing_k", label: "Smoothing", min: 0.0, max: 20.0, step: 1.0, def: 4.0, unit: "", log: false, hint: "Higher values pull sparsely-tagged games toward neutral so one lucky tag cannot outscore a game with several genuinely matching tags." },
        Knob { key: "tag_similarity_playtime_cap_hours", label: "Playtime saturation", min: 1.0, max: 200.0, step: 1.0, def: 60.0, unit: " hr", log: false, hint: "Hours a Beaten/Completed game needs before it counts at full weight in your taste profile. Lower if short games should shape your profile as much as long ones." },
    ] },
    Section { id: "reviews", title: "Review Scores", desc: "The weighted review score pulls scores with few reviews toward neutral (50%). Saving recomputes it for every Steam game from the review counts already stored, with no re-scraping.", knobs: &[
        Knob { key: "review_half_trust_count", label: "Reviews to trust a score", min: 2.0, max: 1000.0, step: 1.0, def: 10.0, unit: " reviews", log: true, hint: "How many reviews before a score counts halfway between neutral and its raw value. Higher makes small samples count for less, for longer." },
    ] },
    Section { id: "pick6", title: "Pick 6 Scoring", desc: "How Pick 6 turns a game's data into scores. The signal weights themselves stay on the Pick 6 page (Weighted mode).", knobs: &[
        Knob { key: "pick6_smart_tag_weight", label: "Smart mode blend", min: 0.0, max: 100.0, step: 1.0, def: 65.0, unit: "/", log: false, hint: "Tag similarity vs. review score (the second number), when not using Weighted." },
        Knob { key: "pick6_staleness_cap_days", label: "Staleness cap", min: 30.0, max: 2000.0, step: 10.0, def: 730.0, unit: " days", log: false, hint: "Days since last played that count as \"maximally stale.\"" },
        Knob { key: "pick6_staleness_curve", label: "Staleness curve", min: 0.2, max: 4.0, step: 0.05, def: 1.0, unit: "", log: false, hint: CURVE },
        Knob { key: "pick6_recency_cap_years", label: "Recency cap", min: 1.0, max: 50.0, step: 1.0, def: 10.0, unit: " yr", log: false, hint: "Age in years that counts as \"maximally old.\"" },
        Knob { key: "pick6_recency_curve", label: "Recency curve", min: 0.2, max: 4.0, step: 0.05, def: 1.0, unit: "", log: false, hint: CURVE },
        Knob { key: "pick6_hltb_long_floor_hours", label: "Prefer-long floor", min: 0.0, max: 100.0, step: 1.0, def: 10.0, unit: " hr", log: false, hint: "Below this, a long game scores 0 on Length." },
        Knob { key: "pick6_hltb_long_cap_hours", label: "Prefer-long cap", min: 10.0, max: 300.0, step: 1.0, def: 110.0, unit: " hr", log: false, hint: "At or above this, a long game scores 1.0 on Length." },
        Knob { key: "pick6_hltb_long_curve", label: "Prefer-long curve", min: 0.2, max: 4.0, step: 0.05, def: 0.5, unit: "", log: false, hint: CURVE },
        Knob { key: "pick6_hltb_short_cap_hours", label: "Prefer-short cap", min: 1.0, max: 100.0, step: 1.0, def: 10.0, unit: " hr", log: false, hint: "At or above this, a short game scores 0 on Length." },
        Knob { key: "pick6_hltb_short_curve", label: "Prefer-short curve", min: 0.2, max: 4.0, step: 0.05, def: 0.5, unit: "", log: false, hint: CURVE },
    ] },
];

/// PlayDate's `HOVER_TIP_FIELDS` and `_HOVER_TIP_LABELS`: the details the card tooltip can show, in the order it saves them.
const TIP_FIELDS: &[(&str, &str)] = &[
    ("cover_alt", "Other-orientation cover"), ("description", "Short description (Steam / plugin)"), ("playtime", "Time played"), ("hltb", "HowLongToBeat times (main, extras, 100%)"),
    ("last_played", "Last played"), ("date_added", "Date added"), ("release_date", "Release date"), ("platform", "Library (Steam, Epic, GOG...)"),
    ("community_score", "Steam community score"), ("metacritic", "Metacritic score"), ("developers", "Developer"), ("publishers", "Publisher"),
];
/// PlayDate's `normalize_hover_tip`: pages and fields of a saved tooltip setting, an older `{enabled, home, fields}` included (`enabled` meant the Library page).
fn read_tip(v: &Value) -> ([bool; 3], Vec<&'static str>) {
    let b = |k: &str| v.get(k).and_then(|x| x.as_bool()).unwrap_or(false);
    let legacy = v.get("library").is_none();
    let library = if legacy { b("enabled") } else { b("library") };
    let home = b("home") && (library || !legacy);
    let want: Option<Vec<&str>> = v.get("fields").and_then(|f| f.as_array()).map(|a| a.iter().filter_map(|x| x.as_str()).collect());
    ([library, home, b("pick6")], TIP_FIELDS.iter().map(|(k, _)| *k).filter(|k| want.as_ref().map_or(true, |w| w.contains(k))).collect())
}

/// A plain on/off setting in `state.json` (PlayDate's Library modal: Artwork Sources and Completion Sync).
struct Switch { key: &'static str, label: &'static str, hint: &'static str }
const SWITCHES: &[(&str, &[Switch])] = &[
    ("Artwork Sources", &[Switch { key: "art_prefer_fit", label: "Prefer art with the right shape over the source order", hint: "Tries the sources in order but takes the first image that fits the card (2:3 covers, 616x353 wide art). If none fits, the first image found is used. Turn off to always use the first source that has art." }]),
    ("Completion Sync", &[
        Switch { key: "auto_promote_unfinished", label: "Mark games as Unfinished when playtime > 0", hint: "On startup, promotes Never Played games to Unfinished if Steam shows any playtime. Disable if you use BLAEO to manage Never Played status." },
        Switch { key: "auto_complete_on_100pct", label: "Mark games as Completed at 100% achievements", hint: "Applies whenever achievement data is synced or re-scraped." },
        Switch { key: "auto_downgrade_completed", label: "Downgrade Completed games back to Beaten if no longer 100%", hint: "Catches games where the developer added achievements after you'd already 100%'d it." },
    ]),
];

pub struct SettingsDialog {
    /// The slider values as last saved (the dashed lines of the charts).
    saved_vals: HashMap<&'static str, f64>,
    /// A color band changed and is waiting for the pointer to be released before it is saved (a color drag would otherwise save on every frame).
    badges_dirty: bool,
    /// Pad: the first focus is moved from the close cross to the active tab (once).
    focused_once: bool, focus_tab: bool,
    /// `None` = the Settings list; `Some` = that entry's dialog.
    group: Option<Group>,
    /// Where the hover edit button sits and which pages show it (`edit_button`); its corner is not available to a badge.
    edit_button: Value,
    /// A badge icon being read from the file the user picked: (the slot, the saved file name or the reason it failed). `icon_msg` is the line under the list.
    icon_rx: Option<std::sync::mpsc::Receiver<(String, Result<String, String>)>>, icon_msg: Option<String>,
    /// The small logo shown beside each row of "Your own icons" (the chosen file, else the bundled one), loaded once per file.
    icon_prev: HashMap<String, Option<egui::TextureHandle>>,
    dir: PathBuf, form: crate::account_form::AccountForm, sgdb: String, status: Option<(String, bool)>,
    /// Account tab: the SteamGifts name, the show-key toggles, the Steam accounts DETECT found (when more than one) and the Sign in with Steam result on its way.
    sg_name: String, show_sgdb: bool,
    /// Tuning: the section open in its own dialog (None = the list of sections).
    tune_sec: Option<usize>,
    /// Appearance: the dialog open from its list (None = the list).
    app_sub: Option<AppSub>,
    /// Gamepad: 1 = Gamepad Controls, 2 = Gamepad Setup open in their own dialog (None = the list).
    pad_sub: Option<u8>,
    /// The Library group draws its sections on one page, so the sections' own scroll areas stand down.
    flat: bool,
    /// Advanced: the two update switches (`check_updates`, `beta_updates`).
    adv_auto: bool, adv_beta: bool,
    tab: Tab, vals: HashMap<&'static str, f64>, flags: HashMap<&'static str, bool>,
    /// What each (button, click) does: the saved rule's action, or None for no rule. One row per pair, so two rules can never claim the same one.
    mouse: Vec<(Button, Click, Option<String>)>,
    /// The platform order that decides which copy of a game is shown, as it is being edited.
    order: Vec<String>, hide_dups: bool,
    /// The card tooltip: shown on [Library, Home, Pick 6], and the details it includes.
    tip_pages: [bool; 3], tip_fields: Vec<&'static str>,
    /// The page Zest opens on: home, library or pick.
    startup: String,
    /// Card Outlines tab: `card_outlines` as being edited, the saved filters (id, name) a rule can use, and the add-rule form when open.
    outlines: Value, saved_filters: Vec<(String, String)>, co_form: Option<CoForm>,
    /// Gamepad tab: input on, dead zone %, first repeat ms, repeat ms.
    pad_on: bool, pad_nums: [f64; 3],
    /// Each action's button (see `gamepad::ACTIONS`), the row waiting for a button press, and what the last change did.
    pad_labels: std::collections::HashMap<String, crate::gamepad::PadLabels>, names_open: bool, pad_assign: Vec<Option<u8>>, pad_capture: Option<usize>, pad_msg: Option<String>, sdl_text: String, sdl_msg: Option<String>, pad_swap_xy: bool,
    /// The line under a Tuning section's buttons.
    tune_status: HashMap<&'static str, String>,
    dup_status: Option<String>,
    /// Every account in config.json, and the one asked to be removed (a second press confirms).
    accts: Vec<(Account, bool)>, remove_ask: Option<String>,
    appearance: crate::appearance::Appearance,
    /// Tag pins being edited (`tag_similarity_overrides`), the library to preview them on, the tag typed for a new pin, and the last preview.
    badges: Value, art_prefs: Value, art_plats: Vec<(String, String, bool)>, art_edit: Option<usize>, pins: serde_json::Map<String, Value>, saved_pins: serde_json::Map<String, Value>, tag_vocab: Option<Vec<String>>, db: Option<PathBuf>, pin_input: String, preview: Option<(String, pd_filter::tagaffinity::Lists)>,
}
pub enum SettingsOut {
    None, Close,
    /// Keys to write into `state.json`; `tags` / `reviews` ask for the matching recalculation afterwards.
    Apply { updates: Vec<(String, Value)>, tags: bool, reviews: bool, section: Option<&'static str> },
    /// A new platform order (best first) to save; the app re-points the duplicate links and says how many changed.
    Priority(Vec<String>),
    /// The theme, background or scale changed; the app reloads it.
    Appearance(crate::appearance::AppearanceOut),
    /// Close Settings and open the Home page editor.
    EditHome,
    /// Close Settings and open another dialog from the hamburger menu (`update`, `data`).
    Menu(&'static str),
}

impl SettingsDialog {
    pub fn open(dir: PathBuf, state: &Value, order: Vec<String>) -> Self {
        let a = account::load_account(&dir).unwrap_or_default();
        let dir_for_accts = dir.clone();
        let mut vals = HashMap::new();
        for s in SECTIONS { for k in s.knobs { vals.insert(k.key, state.get(k.key).and_then(|v| v.as_f64()).unwrap_or(k.def).clamp(k.min, k.max)); } }
        let mut flags = HashMap::new();
        for (_, sw) in SWITCHES { for s in *sw { flags.insert(s.key, state.get(s.key).and_then(|v| v.as_bool()).unwrap_or(true)); } }
        let rules = card_actions::rules_from_state(state);
        let mouse = card_actions::BUTTONS.iter().flat_map(|b| [Click::Single, Click::Double].map(|c| (*b, c))).map(|(b, c)| (b, c, rules.iter().find(|r| r.button == b && r.click == c).map(|r| r.action.clone()))).collect();
        let hide_dups = state.get("hide_duplicates").and_then(|v| v.as_bool()).unwrap_or(true);
        let (tip_pages, tip_fields) = read_tip(state.get("hover_tooltip").unwrap_or(&Value::Null));
        let startup = state.get("startup_page").and_then(|v| v.as_str()).filter(|p| matches!(*p, "home" | "library" | "pick")).unwrap_or("home").to_string();
        let pins = pd_filter::tagaffinity::normalize_overrides(state.get("tag_similarity_overrides").unwrap_or(&Value::Null)).as_object().cloned().unwrap_or_default();
        SettingsDialog { group: None, saved_vals: vals.clone(), outlines: state.get("card_outlines").filter(|v| v.is_object()).cloned().unwrap_or_else(default_outlines), saved_filters: state.get("saved_filters").and_then(|v| v.as_object()).map(|m| { let mut v: Vec<(String, String)> = m.iter().filter_map(|(name, sf)| Some((sf.get("id")?.as_str()?.to_string(), name.clone()))).collect(); v.sort_by(|a, b| a.1.to_lowercase().cmp(&b.1.to_lowercase())); v }).unwrap_or_default(), co_form: None, pad_on: state.get("gamepad_enabled").and_then(|v| v.as_bool()).unwrap_or(true), pad_nums: [state.get("gamepad_deadzone").and_then(|v| v.as_f64()).unwrap_or(35.0).clamp(0.0, 90.0), state.get("gamepad_repeat_initial_ms").and_then(|v| v.as_f64()).unwrap_or(400.0).clamp(50.0, 2000.0), state.get("gamepad_repeat_rate_ms").and_then(|v| v.as_f64()).unwrap_or(150.0).clamp(20.0, 1000.0)], badges_dirty: false, pad_labels: crate::gamepad::parse_labels(state), names_open: false, pad_assign: crate::gamepad::assignments(&crate::gamepad::parse_remaps(state)), pad_capture: None, pad_msg: None, sdl_text: crate::gamepad::parse_sdl_mappings(state).join("\n"), sdl_msg: None, pad_swap_xy: state.get("gamepad_swap_xy").and_then(|v| v.as_bool()).unwrap_or(false), focused_once: false, focus_tab: false, edit_button: state.get("edit_button").filter(|v| v.is_object()).cloned().unwrap_or_else(|| json!({ "corner": "top_right", "pages": { "library": true, "home": false, "pick6": false } })), icon_rx: None, icon_msg: None, icon_prev: HashMap::new(), badges: state.get("card_badges").filter(|v| v.is_object()).cloned().unwrap_or_else(default_badges), art_prefs: state.get("art_source_prefs").filter(|v| v.is_object()).cloned().unwrap_or_else(|| json!({})), art_plats: Vec::new(), art_edit: std::env::var_os("ZEST_ART_EDIT").map(|_| 0), saved_pins: pins.clone(), tag_vocab: None, pins, db: None, pin_input: String::new(), preview: None, mouse, order, hide_dups, tip_pages, tip_fields, startup, sgdb: account::sgdb_key(&dir).unwrap_or_default(), sg_name: account::sg_username(&dir), show_sgdb: false, tune_sec: None, app_sub: None, pad_sub: None, flat: false, adv_auto: state.get("check_updates").and_then(|v| v.as_bool()).unwrap_or(true), adv_beta: state.get("beta_updates").and_then(|v| v.as_bool()).unwrap_or(pd_filter::updater::VERSION.contains('-')), form: crate::account_form::AccountForm::from_account(a), dir, status: None, tab: match std::env::var("ZEST_SETTINGS_TAB").as_deref() { Ok("library") => Tab::Library, Ok("art") => Tab::Art, Ok("outlines") => Tab::Outlines, Ok("badges") => Tab::Badges, Ok("duplicates") => Tab::Duplicates, Ok("mouse") => Tab::Mouse, Ok("gamepad") => Tab::Gamepad, Ok("appearance") => Tab::Appearance, Ok("tuning") => Tab::Tuning, _ => Tab::Accounts }, vals, flags, tune_status: HashMap::new(), dup_status: None, accts: account::list_accounts(&dir_for_accts), remove_ask: None, appearance: crate::appearance::Appearance::new(dir_for_accts.clone(), state.get("ui_scale").and_then(|v| v.as_u64()).unwrap_or(100) as u32) }
    }
    /// The stores of the library as (platform, label, has its own art), for the Artwork Sources lists.
    pub fn with_art_platforms(mut self, p: Vec<(String, String, bool)>) -> Self { self.art_plats = p; self }
    /// The library the Tuning tag lists are previewed on (read-only).
    pub fn with_db(mut self, db: PathBuf) -> Self { self.db = Some(db); self }
    /// The liked / disliked lists for the sliders as they are now (PlayDate refreshes them as a slider moves).
    fn refresh_preview(&mut self) {
        let Some(db) = self.db.clone() else { return };
        let (rate, cap) = (self.vals["tag_similarity_min_library_rate"], self.vals["tag_similarity_playtime_cap_hours"]);
        let ov = Value::Object(self.pins.clone());
        let key = format!("{rate}|{cap}|{ov}");
        if self.preview.as_ref().map_or(false, |(k, _)| *k == key) { return; }
        let lists = rusqlite::Connection::open_with_flags(&db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).ok().and_then(|c| { c.busy_timeout(std::time::Duration::from_secs(5)).ok(); pd_filter::tagaffinity::compute(&c, rate, cap, Some(&ov)).ok() }).map(|aff| pd_filter::tagaffinity::lists(&aff, Some(&ov), 10)).unwrap_or_default();
        self.preview = Some((key, lists));
    }
    /// The app saved a Tuning section: say so (and how many games were rescored).
    pub fn saved(&mut self, section: &'static str, message: String) { self.tune_status.insert(section, message); }
    /// The app saved the platform order (and how many duplicate links it moved).
    pub fn priority_saved(&mut self, message: String) { self.dup_status = Some(message); }

    fn save(&mut self) {
        let id = self.form.steam_id.trim().to_string();
        let r = (|| -> Result<(), String> {
            if !id.is_empty() {
                if !id.chars().all(|c| c.is_ascii_digit()) || id.len() < 15 { return Err("The SteamID is the long number of your profile (17 digits), not the profile name.".into()); }
                account::save_account(&self.dir, &Account { steam_id: id.clone(), api_key: self.form.api_key.trim().to_string(), label: self.form.label.clone() }, true).map_err(|e| format!("Could not save: {}", e.kind() as u8))?;
                let db = self.dir.join(format!("games_{id}.db"));
                if !db.exists() { pd_filter::schema::init_db(&db).map_err(|e| format!("Could not make the library: {e}"))?; }
            }
            account::set_sgdb_key(&self.dir, &self.sgdb).map_err(|_| "Could not save".to_string())
        })();
        self.accts = account::list_accounts(&self.dir);
        self.status = Some(match r { Ok(()) => ("Saved. A new key is used by the next sync.".into(), true), Err(e) => (e, false) });
    }

    /// Re-reads the account list and the fields after a switch or removal.
    fn reload_account(&mut self) {
        self.accts = account::list_accounts(&self.dir);
        let a = account::load_account(&self.dir).unwrap_or_default();
        self.form = crate::account_form::AccountForm::from_account(a);
    }

    pub fn show(&mut self, ctx: &egui::Context) -> (SettingsOut, Option<String>) {
        // rows of side-by-side choices and a tab row: Up/Down must go by position, not step through the controls in creation order (which walked along a row)
        crate::ui_kit::PAD_GEOMETRY.store(true, std::sync::atomic::Ordering::Relaxed);
        // the pad's first press lands on the close cross (first in creation order); the active tab is the better start, and Down from it goes into the page
        if !self.focused_once && ctx.memory(|m| m.focused()).is_some() { self.focused_once = true; self.focus_tab = true; ctx.request_repaint(); }
        if self.form.poll(&self.accts) {   // a Steam sign-in finished: keep what it found, and say what Steam said about the key too
            let note = self.form.status.take();
            self.save();
            if let (Some((n, _)), Some((m, ok))) = (note, self.status.take()) { self.status = Some(if ok { (format!("{n} {m}"), true) } else { (m, false) }); }
        }
        // test hook for screenshots: open a Tuning section at once (ZEST_TUNE_SEC=<index>)
        if self.group.is_none() { if let Some(i) = std::env::var("ZEST_TUNE_SEC").ok().and_then(|v| v.parse::<usize>().ok()) { self.group = Some(Group::Tuning); self.tab = Tab::Tuning; self.tune_sec = Some(i); } }
        // test hook for screenshots: open the Appearance page at once
        if self.group.is_none() && std::env::var_os("ZEST_APPEARANCE").is_some() { self.group = Some(Group::Appearance); self.tab = Tab::Appearance; }
        // test hook for screenshots: open the Theme Editor at once
        if self.group.is_none() && std::env::var_os("ZEST_THEME_EDITOR").is_some() { self.group = Some(Group::Appearance); self.tab = Tab::Appearance; self.app_sub = Some(AppSub::Editor); }
        let accent = ui_kit::accent();
        let mut open_url = None;
        let mut save = false;
        let mut out = SettingsOut::None;
        if self.tab != Tab::Gamepad { crate::gamepad::set_test(false); }
        let Some(group) = self.group else {
            let mut pick = None;
            
            ui_kit::NO_FOOTER.store(true, std::sync::atomic::Ordering::Relaxed);
            let res = ui_kit::modal_full(ctx, "settings", "Settings", 22.5,
                |ui, r| { ui.label(ui_kit::muted(format!("v{}", pd_filter::updater::VERSION), 0.78 * r)); },
                |ui, r| { ui.spacing_mut().item_spacing.y = 0.4 * r; for g in Group::ALL.iter() { if ui_kit::settings_row(ui, g.title(), r, false).clicked() { pick = Some(*g); } } },
                |_, _| {});
            if let Some(g) = pick { self.group = Some(g); self.tab = g.tabs()[0].0; self.focus_tab = true; ctx.request_repaint(); return (SettingsOut::None, None); }
            return (if res.closed { SettingsOut::Close } else { SettingsOut::None }, None);
        };
        let title = match (group, self.tune_sec, self.app_sub, self.pad_sub) { (Group::Tuning, Some(i), _, _) => SECTIONS[i].title, (Group::Appearance, _, Some(a), _) => a.title(), (Group::Gamepad, _, _, Some(1)) => "Gamepad Controls", (Group::Gamepad, _, _, Some(_)) => "Gamepad Setup", _ => group.title() };
        let width = if (group == Group::Tuning && self.tune_sec.is_none()) || (group == Group::Appearance && self.app_sub.is_none()) || (group == Group::Gamepad && self.pad_sub.is_none()) { 24.0 } else if group == Group::Appearance && self.app_sub == Some(AppSub::Editor) { 62.0 } else { 56.0 };
        ui_kit::NO_FOOTER.store(true, std::sync::atomic::Ordering::Relaxed);
        let art_open = self.art_edit.is_some();   // Esc or a click outside that closes the artwork dialog must not also close this one
        let res = ui_kit::modal(ctx, "settings", title, width, |ui, r| {
            ui_kit::style_fields(ui);
            if group.tabs().len() > 1 && group != Group::Library { ui.horizontal(|ui| {
                for (t, name) in group.tabs().iter().copied() { let resp = ui_kit::pill(ui, name, self.tab == t, accent, 0.8 * r); if self.tab == t && std::mem::take(&mut self.focus_tab) { resp.request_focus(); } if resp.clicked() { self.tab = t; self.focus_tab = true; ui.ctx().request_repaint(); } }
            }); }
            ui.add_space(0.5 * r);
            // PlayDate's Library dialog is one page; the four sections it covers are drawn one after another in a single scroll area
            let flat = group == Group::Library;
            self.flat = flat;
            let tabs_now: Vec<Tab> = if flat { vec![Tab::Library, Tab::Art, Tab::Duplicates, Tab::Mouse] } else { vec![self.tab] };
            let outer_h = f32::INFINITY;
            let mut first_tab = true;
            let draw_tabs = |ui: &mut egui::Ui| {
            for tab_now in tabs_now {
            if !std::mem::take(&mut first_tab) { ui.add_space(1.0 * r); ui.separator(); ui.add_space(0.6 * r); }
            match tab_now {
                Tab::Appearance if self.app_sub.is_some() => {
                    use crate::appearance::Part;
                    match self.app_sub {
                        Some(AppSub::Background) | Some(AppSub::Theme) | Some(AppSub::Editor) => {
                            let part = match self.app_sub { Some(AppSub::Theme) => Part::Theme, Some(AppSub::Editor) => Part::Editor, _ => Part::Background };
                            let o = egui::ScrollArea::vertical().id_salt("appearance_sub").max_height(f32::INFINITY).auto_shrink([false, true]).show(ui, |ui| self.appearance.show(ui, r, accent, part)).inner;
                            if o != crate::appearance::AppearanceOut::None { out = SettingsOut::Appearance(o); }
                            if std::mem::take(&mut self.appearance.done) { self.app_sub = None; }
                        }
                        Some(AppSub::Outlines) => { if self.outlines_ui(ui, r, accent) { out = SettingsOut::Apply { updates: vec![("card_outlines".to_string(), self.outlines.clone())], tags: false, reviews: false, section: None }; } }
                        _ => { if self.badges_ui(ui, r, accent) { out = SettingsOut::Apply { updates: vec![("card_badges".to_string(), self.badges.clone()), ("edit_button".to_string(), self.edit_button.clone())], tags: false, reviews: false, section: None }; } }
                    }
                }
                Tab::Appearance => {
                    let small = |ui: &mut egui::Ui, t: &str| { ui.add_space(0.4 * r); ui.label(egui::RichText::new(t).size(0.72 * r).color(ui_kit::MUTED())); ui.add_space(0.2 * r); };
                    egui::ScrollArea::vertical().id_salt("appearance_menu").max_height(f32::INFINITY).auto_shrink([false, true]).show(ui, |ui| {
                        if ui_kit::settings_row(ui, "Edit Home Layout", r, false).clicked() { out = SettingsOut::EditHome; }
                        ui.add_space(0.5 * r);
                        for (name, sub) in [("Background & Opacity", AppSub::Background), ("Theme", AppSub::Theme), ("Theme Editor", AppSub::Editor)] {
                            if ui_kit::settings_row(ui, name, r, false).clicked() { if sub == AppSub::Editor { self.appearance.discard(); } self.app_sub = Some(sub); }   // the editor starts from the saved theme
                            ui.add_space(0.3 * r);
                        }
                        small(ui, "STARTUP PAGE");
                        ui.horizontal(|ui| {
                            for (key, name) in [("home", "HOME"), ("library", "LIBRARY"), ("pick", "PICK 6")] {
                                if ui_kit::pill(ui, name, self.startup == key, accent, 0.8 * r).clicked() && self.startup != key { self.startup = key.to_string(); out = SettingsOut::Apply { updates: vec![("startup_page".to_string(), json!(key))], tags: false, reviews: false, section: None }; }
                            }
                        });
                        small(ui, "UI SCALE");
                        ui.label(ui_kit::muted("Increase if text looks small on a high-DPI display.", 0.78 * r));
                        let o = self.appearance.show(ui, r, accent, crate::appearance::Part::Scale);
                        if o != crate::appearance::AppearanceOut::None { out = SettingsOut::Appearance(o); }
                        small(ui, "CARD OUTLINES");
                        ui.horizontal(|ui| {
                            for (k, name, def) in [("home", "HOME", false), ("library", "LIBRARY", true), ("pick6", "PICK 6", true)] {
                                let on = self.outlines.get("enabled").and_then(|e| e.get(k)).and_then(|v| v.as_bool()).unwrap_or(def);
                                if ui_kit::pill(ui, name, on, accent, 0.75 * r).clicked() {
                                    if !self.outlines.get("enabled").map_or(false, |e| e.is_object()) { self.outlines["enabled"] = json!({}); }
                                    self.outlines["enabled"][k] = json!(!on);
                                    out = SettingsOut::Apply { updates: vec![("card_outlines".to_string(), self.outlines.clone())], tags: false, reviews: false, section: None };
                                }
                            }
                            if ui_kit::pill(ui, "EDIT RULES", false, accent, 0.75 * r).clicked() { self.app_sub = Some(AppSub::Outlines); }
                        });
                        small(ui, "CARD BADGES");
                        ui.horizontal(|ui| {
                            for (k, name) in [("home", "HOME"), ("library", "LIBRARY"), ("pick6", "PICK 6")] {
                                let on = self.badges.get("enabled").and_then(|e| e.get(k)).and_then(|v| v.as_bool()).unwrap_or(true);
                                if ui_kit::pill(ui, name, on, accent, 0.75 * r).clicked() {
                                    if !self.badges.get("enabled").map_or(false, |e| e.is_object()) { self.badges["enabled"] = json!({}); }
                                    self.badges["enabled"][k] = json!(!on);
                                    out = SettingsOut::Apply { updates: vec![("card_badges".to_string(), self.badges.clone()), ("edit_button".to_string(), self.edit_button.clone())], tags: false, reviews: false, section: None };
                                }
                            }
                            if ui_kit::pill(ui, "CONFIGURE", false, accent, 0.75 * r).clicked() { self.app_sub = Some(AppSub::Badges); }
                        });
                        small(ui, "EDIT BUTTON");
                        if self.edit_button_ui(ui, r, accent) { out = SettingsOut::Apply { updates: vec![("card_badges".to_string(), self.badges.clone()), ("edit_button".to_string(), self.edit_button.clone())], tags: false, reviews: false, section: None }; }
                    });
                }
                Tab::Accounts => {
                    {   // the dialog itself scrolls when the window is too short (ui_kit::fit_scroll)
                    if self.accts.len() > 1 || self.accts.iter().any(|(a, _)| a.steam_id != self.form.steam_id) {
                        ui.label(egui::RichText::new("Accounts").size(1.0 * r).strong().color(crate::ui_kit::heading()));
                        let mut act: Option<(&'static str, String)> = None;
                        for (a, active) in &self.accts {
                            ui.horizontal(|ui| {
                                let name = if a.label.is_empty() { a.steam_id.clone() } else { format!("{} ({})", a.label, a.steam_id) };
                                ui.label(egui::RichText::new(name).size(0.88 * r).color(if *active { accent } else { egui::Color32::from_gray(0xc8) }));
                                if *active { ui.label(ui_kit::muted("ACTIVE", 0.75 * r)); }
                                else if ui_kit::button(ui, "SWITCH", Kind::Plain, accent, r).clicked() { act = Some(("switch", a.steam_id.clone())); }
                                let asking = self.remove_ask.as_deref() == Some(a.steam_id.as_str());
                                if ui_kit::button(ui, if asking { "REMOVE? PRESS AGAIN" } else { "REMOVE" }, Kind::Plain, accent, r).clicked() { act = Some(("remove", a.steam_id.clone())); }
                            });
                        }
                        if let Some((what, id)) = act {
                            if what == "switch" {
                                self.status = Some(match account::switch_account(&self.dir, &id) { Ok(()) => { self.reload_account(); ("Switched. Restart Zest to open this account's library.".to_string(), true) } Err(e) => (e, false) });
                            } else if self.remove_ask.as_deref() == Some(id.as_str()) {
                                self.remove_ask = None;
                                self.status = Some(match account::remove_account(&self.dir, &id) { Ok(_) => { self.reload_account(); ("Removed. The library file stays on disk; restart Zest if the active account changed.".to_string(), true) } Err(e) => (e, false) });
                            } else { self.remove_ask = Some(id); }
                        }
                        ui.add_space(0.35 * r);
                    }
                    if ui_kit::button(ui, "+ ADD ACCOUNT", Kind::Plain, accent, r).clicked() { self.form.clear(); self.status = Some(("Enter the new account's SteamID and key, then SAVE. Restart Zest to open its library; its first sync builds it.".to_string(), true)); }
                    ui.add_space(0.25 * r);
                    if let Some(u) = self.form.ui(ui, r, accent) { open_url = Some(u.to_string()); }
                    if let Some(m) = self.form.status.take() { self.status = Some(m); }
                    ui.horizontal(|ui| {
                        if ui_kit::button(ui, "SAVE", Kind::Plain, accent, r).clicked() { save = true; }
                        if let Some((m, ok)) = &self.status { ui.label(egui::RichText::new(m).size(0.85 * r).color(if *ok { egui::Color32::from_rgb(0x8b, 0xb4, 0x2c) } else { crate::ui_kit::text_danger() })); }
                    });
                    ui.add_space(0.35 * r);
                    ui.separator();
                    ui.label(ui_kit::muted("SteamGridDB Key (optional, shared across accounts)", 0.8 * r));
                    ui.label(ui_kit::muted("Used for higher quality artwork when Steam's is missing, and for the cover picker.", 0.78 * r));
                    ui.horizontal(|ui| {
                        let w = (ui.available_width() - 7.0 * r).max(4.0 * r);
                        ui.add(egui::TextEdit::singleline(&mut self.sgdb).password(!self.show_sgdb).desired_width(w).font(egui::FontId::proportional(0.95 * r)));
                        if ui_kit::button(ui, if self.show_sgdb { "HIDE" } else { "SHOW" }, Kind::Plain, accent, r).clicked() { self.show_sgdb = !self.show_sgdb; }
                    });
                    ui.horizontal(|ui| {
                        if ui_kit::button(ui, "SAVE SGDB KEY", Kind::Plain, accent, r).clicked() {
                            self.status = Some(match account::set_sgdb_key(&self.dir, &self.sgdb) { Ok(()) => ("SteamGridDB key saved.".to_string(), true), Err(_) => ("Could not save the SteamGridDB key.".to_string(), false) });
                        }
                        if ui_kit::button(ui, "GET A KEY", Kind::Plain, accent, r).clicked() { open_url = Some("https://www.steamgriddb.com/profile/preferences/api".to_string()); }
                    });
                    ui.add_space(0.35 * r);
                    ui.separator();
                    ui.label(ui_kit::muted("SteamGifts Username (optional, used in the PAGYWOSG qualifications panel)", 0.8 * r));
                    ui.add(egui::TextEdit::singleline(&mut self.sg_name).desired_width(f32::INFINITY).font(egui::FontId::proportional(0.95 * r)));
                    if ui_kit::button(ui, "SAVE", Kind::Plain, accent, r).clicked() {
                        self.status = Some(match account::set_sg_username(&self.dir, &self.sg_name) { Ok(()) => ("SteamGifts name saved.".to_string(), true), Err(e) => (format!("Could not save: {e}. Save the Steam account first."), false) });
                    }
                                    }
                }
                Tab::Library => {
                    for (title, switches) in SWITCHES {
                        ui.label(egui::RichText::new(*title).size(1.0 * r).strong().color(crate::ui_kit::heading()));
                        for s in *switches {
                            let mut on = self.flags[s.key];
                            if ui.checkbox(&mut on, egui::RichText::new(s.label).size(0.9 * r)).changed() { self.flags.insert(s.key, on); out = SettingsOut::Apply { updates: vec![(s.key.to_string(), json!(on))], tags: false, reviews: false, section: None }; }
                            ui.label(ui_kit::muted(s.hint, 0.78 * r));
                            ui.add_space(0.3 * r);
                        }
                        ui.add_space(0.5 * r);
                    }
                    ui.label(ui_kit::muted("The other settings from PlayDate are not in Zest yet.", 0.75 * r));
                }
                Tab::Badges => {
                    if self.badges_ui(ui, r, accent) { out = SettingsOut::Apply { updates: vec![("card_badges".to_string(), self.badges.clone()), ("edit_button".to_string(), self.edit_button.clone())], tags: false, reviews: false, section: None }; }
                }
                Tab::Art => {
                    if let Some(u) = self.art_ui(ui, r, accent) { out = SettingsOut::Apply { updates: vec![("art_source_prefs".to_string(), u)], tags: false, reviews: false, section: None }; }
                }
                Tab::Duplicates => {
                    ui.label(egui::RichText::new("Duplicate Games").size(1.0 * r).strong().color(crate::ui_kit::heading()));
                    if ui.checkbox(&mut self.hide_dups, egui::RichText::new("Hide duplicate entries").size(0.9 * r)).changed() { out = SettingsOut::Apply { updates: vec![("hide_duplicates".to_string(), json!(self.hide_dups))], tags: false, reviews: false, section: None }; }
                    ui.label(ui_kit::muted("A game you own in several libraries is shown once, as its best-ranked copy. Turn this off to show every copy.", 0.78 * r));
                    ui.add_space(0.7 * r);
                    ui.label(egui::RichText::new("Platform priority").size(1.0 * r).strong().color(crate::ui_kit::heading()));
                    ui.label(ui_kit::muted("The top platform wins: its copy is the one shown. Move a platform up or down, then save. Saving re-points the existing duplicate links to match.", 0.8 * r));
                    ui.add_space(0.3 * r);
                    let n = self.order.len();
                    let mut mv: Option<(usize, usize)> = None;
                    for (i, p) in self.order.iter().enumerate() {
                        ui.horizontal(|ui| {
                            ui.allocate_ui_with_layout(egui::vec2(2.0 * r, 1.6 * r), egui::Layout::left_to_right(egui::Align::Center), |ui| { ui.set_min_width(2.0 * r); ui.label(ui_kit::muted(format!("{}", i + 1), 0.85 * r)); });
                            ui.allocate_ui_with_layout(egui::vec2(14.0 * r, 1.6 * r), egui::Layout::left_to_right(egui::Align::Center), |ui| { ui.set_min_width(14.0 * r); ui.label(egui::RichText::new(crate::home::platform_label(p)).size(0.9 * r).color(ui_kit::TEXT())); });
                            // the first can only go down and the last only up; the other button stays, dimmed, so the columns line up
                            if ui.add_enabled_ui(i > 0, |ui| ui_kit::button(ui, "UP", Kind::Plain, accent, r)).inner.clicked() { mv = Some((i, i - 1)); }
                            if ui.add_enabled_ui(i + 1 < n, |ui| ui_kit::button(ui, "DOWN", Kind::Plain, accent, r)).inner.clicked() { mv = Some((i, i + 1)); }
                        });
                    }
                    if let Some((a, b)) = mv { self.order.swap(a, b); self.dup_status = None; }
                    ui.add_space(0.5 * r);
                    ui.horizontal(|ui| {
                        if ui_kit::button(ui, "SAVE ORDER", Kind::Primary, accent, r).clicked() { out = SettingsOut::Priority(self.order.clone()); self.dup_status = Some("Saving...".into()); }
                        if let Some(m) = &self.dup_status { ui.label(ui_kit::muted(m.clone(), 0.82 * r)); }
                    });
                }
                Tab::Outlines => {
                    if self.outlines_ui(ui, r, accent) { out = SettingsOut::Apply { updates: vec![("card_outlines".to_string(), self.outlines.clone())], tags: false, reviews: false, section: None }; }
                }
                Tab::Gamepad => {
                    let part = self.pad_sub.unwrap_or(0);
                    let upd = egui::ScrollArea::vertical().id_salt(("gamepad_tab", part)).max_height(f32::INFINITY).auto_shrink([false, true]).show(ui, |ui| self.gamepad_ui(ui, r, accent, part)).inner;
                    if let Some(u) = upd { out = SettingsOut::Apply { updates: u, tags: false, reviews: false, section: None }; }
                }
                Tab::Mouse => ui_kit::maybe_scroll(ui, !self.flat, f32::INFINITY, "mouse_tab", |ui| {
                    ui.label(egui::RichText::new("Mouse Actions").size(1.0 * r).strong().color(crate::ui_kit::heading()));
                    ui.label(ui_kit::muted("What each mouse button does on a game cover, on every page. A button with a double click rule holds its single click back for a moment to see whether a second one follows. Delete and uninstall are deliberately not actions, so a stray click cannot remove anything.", 0.8 * r));
                    ui.add_space(0.4 * r);
                    let mut changed = false;
                    for (i, (b, c, act)) in self.mouse.iter_mut().enumerate() {
                        ui.horizontal(|ui| {
                            ui.allocate_ui_with_layout(egui::vec2(20.0 * r, 1.6 * r), egui::Layout::left_to_right(egui::Align::Center), |ui| { ui.set_min_width(20.0 * r); ui.label(egui::RichText::new(format!("{}, {}", b.label(), c.label())).size(0.88 * r).color(ui_kit::TEXT())); });
                            let text = act.as_deref().map(card_actions::action_label).unwrap_or("(no rule)");
                            egui::ComboBox::from_id_salt(("mouse_rule", i)).width(16.0 * r).height(24.0 * r).selected_text(egui::RichText::new(text).size(0.88 * r).color(ui_kit::TEXT())).show_ui(ui, |ui| {
                                if ui.selectable_label(act.is_none(), "(no rule)").clicked() && act.is_some() { *act = None; changed = true; }
                                for a in card_actions::ACTIONS { if ui.selectable_label(act.as_deref() == Some(*a), card_actions::action_label(a)).clicked() && act.as_deref() != Some(*a) { *act = Some(a.to_string()); changed = true; } }
                            });
                        });
                    }
                    if changed {
                        let rules: Vec<card_actions::Rule> = self.mouse.iter().filter_map(|(b, c, a)| a.clone().map(|action| card_actions::Rule { button: *b, click: *c, action })).collect();
                        out = SettingsOut::Apply { updates: vec![("card_click_rules".to_string(), card_actions::rules_json(&rules))], tags: false, reviews: false, section: None };
                    }
                    ui.add_space(0.4 * r);
                    ui.label(ui_kit::muted("The side buttons are the thumb buttons some mice have. Changes apply at once.", 0.78 * r));
                    ui.add_space(0.8 * r);
                    ui.label(egui::RichText::new("Card Tooltip").size(1.0 * r).strong().color(crate::ui_kit::heading()));
                    ui.label(ui_kit::muted("Show an info tooltip when hovering a game cover. Off by default. Choose the pages it appears on, and the details to include. A mouse action above that shows the tooltip works on any page whatever is chosen here.", 0.8 * r));
                    let mut tip_changed = false;
                    ui.horizontal(|ui| {
                        for (i, name) in ["Library page (grid view)", "Home page", "Pick 6 page"].iter().enumerate() { if ui.checkbox(&mut self.tip_pages[i], egui::RichText::new(*name).size(0.88 * r)).changed() { tip_changed = true; } }
                    });
                    ui.add_space(0.2 * r);
                    egui::Grid::new("tip_fields").num_columns(2).spacing([2.0 * r, 0.2 * r]).show(ui, |ui| {
                        for (n, (key, label)) in TIP_FIELDS.iter().enumerate() {
                            let mut on = self.tip_fields.contains(key);
                            if ui.checkbox(&mut on, egui::RichText::new(*label).size(0.84 * r)).changed() { tip_changed = true; if on { self.tip_fields.push(*key); } else { self.tip_fields.retain(|k| k != key); } }
                            if n % 2 == 1 { ui.end_row(); }
                        }
                    });
                    if tip_changed {
                        let fields: Vec<&str> = TIP_FIELDS.iter().map(|(k, _)| *k).filter(|k| self.tip_fields.contains(k)).collect();
                        out = SettingsOut::Apply { updates: vec![("hover_tooltip".to_string(), json!({"library": self.tip_pages[0], "home": self.tip_pages[1], "pick6": self.tip_pages[2], "fields": fields}))], tags: false, reviews: false, section: None };
                    }
                }),
                Tab::Advanced => {
                    let small = |ui: &mut egui::Ui, t: &str| { ui.add_space(0.4 * r); ui.label(egui::RichText::new(t).size(0.72 * r).color(ui_kit::MUTED())); ui.add_space(0.2 * r); };
                    ui.spacing_mut().item_spacing.y = 0.4 * r;
                    small(ui, "UPDATES");
                    if ui_kit::settings_row(ui, &format!("Auto-check Updates: {}", if self.adv_auto { "ON" } else { "OFF" }), r, false).clicked() { self.adv_auto = !self.adv_auto; out = SettingsOut::Apply { updates: vec![("check_updates".to_string(), json!(self.adv_auto))], tags: false, reviews: false, section: None }; }
                    if ui_kit::settings_row(ui, &format!("Beta updates: {}", if self.adv_beta { "ON" } else { "OFF" }), r, false).clicked() { self.adv_beta = !self.adv_beta; out = SettingsOut::Apply { updates: vec![("beta_updates".to_string(), json!(self.adv_beta))], tags: false, reviews: false, section: None }; }
                    if ui_kit::settings_row(ui, "Check for Updates", r, false).clicked() { out = SettingsOut::Menu("update"); }
                    small(ui, "SUPPORT");
                    if ui_kit::settings_row(ui, "Send Log to Developer", r, false).clicked() { out = SettingsOut::Menu("send_log"); }
                    if ui_kit::settings_row(ui, "Open Data Folder", r, false).clicked() { open_url = Some(self.dir.to_string_lossy().to_string()); }
                }
                Tab::Tuning if self.tune_sec.is_none() => {
                    ui.label(ui_kit::muted("Adjust the formulas behind tag similarity, review scores and Pick 6. Solid lines show your current slider values; dashed lines show what's saved.", 0.8 * r));
                    ui.add_space(0.5 * r);
                    for (i, sec) in SECTIONS.iter().enumerate() { if ui_kit::settings_row(ui, sec.title, r, false).clicked() { self.tune_sec = Some(i); } ui.add_space(0.3 * r); }
                }
                Tab::Tuning => {
                    egui::ScrollArea::vertical().max_height(f32::INFINITY).auto_shrink([false, true]).show(ui, |ui| {
                        let open_sec = self.tune_sec;
                        for sec in SECTIONS.iter().enumerate().filter(|(i, _)| Some(*i) == open_sec).map(|(_, s)| s) {
                            ui.label(ui_kit::muted(sec.desc, 0.8 * r));
                            ui.add_space(0.3 * r);
                            for k in sec.knobs {
                                let cur = self.vals[k.key];
                                ui.horizontal(|ui| {
                                    ui.allocate_ui_with_layout(egui::vec2(14.0 * r, 1.4 * r), egui::Layout::left_to_right(egui::Align::Center), |ui| { ui.set_min_width(14.0 * r); ui.add(egui::Label::new(egui::RichText::new(k.label).size(0.88 * r).color(ui_kit::TEXT())).truncate()); });
                                    ui.spacing_mut().slider_width = 16.0 * r;
                                    let mut v = if k.log { (cur.max(1.0).log10() * 100.0).round() } else { cur };
                                    let (lo, hi) = if k.log { (k.min.log10() * 100.0, k.max.log10() * 100.0) } else { (k.min, k.max) };
                                    let step = if k.log { 1.0 } else { k.step };
                                    let key = k.key;
                                    let show = move |x: f64| fmt_value(key, if k.log { 10f64.powf(x / 100.0).round() } else { x }, k.unit);
                                    let changed = ui_kit::add_slider(ui, egui::Slider::new(&mut v, lo..=hi).step_by(step).custom_formatter(move |x, _| show(x)).custom_parser(|t| t.trim().trim_end_matches(|c: char| !c.is_ascii_digit() && c != '.').parse::<f64>().ok())).changed();
                                    ui_kit::slider_value(ui, show(v), 0.88 * r);
                                    if changed { self.vals.insert(k.key, if k.log { 10f64.powf(v / 100.0).round().clamp(k.min, k.max) } else { v }); }
                                });
                                ui.label(ui_kit::muted(k.hint, 0.76 * r));
                                if let Some(c) = chart_of(k.key) { self.chart_ui(ui, r, accent, c); }
                                ui.add_space(0.25 * r);
                            }
                            if sec.id == "tags" { self.pin_ui(ui, r, accent); }
                            ui.horizontal(|ui| {
                                if ui_kit::button(ui, "SAVE", Kind::Primary, accent, r).clicked() {
                                    let mut updates: Vec<(String, Value)> = sec.knobs.iter().map(|k| (k.key.to_string(), knob_json(k, self.vals[k.key]))).collect();
                                    if sec.id == "tags" { updates.push(("tag_similarity_overrides".to_string(), Value::Object(self.pins.clone()))); }
                                    out = SettingsOut::Apply { updates, tags: sec.id == "tags", reviews: sec.id == "reviews", section: Some(sec.id) };
                                    for k in sec.knobs { self.saved_vals.insert(k.key, self.vals[k.key]); } self.saved_pins = self.pins.clone();
                                    self.tune_status.insert(sec.id, "Saving...".into());
                                }
                                if ui_kit::button(ui, "RESET TO DEFAULTS", Kind::Plain, accent, r).clicked() { for k in sec.knobs { self.vals.insert(k.key, k.def); } if sec.id == "tags" { self.pins.clear(); } self.tune_status.insert(sec.id, "Defaults restored: press SAVE to keep them.".into()); }
                                if let Some(m) = self.tune_status.get(sec.id) { ui.label(ui_kit::muted(m.clone(), 0.82 * r)); }
                            });
                            ui.add_space(1.0 * r);
                        }
                    });
                }
            }
            }
            };
            if flat { egui::ScrollArea::vertical().id_salt("library_page").max_height(outer_h).auto_shrink([false, true]).show(ui, |ui| draw_tabs(ui)); } else { draw_tabs(ui); }
        });
        if save { self.save(); }
        if let Some(u) = self.art_edit_dialog(ctx) { out = SettingsOut::Apply { updates: vec![("art_source_prefs".to_string(), u)], tags: false, reviews: false, section: None }; }
        let res = ui_kit::ModalOut { closed: res.closed && !art_open, inner: res.inner };
        // leaving a Tuning dialog without SAVE drops what was changed in it (the sliders and the tag pins go back to what is saved)
        if res.closed && matches!(out, SettingsOut::None) && self.tune_sec.take().is_some() { self.vals = self.saved_vals.clone(); self.pins = self.saved_pins.clone(); return (SettingsOut::None, open_url); }
        if res.closed && matches!(out, SettingsOut::None) && self.app_sub.is_some() {
            // closing the Theme Editor drops its draft: the app goes back to the saved theme
            let was_editor = self.app_sub == Some(AppSub::Editor);
            self.app_sub = None;
            if was_editor { self.appearance.discard(); return (SettingsOut::Appearance(crate::appearance::AppearanceOut::Theme), open_url); }
            return (SettingsOut::None, open_url);
        }
        if res.closed && matches!(out, SettingsOut::None) && self.pad_sub.take().is_some() { self.pad_capture = None; crate::gamepad::cancel_capture(); return (SettingsOut::None, open_url); }
        if res.closed && matches!(out, SettingsOut::None) { self.group = None; self.focused_once = false; return (SettingsOut::None, open_url); }
        (out, open_url)
    }
}

/// PlayDate's `DEFAULT_CARD_BADGES` (the parts this tab edits; thresholds and uploaded icons are kept when present).
fn default_badges() -> Value {
    json!({"enabled": {"library": true, "home": true, "pick6": true}, "slots": {"top_left": null, "top_right": null, "bottom_left": null, "bottom_right": null}, "platform_fallback": "default", "scale": 1.0,
        "icons": {"installed": null, "platform": {}},
        "achievement_percent": {"thresholds": [{"min": 0, "color": "#e74c3c"}, {"min": 50, "color": "#f1c40f"}, {"min": 90, "color": "#2ecc71"}]},
        "review_score": {"mode": "weighted", "thresholds": [{"min": 0, "color": "#e74c3c"}, {"min": 50, "color": "#f1c40f"}, {"min": 80, "color": "#2ecc71"}]},
        "hltb": {"mode": "main"}})
}
/// Swatches offered for a badge color band (the first three are PlayDate's defaults).
const BAND_COLORS: [[u8; 3]; 8] = [[0xe7, 0x4c, 0x3c], [0xf1, 0xc4, 0x0f], [0x2e, 0xcc, 0x71], [0xe6, 0x7e, 0x22], [0x1a, 0xbc, 0x9c], [0x34, 0x98, 0xdb], [0x9b, 0x59, 0xb6], [0x88, 0x88, 0x88]];
const BADGE_SLOTS: [(&str, &str); 4] = [("top_left", "Top left"), ("top_right", "Top right"), ("bottom_left", "Bottom left"), ("bottom_right", "Bottom right")];
const BADGE_FIELDS: [(&str, &str); 6] = [("", "NONE"), ("installed", "INSTALLED"), ("platform", "PLATFORM"), ("achievement_percent", "ACHIEVEMENT %"), ("review_score", "REVIEW SCORE"), ("hltb", "HLTB TIME")];
/// PlayDate's `TUNE_CHARTS`: the graph under a slider, drawn from the formulas the app really uses (keep them in step with `pick.rs` / `tagaffinity.rs` / `recalc.rs`).
struct Chart { name: &'static str, x_min: f64, x_max: f64, log: bool, x_label: &'static str, y_label: &'static str, series: &'static [&'static str] }
const CHARTS: &[Chart] = &[
    Chart { name: "smoothing", x_min: 0.0, x_max: 30.0, log: false, x_label: "tags on a game", y_label: "share of tag score kept", series: &[""] },
    Chart { name: "playtime", x_min: 0.0, x_max: 200.0, log: false, x_label: "hours played", y_label: "weight in profile", series: &[""] },
    Chart { name: "review", x_min: 1.0, x_max: 100000.0, log: true, x_label: "review count", y_label: "weighted score", series: &["95% positive", "75% positive", "30% positive"] },
    Chart { name: "staleness", x_min: 0.0, x_max: 2000.0, log: false, x_label: "days since last played", y_label: "staleness score", series: &[""] },
    Chart { name: "recency", x_min: 0.0, x_max: 50.0, log: false, x_label: "game age (years)", y_label: "recency score", series: &[""] },
    Chart { name: "hltbLong", x_min: 0.0, x_max: 300.0, log: false, x_label: "hours to beat", y_label: "Length score (prefer long)", series: &[""] },
    Chart { name: "hltbShort", x_min: 0.0, x_max: 100.0, log: false, x_label: "hours to beat", y_label: "Length score (prefer short)", series: &[""] },
];
/// Which slider shows which chart.
fn chart_of(key: &str) -> Option<&'static Chart> {
    let n = match key {
        "tag_similarity_smoothing_k" => "smoothing", "tag_similarity_playtime_cap_hours" => "playtime", "review_half_trust_count" => "review",
        "pick6_staleness_curve" => "staleness", "pick6_recency_curve" => "recency", "pick6_hltb_long_curve" => "hltbLong", "pick6_hltb_short_curve" => "hltbShort", _ => return None,
    };
    CHARTS.iter().find(|c| c.name == n)
}
/// The height (0..1) of series `s` of a chart at `x`, for the slider values `v`.
fn chart_y(chart: &str, s: usize, x: f64, v: &dyn Fn(&str) -> f64) -> f64 {
    let y = match chart {
        "smoothing" => x / (x + v("tag_similarity_smoothing_k")),
        "playtime" => if x <= 0.0 { 0.0 } else { (x.ln_1p() / v("tag_similarity_playtime_cap_hours").ln_1p()).min(1.0) },
        "review" => { let p = [0.95, 0.75, 0.30][s.min(2)]; let half = v("review_half_trust_count").max(2.0); p - (p - 0.5) * 2f64.powf(-(x + 1.0).ln() / half.ln()) }
        "staleness" => { let cap = v("pick6_staleness_cap_days"); (x.min(cap) / cap).powf(v("pick6_staleness_curve")) }
        "recency" => { let cap = v("pick6_recency_cap_years"); (1.0 - x.min(cap) / cap).powf(v("pick6_recency_curve")) }
        "hltbLong" => { let floor = v("pick6_hltb_long_floor_hours"); let span = (v("pick6_hltb_long_cap_hours") - floor).max(1.0); ((x - floor).clamp(0.0, span) / span).powf(v("pick6_hltb_long_curve")) }
        "hltbShort" => { let cap = v("pick6_hltb_short_cap_hours"); (x.min(cap) / cap).powf(v("pick6_hltb_short_curve")) }
        _ => 0.0,
    };
    y.clamp(0.0, 1.0)
}

const ART_KINDS: [(&str, &str); 3] = [("vertical", "Vertical covers"), ("horizontal", "Wide art"), ("icon", "Icons")];
fn art_source_label(s: &str) -> &'static str { match s { "store" => "The store's own art", "sgdb" => "SteamGridDB", _ => "Steam" } }

impl SettingsDialog {
    /// A Tuning chart: the saved values dashed and faint, the slider's current values solid.
    fn chart_ui(&self, ui: &mut egui::Ui, r: f32, accent: egui::Color32, c: &Chart) {
        ui.label(ui_kit::muted(c.y_label, 0.72 * r));
        let (w, h) = (ui.available_width().min(26.0 * r), 7.5 * r);
        let (rect, _) = ui.allocate_exact_size(egui::vec2(w, h), egui::Sense::hover());
        let p = ui.painter();
        let plot = egui::Rect::from_min_max(egui::pos2(rect.min.x + 2.2 * r, rect.min.y + 0.4 * r), egui::pos2(rect.max.x - 0.5 * r, rect.max.y - 1.9 * r));
        let font = egui::FontId::proportional(0.62 * r);
        for y in [0.0, 0.5, 1.0] {
            let py = plot.max.y - y * plot.height();
            p.line_segment([egui::pos2(plot.min.x, py), egui::pos2(plot.max.x, py)], egui::Stroke::new(1.0_f32, ui_kit::RULE()));
            p.text(egui::pos2(plot.min.x - 0.3 * r, py), egui::Align2::RIGHT_CENTER, format!("{y}"), font.clone(), ui_kit::MUTED());
        }
        let at = |t: f64| if c.log { c.x_min * (c.x_max / c.x_min).powf(t) } else { c.x_min + (c.x_max - c.x_min) * t };
        for (t, al) in [(0.0, egui::Align2::LEFT_TOP), (0.5, egui::Align2::CENTER_TOP), (1.0, egui::Align2::RIGHT_TOP)] {
            let xv = at(t); let txt = if xv >= 1000.0 { format!("{}k", (xv / 1000.0).round()) } else { format!("{}", xv.round()) };
            p.text(egui::pos2(plot.min.x + t as f32 * plot.width(), plot.max.y + 0.2 * r), al, txt, font.clone(), ui_kit::MUTED());
        }
        p.text(egui::pos2(plot.center().x, rect.max.y), egui::Align2::CENTER_BOTTOM, c.x_label, font.clone(), ui_kit::MUTED());
        let colors = [accent, egui::Color32::from_rgb(0xf1, 0xc4, 0x0f), egui::Color32::from_rgb(0xe7, 0x4c, 0x3c)];
        let cur = |k: &str| self.vals.get(k).copied().unwrap_or(0.0);
        let sav = |k: &str| self.saved_vals.get(k).copied().unwrap_or_else(|| cur(k));
        for (si, _) in c.series.iter().enumerate() {
            for (vals, saved) in [(&sav as &dyn Fn(&str) -> f64, true), (&cur as &dyn Fn(&str) -> f64, false)] {
                let pts: Vec<egui::Pos2> = (0..=80).map(|i| { let t = i as f64 / 80.0; egui::pos2(plot.min.x + t as f32 * plot.width(), plot.max.y - chart_y(c.name, si, at(t), vals) as f32 * plot.height()) }).collect();
                let col = colors[si % 3];
                if saved { p.add(egui::Shape::dashed_line(&pts, egui::Stroke::new(1.5_f32, col.gamma_multiply(0.5)), 4.0, 3.0)); } else { p.add(egui::Shape::line(pts, egui::Stroke::new(2.0_f32, col))); }
            }
        }
        if c.series.iter().any(|s| !s.is_empty()) {
            ui.horizontal(|ui| { for (i, s) in c.series.iter().enumerate() { ui.label(egui::RichText::new(format!("\u{25A0} {s}")).size(0.7 * r).color(colors[i % 3])); } });
        }
    }

    /// A row of exclusive choices stored at `path` ("a.b") in the badge settings; true when it changed.
    fn badge_choice(&mut self, ui: &mut egui::Ui, r: f32, accent: egui::Color32, label: &str, path: &str, choices: &[(&str, &str)]) -> bool {
        let cur = path.split('.').fold(Some(&self.badges), |v, k| v.and_then(|v| v.get(k))).and_then(|v| v.as_str()).unwrap_or("").to_string();
        let mut pick = None;
        ui.horizontal_wrapped(|ui| {
            ui.allocate_ui_with_layout(egui::vec2(8.0 * r, 1.4 * r), egui::Layout::left_to_right(egui::Align::Center), |ui| { ui.set_min_width(8.0 * r); ui.label(egui::RichText::new(label).size(0.88 * r).color(ui_kit::TEXT())); });
            for (v, name) in choices { if ui_kit::pill(ui, name, cur == *v, accent, 0.75 * r).clicked() && cur != *v { pick = Some(*v); } }
        });
        let Some(v) = pick else { return false };
        let (head, last) = path.rsplit_once('.').unwrap_or(("", path));
        let mut node = &mut self.badges;
        for k in head.split('.').filter(|k| !k.is_empty()) { if !node.get(k).map_or(false, |x| x.is_object()) { node[k] = json!({}); } node = node.get_mut(k).unwrap(); }
        node[last] = if v.is_empty() && path.starts_with("slots.") { Value::Null } else { json!(v) };
        true
    }
    /// The color bands of one text badge (`thresholds`: `{min, color}` rows): edit a minimum or a color, remove a band, add one. Returns true when something changed.
    fn bands_ui(&mut self, ui: &mut egui::Ui, r: f32, accent: egui::Color32, key: &str) -> bool {
        let mut bands: Vec<(f64, [u8; 3])> = self.badges.get(key).and_then(|b| b.get("thresholds")).and_then(|t| t.as_array()).map(|a| a.iter().filter_map(|b| {
            let min = b.get("min")?.as_f64()?; let c = b.get("color")?.as_str()?.trim_start_matches('#');
            if c.len() != 6 { return None; }
            Some((min, [u8::from_str_radix(&c[0..2], 16).ok()?, u8::from_str_radix(&c[2..4], 16).ok()?, u8::from_str_radix(&c[4..6], 16).ok()?]))
        }).collect()).unwrap_or_default();
        let (mut changed, mut drop) = (false, None);
        for (i, (min, col)) in bands.iter_mut().enumerate() {
            ui.horizontal(|ui| {
                ui.label(ui_kit::muted("from", 0.8 * r));
                // a slider (Left / Right on a pad) and preset swatches (a pad cannot drive a color picker); the picker stays for the mouse
                ui.spacing_mut().slider_width = 7.0 * r;
                let resp = ui_kit::add_slider(ui, egui::Slider::new(min, 0.0..=100.0).step_by(1.0).custom_formatter(|x, _| format!("{}%", x as i64)));
                ui_kit::slider_value(ui, format!("{}%", *min as i64), 0.88 * r);
                if resp.drag_stopped() || (resp.changed() && !resp.dragged()) { changed = true; }
                for p in BAND_COLORS {
                    let (rect, resp) = ui.allocate_exact_size(egui::vec2(1.3 * r, 1.3 * r), egui::Sense::click());
                    ui.painter().rect_filled(rect, 3, egui::Color32::from_rgb(p[0], p[1], p[2]));
                    if *col == p { ui.painter().rect_stroke(rect.expand(1.0), 3, egui::Stroke::new(2.0_f32, egui::Color32::WHITE), egui::StrokeKind::Outside); }
                    if resp.has_focus() { ui.painter().rect_stroke(rect.expand(3.0), 4, egui::Stroke::new(2.5_f32, accent), egui::StrokeKind::Outside); }   // the pad's cursor, drawn here so it shows even when the page ring is hidden
                    if resp.clicked() { *col = p; changed = true; }
                }
                if ui.color_edit_button_srgb(col).changed() { changed = true; }
                if ui.add(egui::Label::new(egui::RichText::new("\u{2715}").size(1.0 * r).color(egui::Color32::from_rgb(0xe0, 0x50, 0x50))).sense(egui::Sense::click())).clicked() { drop = Some(i); }
            });
        }
        if let Some(i) = drop { bands.remove(i); changed = true; }
        if ui_kit::button(ui, "+ BAND", Kind::Plain, accent, 0.8 * r).clicked() { bands.push((bands.last().map_or(0.0, |b| (b.0 + 10.0).min(100.0)), [0x88, 0x88, 0x88])); changed = true; }
        if changed {
            bands.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
            let rows: Vec<Value> = bands.iter().map(|(m, c)| json!({"min": (m * 10.0).round() / 10.0, "color": format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2])})).collect();
            if !self.badges.get(key).map_or(false, |b| b.is_object()) { self.badges[key] = json!({}); }
            self.badges[key]["thresholds"] = Value::Array(rows);
            self.badges_dirty = true;
        }
        let flush = self.badges_dirty && !ui.input(|i| i.pointer.any_down());
        if flush { self.badges_dirty = false; }
        flush
    }

    /// Card Badges (the corner badges on game cards): which pages show them, what each corner shows, size, and the platform / review / HLTB options.
    /// Color bands and uploaded icons are not editable here yet but are kept as they are. Returns true when something changed (saved at once).
    fn badges_ui(&mut self, ui: &mut egui::Ui, r: f32, accent: egui::Color32) -> bool {
        let mut changed = false;
        egui::ScrollArea::vertical().id_salt("badges_tab").max_height(f32::INFINITY).auto_shrink([false, true]).show(ui, |ui| {
            ui.label(ui_kit::muted("Small badges in the corners of game covers: installed, library, achievement progress, review score or time to beat. Changes apply at once.", 0.8 * r));
            ui.add_space(0.3 * r);
            ui.horizontal(|ui| {
                for (k, name) in [("library", "Library page"), ("home", "Home page"), ("pick6", "Pick 6 page")] {
                    let mut on = self.badges.get("enabled").and_then(|e| e.get(k)).and_then(|v| v.as_bool()).unwrap_or(true);
                    if ui.checkbox(&mut on, egui::RichText::new(name).size(0.88 * r)).changed() { if !self.badges.get("enabled").map_or(false, |e| e.is_object()) { self.badges["enabled"] = json!({}); } self.badges["enabled"][k] = json!(on); changed = true; }
                }
            });
            ui.add_space(0.3 * r);
            let eb_corner = self.edit_corner();
            for (k, name) in BADGE_SLOTS {
                if k == eb_corner {
                    ui.horizontal(|ui| {
                        ui.allocate_ui_with_layout(egui::vec2(8.0 * r, 1.4 * r), egui::Layout::left_to_right(egui::Align::Center), |ui| { ui.set_min_width(8.0 * r); ui.label(egui::RichText::new(name).size(0.88 * r).color(ui_kit::TEXT())); });
                        ui.label(ui_kit::muted("Reserved for the edit button (move the button below to use this corner)", 0.78 * r));
                    });
                } else { changed |= self.badge_choice(ui, r, accent, name, &format!("slots.{k}"), &BADGE_FIELDS); }
            }
            ui.add_space(0.3 * r);
            changed |= self.edit_button_ui(ui, r, accent);
            ui.add_space(0.3 * r);
            let mut scale = self.badges.get("scale").and_then(|v| v.as_f64()).unwrap_or(1.0).clamp(0.6, 1.8);
            ui.horizontal(|ui| {
                ui.allocate_ui_with_layout(egui::vec2(8.0 * r, 1.4 * r), egui::Layout::left_to_right(egui::Align::Center), |ui| { ui.set_min_width(8.0 * r); ui.label(egui::RichText::new("Size").size(0.88 * r).color(ui_kit::TEXT())); });
                ui.spacing_mut().slider_width = 16.0 * r;
                let resp = ui_kit::add_slider(ui, egui::Slider::new(&mut scale, 0.6..=1.8).step_by(0.05).custom_formatter(|x, _| format!("{x:.2}x")));
                ui_kit::slider_value(ui, format!("{scale:.2}x"), 0.88 * r);
                // saved when the drag ends (or for a key / pad step), not on every pixel of the drag
                if resp.drag_stopped() || (resp.changed() && !resp.dragged()) { self.badges["scale"] = json!((scale * 100.0).round() / 100.0); changed = true; }
            });
            ui.add_space(0.3 * r);
            changed |= self.badge_choice(ui, r, accent, "No icon for a store", "platform_fallback", &[("default", "BUNDLED BADGE"), ("label", "TEXT"), ("none", "NOTHING")]);
            changed |= self.badge_choice(ui, r, accent, "Review score", "review_score.mode", &[("weighted", "WEIGHTED"), ("raw", "RAW")]);
            changed |= self.badge_choice(ui, r, accent, "HLTB time", "hltb.mode", &[("main", "MAIN"), ("extras", "EXTRAS"), ("completionist", "COMPLETIONIST"), ("shortest", "SHORTEST"), ("longest", "LONGEST")]);
            ui.add_space(0.4 * r);
            for (key, title) in [("achievement_percent", "Achievement % colors"), ("review_score", "Review score colors")] {
                ui.label(egui::RichText::new(title).size(0.9 * r).strong().color(crate::ui_kit::heading()));
                ui.label(ui_kit::muted("A game gets the color of the highest band whose minimum it reaches; a game with no data shows no badge.", 0.75 * r));
                changed |= self.bands_ui(ui, r, accent, key);
                ui.add_space(0.3 * r);
            }
            changed |= self.icons_ui(ui, r, accent);
        });
        changed
    }

    fn edit_corner(&self) -> String { self.edit_button.get("corner").and_then(|c| c.as_str()).filter(|c| BADGE_SLOTS.iter().any(|(k, _)| k == c)).unwrap_or("top_right").to_string() }

    /// The hover edit button: the pages that show it and its corner. Taking a corner clears the badge that was in it (PlayDate's `/api/edit-button`).
    fn edit_button_ui(&mut self, ui: &mut egui::Ui, r: f32, accent: egui::Color32) -> bool {
        let mut changed = false;
        ui.label(egui::RichText::new("Edit button").size(0.9 * r).strong().color(crate::ui_kit::heading()));
        ui.horizontal(|ui| {
            ui.allocate_ui_with_layout(egui::vec2(8.0 * r, 1.4 * r), egui::Layout::left_to_right(egui::Align::Center), |ui| { ui.set_min_width(8.0 * r); ui.label(egui::RichText::new("Shown on").size(0.88 * r).color(ui_kit::TEXT())); });
            for (k, name, def) in [("home", "HOME", false), ("library", "LIBRARY", true), ("pick6", "PICK 6", false)] {
                let on = self.edit_button.get("pages").and_then(|p| p.get(k)).and_then(|v| v.as_bool()).unwrap_or(def);
                if ui_kit::pill(ui, name, on, accent, 0.75 * r).clicked() {
                    if !self.edit_button.get("pages").map_or(false, |p| p.is_object()) { self.edit_button["pages"] = json!({}); }
                    self.edit_button["pages"][k] = json!(!on); changed = true;
                }
            }
        });
        let cur = self.edit_corner();
        // the four corners laid out as the corners they are: top left and right above bottom left and right, in equal-width columns
        let pill_w = ui.painter().layout_no_wrap("BOTTOM RIGHT".to_string(), crate::theme::bold(0.75 * r), egui::Color32::PLACEHOLDER).size().x + 2.0 * 0.75 * r * 1.15;
        for row in 0..2 {
            ui.horizontal(|ui| {
                if row == 0 { ui.allocate_ui_with_layout(egui::vec2(8.0 * r, 1.4 * r), egui::Layout::left_to_right(egui::Align::Center), |ui| { ui.set_min_width(8.0 * r); ui.label(egui::RichText::new("Corner").size(0.88 * r).color(ui_kit::TEXT())); }); }
                else { ui.add_space(8.0 * r + ui.spacing().item_spacing.x); }
                for col in 0..2 {
                    let (k, name) = BADGE_SLOTS[row * 2 + col];
                    if ui_kit::pill_w(ui, &name.to_uppercase(), cur == k, accent, 0.75 * r, pill_w).clicked() && cur != k {
                        self.edit_button["corner"] = json!(k);
                        if self.badges.get("slots").and_then(|sl| sl.get(k)).map_or(false, |v| !v.is_null()) { self.badges["slots"][k] = Value::Null; }
                        changed = true;
                    }
                }
            });
        }
        ui.label(ui_kit::muted("Whichever corner the edit button is in is not available for badges.", 0.75 * r));
        ui.add_space(0.3 * r);
        changed
    }

    /// Card Outlines: which pages show them and the rule list (PlayDate's Card Outlines + Edit Rules). The first matching rule in the list wins; saved at once.
    fn outlines_ui(&mut self, ui: &mut egui::Ui, r: f32, accent: egui::Color32) -> bool {
        let mut changed = false;
        egui::ScrollArea::vertical().id_salt("outlines_tab").max_height(f32::INFINITY).auto_shrink([false, true]).show(ui, |ui| {
            ui.label(egui::RichText::new("Card outlines").size(1.0 * r).strong().color(crate::ui_kit::heading()));
            ui.label(ui_kit::muted("A card gets the color of the first rule in the list that matches it, as a ring around the cover.", 0.8 * r));
            ui.horizontal(|ui| {
                ui.allocate_ui_with_layout(egui::vec2(8.0 * r, 1.4 * r), egui::Layout::left_to_right(egui::Align::Center), |ui| { ui.set_min_width(8.0 * r); ui.label(egui::RichText::new("Shown on").size(0.88 * r).color(ui_kit::TEXT())); });
                for (k, name, def) in [("home", "HOME", false), ("library", "LIBRARY", true), ("pick6", "PICK 6", true)] {
                    let on = self.outlines.get("enabled").and_then(|e| e.get(k)).and_then(|v| v.as_bool()).unwrap_or(def);
                    if ui_kit::pill(ui, name, on, accent, 0.75 * r).clicked() {
                        if !self.outlines.get("enabled").map_or(false, |e| e.is_object()) { self.outlines["enabled"] = json!({}); }
                        self.outlines["enabled"][k] = json!(!on); changed = true;
                    }
                }
            });
            ui.add_space(0.4 * r);
            if !self.outlines.get("rules").map_or(false, |v| v.is_array()) { self.outlines["rules"] = json!([]); }
            let n = self.outlines["rules"].as_array().map_or(0, |a| a.len());
            if n == 0 { ui.label(ui_kit::muted("No rules. Add one below.", 0.85 * r)); }
            let (mut mv, mut del): (Option<(usize, usize)>, Option<usize>) = (None, None);
            for i in 0..n {
                let rule = self.outlines["rules"][i].clone();
                let label = rule.get("label").and_then(|v| v.as_str()).unwrap_or("(unlabeled)").to_string();
                let color = rule.get("color").and_then(|v| v.as_str()).unwrap_or("#888888").to_string();
                let rgb = parse_hex(&color).unwrap_or([0x88, 0x88, 0x88]);
                ui.horizontal(|ui| {
                    ui.allocate_ui_with_layout(egui::vec2(1.6 * r, 1.4 * r), egui::Layout::left_to_right(egui::Align::Center), |ui| { ui.set_min_width(1.6 * r); ui.label(ui_kit::muted(format!("{}", i + 1), 0.85 * r)); });
                    let (rect, _) = ui.allocate_exact_size(egui::vec2(1.3 * r, 1.3 * r), egui::Sense::hover());
                    ui.painter().rect_filled(rect, 3, egui::Color32::from_rgb(rgb[0], rgb[1], rgb[2]));
                    ui.allocate_ui_with_layout(egui::vec2(11.0 * r, 1.4 * r), egui::Layout::left_to_right(egui::Align::Center), |ui| { ui.set_min_width(11.0 * r); ui.label(egui::RichText::new(&label).size(0.9 * r).color(ui_kit::TEXT())); });
                    ui.allocate_ui_with_layout(egui::vec2(14.0 * r, 1.4 * r), egui::Layout::left_to_right(egui::Align::Center), |ui| { ui.set_min_width(14.0 * r); ui.label(ui_kit::muted(self.outline_filter_label(rule.get("filter")), 0.8 * r)); });
                    if ui.add_enabled_ui(i > 0, |ui| ui_kit::button(ui, "UP", Kind::Plain, accent, 0.8 * r)).inner.clicked() { mv = Some((i, i - 1)); }
                    if ui.add_enabled_ui(i + 1 < n, |ui| ui_kit::button(ui, "DOWN", Kind::Plain, accent, 0.8 * r)).inner.clicked() { mv = Some((i, i + 1)); }
                    if ui_kit::button(ui, "DELETE", Kind::Danger, accent, 0.8 * r).clicked() { del = Some(i); }
                });
                ui.horizontal(|ui| {
                    ui.add_space(2.9 * r);
                    for p in BAND_COLORS {
                        let (rect, resp) = ui.allocate_exact_size(egui::vec2(1.1 * r, 1.1 * r), egui::Sense::click());
                        ui.painter().rect_filled(rect, 3, egui::Color32::from_rgb(p[0], p[1], p[2]));
                        if rgb == p { ui.painter().rect_stroke(rect.expand(1.0), 3, egui::Stroke::new(2.0_f32, egui::Color32::WHITE), egui::StrokeKind::Outside); }
                        if resp.has_focus() { ui.painter().rect_stroke(rect.expand(3.0), 4, egui::Stroke::new(2.5_f32, accent), egui::StrokeKind::Outside); }
                        if resp.clicked() && rgb != p { self.outlines["rules"][i]["color"] = json!(to_hex(p)); changed = true; }
                    }
                    let mut c = rgb;
                    if ui.color_edit_button_srgb(&mut c).changed() { self.outlines["rules"][i]["color"] = json!(to_hex(c)); changed = true; }
                });
                ui.add_space(0.2 * r);
            }
            if let Some((a, b)) = mv { if let Some(v) = self.outlines["rules"].as_array_mut() { v.swap(a, b); for (k, rule) in v.iter_mut().enumerate() { rule["priority"] = json!(k); } } changed = true; }
            if let Some(i) = del { if let Some(v) = self.outlines["rules"].as_array_mut() { v.remove(i); for (k, rule) in v.iter_mut().enumerate() { rule["priority"] = json!(k); } } changed = true; }
            ui.add_space(0.4 * r);
            match &mut self.co_form {
                None => { if ui_kit::button(ui, "+ ADD RULE", Kind::Plain, accent, r).clicked() { self.co_form = Some(CoForm { label: String::new(), color: [0x66, 0xc0, 0xf4], saved: false, preset: 0, saved_pick: 0 }); } }
                Some(f) => {
                    ui.label(egui::RichText::new("New rule").size(0.9 * r).strong().color(crate::ui_kit::heading()));
                    ui.horizontal(|ui| {
                        ui.label(ui_kit::muted("Label", 0.85 * r));
                        ui.add(egui::TextEdit::singleline(&mut f.label).hint_text("e.g. Wishlist").desired_width(14.0 * r));
                    });
                    ui.horizontal(|ui| {
                        ui.label(ui_kit::muted("Color", 0.85 * r));
                        for p in BAND_COLORS {
                            let (rect, resp) = ui.allocate_exact_size(egui::vec2(1.1 * r, 1.1 * r), egui::Sense::click());
                            ui.painter().rect_filled(rect, 3, egui::Color32::from_rgb(p[0], p[1], p[2]));
                            if f.color == p { ui.painter().rect_stroke(rect.expand(1.0), 3, egui::Stroke::new(2.0_f32, egui::Color32::WHITE), egui::StrokeKind::Outside); }
                            if resp.has_focus() { ui.painter().rect_stroke(rect.expand(3.0), 4, egui::Stroke::new(2.5_f32, accent), egui::StrokeKind::Outside); }
                            if resp.clicked() { f.color = p; }
                        }
                        ui.color_edit_button_srgb(&mut f.color);
                    });
                    ui.horizontal(|ui| {
                        ui.label(ui_kit::muted("Matches", 0.85 * r));
                        if ui_kit::pill(ui, "BUILT-IN PRESET", !f.saved, accent, 0.75 * r).clicked() { f.saved = false; }
                        if ui_kit::pill(ui, "SAVED FILTER", f.saved, accent, 0.75 * r).clicked() { f.saved = true; }
                    });
                    ui.horizontal_wrapped(|ui| {
                        if !f.saved { for (i, (_, name)) in OUTLINE_PRESETS.iter().enumerate() { if ui_kit::pill(ui, &name.to_uppercase(), f.preset == i, accent, 0.75 * r).clicked() { f.preset = i; } } }
                        else if self.saved_filters.is_empty() { ui.label(ui_kit::muted("No saved filters yet.", 0.8 * r)); }
                        else { for (i, (_, name)) in self.saved_filters.iter().enumerate() { if ui_kit::pill(ui, &name.to_uppercase(), f.saved_pick == i, accent, 0.75 * r).clicked() { f.saved_pick = i; } } }
                    });
                    ui.label(ui_kit::muted("Rules built from a custom filter in PlayDate keep working and show as \"Custom filter\"; they cannot be created here.", 0.75 * r));
                    let (mut add, mut cancel) = (false, false);
                    ui.horizontal(|ui| {
                        if ui_kit::button(ui, "ADD RULE", Kind::Primary, accent, r).clicked() { add = true; }
                        if ui_kit::button(ui, "CANCEL", Kind::Plain, accent, r).clicked() { cancel = true; }
                    });
                    let form = f.clone();
                    if add {
                        let filter = if form.saved { self.saved_filters.get(form.saved_pick).map(|(id, _)| json!({ "type": "saved", "filter_id": id })) } else { OUTLINE_PRESETS.get(form.preset).map(|(k, _)| json!({ "type": "preset", "preset_key": k })) };
                        if form.label.trim().is_empty() { self.status = Some(("Please enter a label for this rule.".into(), false)); }
                        else if let Some(filter) = filter {
                            let n = self.outlines["rules"].as_array().map_or(0, |a| a.len());
                            let id = new_rule_id();
                            if let Some(v) = self.outlines["rules"].as_array_mut() { v.push(json!({ "id": id, "label": form.label.trim(), "color": to_hex(form.color), "priority": n, "filter": filter })); }
                            changed = true; self.co_form = None; self.status = None;
                        } else { self.status = Some(("Please select a filter.".into(), false)); }
                    }
                    if cancel { self.co_form = None; self.status = None; }
                }
            }
            if self.co_form.is_some() { if let Some((m, _)) = &self.status { ui.label(egui::RichText::new(m).size(0.85 * r).color(egui::Color32::from_rgb(0xe0, 0x60, 0x60))); } }
        });
        changed
    }

    fn outline_filter_label(&self, filter: Option<&Value>) -> String {
        let Some(f) = filter else { return "(none)".into() };
        match f.get("type").and_then(|t| t.as_str()) {
            Some("preset") => { let k = f.get("preset_key").and_then(|k| k.as_str()).unwrap_or(""); OUTLINE_PRESETS.iter().find(|(p, _)| *p == k).map_or(k.to_string(), |(_, l)| l.to_string()) }
            Some("saved") => { let id = f.get("filter_id").and_then(|k| k.as_str()).unwrap_or(""); self.saved_filters.iter().find(|(i, _)| i == id).map_or("(deleted filter)".to_string(), |(_, n)| n.clone()) }
            Some("custom") => "Custom filter".into(),
            _ => "(unknown)".into(),
        }
    }

    /// Gamepad: input on or off, dead zone, and the repeat timing of a held direction (PlayDate's Settings > Gamepad). Saved at once.
    /// `part`: 0 = the Gamepad list (input toggle, Controls and Setup rows, stick feel), 1 = Gamepad Controls (the button mapping), 2 = Gamepad Setup (button names, controller test).
    fn gamepad_ui(&mut self, ui: &mut egui::Ui, r: f32, accent: egui::Color32, part: u8) -> Option<Vec<(String, Value)>> {
        let mut changed = false;
        let mut remap = false;
        let mut remap_sdl: Option<Value> = None;
        if part == 0 {
            let small = |ui: &mut egui::Ui, t: &str| { ui.add_space(0.4 * r); ui.label(egui::RichText::new(t).size(0.72 * r).color(ui_kit::MUTED())); ui.add_space(0.2 * r); };
            if ui_kit::settings_row(ui, &format!("Gamepad input: {}", if self.pad_on { "ON" } else { "OFF" }), r, false).clicked() { self.pad_on = !self.pad_on; changed = true; }
            ui.add_space(0.3 * r);
            ui.label(ui_kit::muted("Turn off to stop the controller driving Zest (a controller used in a game never does: Zest only listens while its window has the focus).", 0.76 * r));
            ui.add_space(0.4 * r);
            if ui_kit::settings_row(ui, "Gamepad Controls", r, false).clicked() { self.pad_sub = Some(1); }
            ui.add_space(0.3 * r);
            if ui_kit::settings_row(ui, "Gamepad Setup", r, false).clicked() { self.pad_sub = Some(2); }
            small(ui, "STICK FEEL");
            let rows: [(&str, &str, f64, f64, f64, &str); 3] = [
                ("Dead zone", "How far a stick must move before it registers. Raise this if a pad drifts or scrolls on its own at rest.", 0.0, 90.0, 1.0, "%"),
                ("Repeat: initial delay", "How long a button or stick must be held before it starts auto-repeating.", 50.0, 1000.0, 10.0, " ms"),
                ("Repeat: rate", "How fast it repeats once held past the initial delay. Lower is faster.", 20.0, 500.0, 5.0, " ms"),
            ];
            for (i, (label, hint, lo, hi, step, unit)) in rows.into_iter().enumerate() {
                ui.label(egui::RichText::new(label).size(0.85 * r).color(ui_kit::TEXT()));
                ui.label(ui_kit::muted(hint, 0.75 * r));
                ui.horizontal(|ui| {
                    ui.spacing_mut().slider_width = 20.0 * r;
                    let resp = ui_kit::add_slider(ui, egui::Slider::new(&mut self.pad_nums[i], lo..=hi).step_by(step).show_value(false));
                    ui_kit::slider_value(ui, format!("{}{unit}", self.pad_nums[i].round() as i64), 0.88 * r);
                    if resp.drag_stopped() || (resp.changed() && !resp.dragged()) { changed = true; }
                });
                ui.add_space(0.3 * r);
            }
            if ui_kit::button(ui, "RESET TO DEFAULTS", Kind::Plain, accent, r).clicked() { self.pad_on = true; self.pad_nums = [35.0, 400.0, 150.0]; changed = true; }
            ui.label(ui_kit::muted("Changes apply at once.", 0.75 * r));
        }
        if part == 2 {
        // test hook: the controller picture with a made-up state (A, LB and the d-pad's right arm down, left stick pushed up-right, right trigger half in), to look at it without a pad
        if std::env::var_os("ZEST_PAD_DRAWING_DEMO").is_some() {
            let mut demo = crate::gamepad::Live::default();
            demo.pressed = (1 << 0) | (1 << 4) | (1 << 15) | (1 << 11); demo.axes = [0.7, 0.6, 0.0, 0.0, 0.0, 0.5];
            let demo_style = std::env::var("ZEST_PAD_DRAWING_DEMO").ok().and_then(|v| crate::gamepad::LabelStyle::from_key(&v)).unwrap_or(crate::gamepad::LabelStyle::Xbox);
            crate::pad_drawing::draw(ui, r, &demo, demo_style, &Default::default(), accent);
        }
        }
        // ---- button names: the label style and the player's own names for the connected controller (PlayDate's Gamepad Setup)
        let mut labels_changed = false;
        if part == 2 {
            let live = crate::gamepad::live();
            ui.label(egui::RichText::new("Button names").size(1.0 * r).strong().color(crate::ui_kit::heading()));
            match live.pads.first() {
                None => { ui.label(ui_kit::muted("Connect a controller to choose how its buttons are named.", 0.78 * r)); }
                Some(p) => {
                    let key = crate::gamepad::pad_key(p.vendor, p.product, &p.name);
                    let guess = crate::gamepad::guess_style(p.vendor, &p.name);
                    let cur = self.pad_labels.get(&key).cloned().unwrap_or_default();
                    ui.label(ui_kit::muted(format!("{}: names follow {} unless you choose another style. Your own names replace the style's for single buttons.", p.name, guess.title()), 0.78 * r));
                    ui.horizontal_wrapped(|ui| {
                        ui.label(egui::RichText::new("Label style").size(0.88 * r).color(ui_kit::TEXT()));
                        if ui_kit::button(ui, "AUTO", if cur.style.is_none() { Kind::Primary } else { Kind::Plain }, accent, r).clicked() { self.pad_labels.entry(key.clone()).or_default().style = None; labels_changed = true; }
                        for st in crate::gamepad::LabelStyle::ALL {
                            if ui_kit::button(ui, &st.title().to_uppercase(), if cur.style == Some(st) { Kind::Primary } else { Kind::Plain }, accent, r).clicked() { self.pad_labels.entry(key.clone()).or_default().style = Some(st); labels_changed = true; }
                        }
                    });
                    if ui_kit::button(ui, if self.names_open { "HIDE BUTTON NAMES" } else { "RENAME BUTTONS..." }, Kind::Plain, accent, r).clicked() { self.names_open = !self.names_open; }
                    if self.names_open {
                        let style = cur.style.unwrap_or(guess);
                        let defaults = crate::gamepad::style_names(style);
                        ui.label(ui_kit::muted("Type a name (12 characters at most) to replace a button's own; leave it empty for the style's. Pressing a button on the controller marks its row.", 0.75 * r));
                        let entry = self.pad_labels.entry(key.clone()).or_default();
                        for i in 0..17u8 {
                            ui.horizontal(|ui| {
                                let lit = live.pressed & (1 << i) != 0;
                                ui.allocate_ui_with_layout(egui::vec2(9.0 * r, 1.4 * r), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                                    ui.set_min_width(9.0 * r);
                                    ui.label(egui::RichText::new(format!("Button {}", i)).size(0.8 * r).color(if lit { accent } else { ui_kit::MUTED() }));
                                });
                                let mut text = entry.names.get(&i).cloned().unwrap_or_default();
                                let resp = ui.add(egui::TextEdit::singleline(&mut text).hint_text(defaults[i as usize]).desired_width(9.0 * r).char_limit(12).font(egui::FontId::proportional(0.88 * r)));
                                if resp.changed() {
                                    let t = text.trim().to_string();
                                    if t.is_empty() { entry.names.remove(&i); } else { entry.names.insert(i, t); }
                                    labels_changed = true;
                                }
                            });
                        }
                        if ui_kit::button(ui, "CLEAR ALL NAMES", Kind::Plain, accent, r).clicked() { entry.names.clear(); labels_changed = true; }
                    }
                }
            }
            ui.add_space(0.8 * r);
        }
        if labels_changed { crate::gamepad::set_labels(self.pad_labels.clone()); changed = true; }
        if part == 1 {
        ui.label(egui::RichText::new("Buttons").size(1.0 * r).strong().color(crate::ui_kit::heading()));
        ui.label(ui_kit::muted("What each button does. Choose a row, then press the button you want on the controller. Taking a button another action has swaps the two. The shortcuts act on the game under the cursor and start unset.", 0.78 * r));
        if let Some(i) = self.pad_capture {
            if let Some(b) = crate::gamepad::take_captured() {
                self.pad_capture = None;
                let name = crate::gamepad::ACTIONS[i].1;
                if self.pad_assign[i] == Some(b) { self.pad_msg = None; }
                else {
                    let owner = self.pad_assign.iter().enumerate().find(|(j, v)| *j != i && **v == Some(b)).map(|(j, _)| j);
                    let mut ok = true;
                    if let Some(j) = owner {
                        if self.pad_assign[i].is_some() { self.pad_assign[j] = self.pad_assign[i]; self.pad_msg = Some(format!("{} is now on {}; \"{}\" took {}.", crate::gamepad::button_name(b), name, crate::gamepad::ACTIONS[j].1, crate::gamepad::button_name(self.pad_assign[i].unwrap()))); }
                        else if crate::gamepad::ACTIONS[j].0 == "a" { ok = false; self.pad_msg = Some(format!("{} is Confirm, which needs a button. Move Confirm first.", crate::gamepad::button_name(b))); }
                        else { self.pad_assign[j] = None; self.pad_msg = Some(format!("{} is now on {}; \"{}\" has no button.", crate::gamepad::button_name(b), name, crate::gamepad::ACTIONS[j].1)); }
                    } else { self.pad_msg = None; }
                    if ok { self.pad_assign[i] = Some(b); remap = true; }
                }
            } else { ui.ctx().request_repaint_after(std::time::Duration::from_millis(100)); }
        }
        for (i, (act, label, _)) in crate::gamepad::ACTIONS.iter().enumerate() {
            if i == 12 { ui.add_space(0.4 * r); ui.label(ui_kit::muted("Shortcuts", 0.8 * r)); }
            ui.horizontal(|ui| {
                ui.allocate_ui_with_layout(egui::vec2(12.0 * r, 1.4 * r), egui::Layout::left_to_right(egui::Align::Center), |ui| { ui.set_min_width(12.0 * r); ui.label(egui::RichText::new(*label).size(0.88 * r).color(ui_kit::TEXT())); });
                if self.pad_capture == Some(i) {
                    ui.label(egui::RichText::new("Press a button...").size(0.85 * r).italics().color(accent));
                    if ui_kit::button(ui, "CANCEL", Kind::Plain, accent, r).clicked() { self.pad_capture = None; crate::gamepad::cancel_capture(); }
                } else {
                    let name = self.pad_assign[i].map_or("Not set".to_string(), crate::gamepad::button_name);
                    if ui_kit::button(ui, &name, Kind::Plain, accent, r).clicked() { self.pad_capture = Some(i); crate::gamepad::capture_next(); }
                    if *act != "a" && self.pad_assign[i].is_some() && ui_kit::button(ui, "CLEAR", Kind::Plain, accent, r).clicked() { self.pad_assign[i] = None; remap = true; self.pad_msg = None; }
                }
            });
        }
        if let Some(m) = &self.pad_msg { ui.label(ui_kit::muted(m.clone(), 0.8 * r)); }
        if ui_kit::button(ui, "RESET BUTTONS", Kind::Plain, accent, r).clicked() { self.pad_assign = crate::gamepad::assignments(&Default::default()); self.pad_capture = None; crate::gamepad::cancel_capture(); remap = true; self.pad_msg = None; }
        }
        ui.add_space(0.4 * r);
        if part == 2 && self.controller_test_ui(ui, r, accent, &mut remap_sdl) { changed = true; }
        let remap_update = remap.then(|| ("button_remaps".to_string(), crate::gamepad::to_storage(&self.pad_assign)));
        if remap || remap_sdl.is_some() { changed = true; }
        changed.then(|| { let mut v = vec![
            ("gamepad_enabled".to_string(), json!(self.pad_on)),
            ("gamepad_deadzone".to_string(), json!(self.pad_nums[0].round() as i64)),
            ("gamepad_repeat_initial_ms".to_string(), json!(self.pad_nums[1].round() as i64)),
            ("gamepad_repeat_rate_ms".to_string(), json!(self.pad_nums[2].round() as i64)),
            ("gamepad_swap_xy".to_string(), json!(self.pad_swap_xy)),
            ("gamepad_labels".to_string(), crate::gamepad::labels_to_storage(&self.pad_labels)),
        ]; if let Some(u) = remap_update { v.push(u); } if let Some(u) = remap_sdl.take() { v.push(("gamepad_sdl_mappings".to_string(), u)); } v })
    }

    /// Settings > Gamepad > Controller test and custom mappings. gilrs already applies SDL's controller database, so a known pad needs nothing; for one it does not know, the
    /// test shows what the pad reports and a pasted SDL mapping line (from SDL's controllermap tool, Steam's controller config, or a mapping site) teaches it. A new line applies at the next start.
    fn controller_test_ui(&mut self, ui: &mut egui::Ui, r: f32, accent: egui::Color32, save: &mut Option<Value>) -> bool {
        let mut changed = false;
        crate::gamepad::watch(true);
        ui.add_space(0.8 * r);
        ui.label(egui::RichText::new("Controller test").size(1.0 * r).strong().color(crate::ui_kit::heading()));
        ui.horizontal(|ui| {
            let on = crate::gamepad::testing();
            if ui_kit::button(ui, if on { "STOP TEST" } else { "TEST BUTTONS" }, if on { Kind::Primary } else { Kind::Plain }, accent, r).clicked() { crate::gamepad::set_test(!on); if !on { ui.ctx().data_mut(|d| d.insert_temp(egui::Id::new("pad_live_goto"), true)); } }   // starting the test brings the picture into view
            ui.label(ui_kit::muted(if on { "The controller is not driving Zest now, so B and the rest can be tried. Hold B for 1.5 seconds, or press STOP TEST, to finish." } else { "Stops the controller driving Zest so every button, B included, can be tried without closing this window." }, 0.78 * r));
        });
        if ui.checkbox(&mut self.pad_swap_xy, egui::RichText::new("X and Y are swapped (only for a controller Zest has no layout for)").size(0.88 * r)).changed() { crate::gamepad::apply_swap(self.pad_swap_xy); changed = true; }
        let live = if std::env::var_os("ZEST_FAKE_PAD").is_some() { crate::gamepad::fake_live() } else { crate::gamepad::live() };
        if live.pads.is_empty() { ui.label(ui_kit::muted("No controller found. Plug one in, or press a button on a wireless one.", 0.8 * r)); }
        for p in &live.pads {
            ui.label(egui::RichText::new(&p.name).size(0.9 * r).color(ui_kit::TEXT()));
            ui.label(ui_kit::muted(format!("Mapping: {}   GUID: {}", match p.mapping.as_str() { "SdlMappings" => "known to SDL's database", "Driver" => "from the driver (unmapped)", "User" => "your mapping", "None" => "none", o => o }, p.uuid), 0.75 * r));
            // a pad the system gave no layout to ("from the driver"): the bundled database may have one, which becomes a saved mapping line (applied at the next start)
            if p.mapping != "SdlMappings" && p.mapping != "User" {
                if let Some((v, pr)) = p.vendor.zip(p.product) {
                    if let Some((dbname, fields)) = pd_filter::gamepad_db::lookup(v, pr) {
                        ui.label(ui_kit::muted(format!("A known layout exists for this controller ({dbname}). Zest normally uses it on its own; if the line above still says it is from the driver, press USE THIS LAYOUT."), 0.75 * r));
                        let line = pd_filter::gamepad_db::mapping_line(&p.uuid, v, pr).unwrap_or_default();
                        let have = self.sdl_text.lines().any(|l| l.trim() == line);
                        if have { ui.label(ui_kit::muted("Saved and applied.", 0.78 * r)); }
                        else if ui_kit::button(ui, "USE THIS LAYOUT", Kind::Plain, accent, r).clicked() {
                            if !self.sdl_text.trim().is_empty() && !self.sdl_text.ends_with('\n') { self.sdl_text.push('\n'); }
                            self.sdl_text.push_str(&line);
                            let lines: Vec<String> = self.sdl_text.lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect();
                            crate::gamepad::set_sdl_lines(&lines); crate::gamepad::reload();
                            *save = Some(json!(lines)); changed = true;
                            self.sdl_msg = Some("Saved and applied.".into());
                        }
                        let _ = fields;
                    }
                }
            }
        }
        // the live display (picture, button boxes, axis readout) is ONE stop for the pad, so moving down the dialog lands on it with all of it in view
        let live_top = ui.cursor().min;
        { let (style, labels) = crate::gamepad::active_labels(); let _ = crate::pad_drawing::draw(ui, r, &live, style, &labels, accent); }
        ui.add_space(0.4 * r);
        let names: Vec<String> = (0..17u8).map(|i| match i { 12 => "Up".to_string(), 13 => "Down".to_string(), 14 => "Left".to_string(), 15 => "Right".to_string(), i => crate::gamepad::button_name(i) }).collect();
        ui.horizontal_wrapped(|ui| {
            for (i, n) in names.iter().enumerate() {
                let on = live.pressed & (1 << i) != 0;
                let (rect, _) = ui.allocate_exact_size(egui::vec2(2.4 * r, 1.4 * r), egui::Sense::hover());
                ui.painter().rect_filled(rect, 3.0, if on { accent } else { egui::Color32::from_rgb(0x33, 0x33, 0x33) });
                ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, n, crate::theme::bold(0.7 * r), if on { ui_kit::ON_ACCENT() } else { ui_kit::MUTED() });
            }
        });
        ui.horizontal_wrapped(|ui| {
            for (i, n) in ["Left stick X", "Left stick Y", "Right stick X", "Right stick Y", "LT", "RT"].iter().enumerate() {
                ui.label(ui_kit::muted(format!("{n} {:+.2}", live.axes[i]), 0.75 * r));
                ui.add_space(0.5 * r);
            }
        });
        { let rect = egui::Rect::from_min_max(live_top, egui::pos2(ui.max_rect().right(), ui.cursor().min.y)); let resp = ui.interact(rect, egui::Id::new("pad_live_display"), egui::Sense::focusable_noninteractive()); if ui.ctx().data_mut(|d| d.remove_temp::<bool>(egui::Id::new("pad_live_goto"))).unwrap_or(false) { resp.request_focus(); resp.scroll_to_me(Some(egui::Align::Center)); } }
        if !live.raw.is_empty() {
            ui.label(ui_kit::muted("Latest raw events (a button that shows nothing above, or the wrong name, needs a mapping):", 0.75 * r));
            for e in live.raw.iter().rev() { ui.label(egui::RichText::new(e).size(0.72 * r).monospace().color(ui_kit::MUTED())); }
        }
        ui.add_space(0.6 * r);
        ui.label(egui::RichText::new("Custom mappings").size(0.9 * r).strong().color(crate::ui_kit::heading()));
        ui.label(ui_kit::muted("Paste SDL mapping lines, one per line (GUID, name, then a:b0,b:b1,... pairs; lines starting with # are ignored). Zest uses them for a pad the system does not know, and applies them as soon as you save. Zest already uses the bundled layout for a pad it knows, on its own.", 0.75 * r));
        ui.add(egui::TextEdit::multiline(&mut self.sdl_text).desired_rows(3).desired_width(f32::INFINITY).font(egui::TextStyle::Monospace));
        ui.horizontal(|ui| {
            if ui_kit::button(ui, "SAVE MAPPINGS", Kind::Plain, accent, r).clicked() {
                let lines: Vec<String> = self.sdl_text.lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect();
                let bad = lines.iter().filter(|l| !l.starts_with('#') && !crate::gamepad::sdl_line_ok(l)).count();
                if bad > 0 { self.sdl_msg = Some(format!("{bad} line(s) are not SDL mappings (a 32-digit GUID, a name, then pairs like a:b0). Nothing was saved.")); }
                else { crate::gamepad::set_sdl_lines(&lines); crate::gamepad::reload(); *save = Some(json!(lines)); self.sdl_msg = Some("Saved and applied.".into()); }
            }
        });
        if let Some(m) = &self.sdl_msg { ui.label(ui_kit::muted(m.clone(), 0.8 * r)); }
        ui.add_space(0.4 * r);
        ui.label(ui_kit::muted("A step-by-step layout wizard, button names and the controller drawing from PlayDate's Gamepad Setup are not in Zest.", 0.78 * r));
        changed
    }

    /// Your own icons for the installed badge and for each store (PlayDate's Card Badges dialog): choose an image, or clear it to go back to the default.
    fn icons_ui(&mut self, ui: &mut egui::Ui, r: f32, accent: egui::Color32) -> bool {
        let mut changed = false;
        if let Some(rx) = &self.icon_rx {
            if let Ok((slot, res)) = rx.try_recv() {
                self.icon_rx = None;
                match res {
                    Ok(file) => { self.set_icon(&slot, Some(file)); changed = true; self.icon_msg = Some("Icon saved.".into()); }
                    Err(e) => self.icon_msg = Some(e),
                }
            } else { ui.ctx().request_repaint_after(std::time::Duration::from_millis(100)); }
        }
        ui.label(egui::RichText::new("Your own icons").size(0.9 * r).strong().color(crate::ui_kit::heading()));
        ui.label(ui_kit::muted("PNG or JPEG, up to 5 MB, shrunk to fit 128 pixels. A store without one uses the setting above.", 0.75 * r));
        let mut slots: Vec<(String, String)> = vec![("installed".into(), "Installed".into())];
        slots.extend(self.art_plats.iter().map(|(id, label, _)| (id.clone(), label.clone())));
        let busy = self.icon_rx.is_some();
        for (slot, label) in slots {
            let cur = self.get_icon(&slot);
            let preview = self.icon_preview(ui.ctx(), &slot, cur.as_deref());
            ui.horizontal(|ui| {
                ui.allocate_ui_with_layout(egui::vec2(9.0 * r, 1.4 * r), egui::Layout::left_to_right(egui::Align::Center), |ui| { ui.set_min_width(9.0 * r); ui.label(egui::RichText::new(&label).size(0.88 * r).color(ui_kit::TEXT())); });
                // the logo that is in use: the chosen file, else the bundled one (none for "Installed", which has no default image)
                let side = 1.6 * r;
                match &preview { Some(t) => { ui.add(egui::Image::new(t).fit_to_exact_size(egui::vec2(side, side))); } None => { ui.allocate_space(egui::vec2(side, side)); } }
                if ui_kit::button(ui, if cur.is_some() { "REPLACE..." } else { "CHOOSE..." }, Kind::Plain, accent, r).clicked() && !busy { self.pick_icon(ui.ctx(), &slot); }
                if cur.is_some() && ui_kit::button(ui, "CLEAR", Kind::Plain, accent, r).clicked() { self.set_icon(&slot, None); changed = true; self.icon_msg = None; }
                ui.label(ui_kit::muted(cur.as_deref().unwrap_or(if preview.is_some() { "bundled logo" } else { "default" }), 0.75 * r));
            });
        }
        if let Some(m) = &self.icon_msg { ui.label(ui_kit::muted(m.clone(), 0.8 * r)); }
        changed
    }

    /// The picture for a row of "Your own icons": the file the user chose, else the store's bundled logo (as the card badge draws it); `None` when there is neither.
    fn icon_preview(&mut self, ctx: &egui::Context, slot: &str, chosen: Option<&str>) -> Option<egui::TextureHandle> {
        let key = match chosen { Some(f) => format!("file:{f}"), None => format!("default:{slot}") };
        if let Some(t) = self.icon_prev.get(&key) { return t.clone(); }
        let img = match chosen {
            Some(f) if !f.contains(['/', '\\']) => image::open(self.dir.join("static/img/badges").join(f)).ok().map(|i| i.thumbnail(96, 96).to_rgba8()),
            Some(_) => None,
            None => if slot == "installed" { None } else { crate::badge_defaults::decode(slot).map(|i| image::imageops::thumbnail(&i, 96, 96)) },
        };
        let tex = img.map(|rgba| ctx.load_texture(format!("badge-prev-{key}"), egui::ColorImage::from_rgba_unmultiplied([rgba.width() as usize, rgba.height() as usize], rgba.as_raw()), egui::TextureOptions::LINEAR));
        self.icon_prev.insert(key, tex.clone());
        tex
    }

    fn get_icon(&self, slot: &str) -> Option<String> {
        let icons = self.badges.get("icons")?;
        let v = if slot == "installed" { icons.get("installed")? } else { icons.get("platform")?.get(slot)? };
        v.as_str().map(String::from)
    }

    /// Point a slot at a saved file (or at nothing) and delete the file it used to use.
    fn set_icon(&mut self, slot: &str, file: Option<String>) {
        let old = self.get_icon(slot);
        if !self.badges.get("icons").map_or(false, |i| i.is_object()) { self.badges["icons"] = json!({ "installed": null, "platform": {} }); }
        if slot == "installed" { self.badges["icons"]["installed"] = file.clone().map_or(Value::Null, Value::from); }
        else {
            if !self.badges["icons"].get("platform").map_or(false, |p| p.is_object()) { self.badges["icons"]["platform"] = json!({}); }
            match &file { Some(f) => { self.badges["icons"]["platform"][slot] = json!(f); } None => { if let Some(m) = self.badges["icons"]["platform"].as_object_mut() { m.remove(slot); } } }
        }
        if let Some(o) = old.filter(|o| Some(o) != file.as_ref() && !o.contains(['/', '\\'])) { let _ = std::fs::remove_file(self.dir.join("static/img/badges").join(o)); }
    }

    /// Ask for an image on a thread (the dialog blocks), then convert it like PlayDate's `save_badge_icon`.
    fn pick_icon(&mut self, ctx: &egui::Context, slot: &str) {
        let (tx, rx) = std::sync::mpsc::channel();
        self.icon_rx = Some(rx);
        let (slot, dir, ctx2) = (slot.to_string(), self.dir.join("static/img/badges"), ctx.clone());
        std::thread::spawn(move || {
            let res = match rfd::FileDialog::new().set_title("Choose an icon").add_filter("Images", &["png", "jpg", "jpeg"]).pick_file() {
                None => Err(String::new()),
                Some(path) => save_badge_icon(&path, &dir, &slot),
            };
            let _ = tx.send((slot, res)); ctx2.request_repaint();
        });
    }

    /// The sources in use for one art type of one store, in order: the saved list, else the default (PlayDate's `art_source_order`).
    fn art_order(&self, plat: &str, kind: &str, has_store: bool) -> Vec<String> {
        let saved: Option<Vec<String>> = self.art_prefs.get(plat).and_then(|p| p.get(kind)).and_then(|l| l.as_array()).map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect());
        grid_core::art::source_order(kind, has_store, saved.as_deref(), None).unwrap_or_else(|| vec!["sgdb".into(), "steam".into()])
    }
    /// Artwork Sources, as PlayDate lays it out: a row per store saying whether it uses the default order or its own, and an EDIT button that opens that store's three lists
    /// (`art_edit_dialog`). Changes are saved at once as PlayDate's `art_source_prefs`.
    fn art_ui(&mut self, ui: &mut egui::Ui, r: f32, accent: egui::Color32) -> Option<Value> {
        if self.art_plats.is_empty() { self.art_plats.push(("steam".into(), "Steam".into(), false)); }
        ui.label(ui_kit::muted("Choose where each library gets its cover art, and in what order the sources are tried: the store's own art (where the plugin provides it), SteamGridDB (needs an API key), or Steam. Applies the next time art is fetched or re-fetched.", 0.8 * r));
        ui.add_space(0.3 * r);
        for (i, (pid, label, _)) in self.art_plats.clone().iter().enumerate() {
            let custom = self.art_prefs.get(pid).is_some();
            ui.horizontal(|ui| {
                // the name takes a cell as tall as the EDIT button so the two line up
                let w = (ui.available_width() - 9.0 * r).max(4.0 * r);
                ui.allocate_ui_with_layout(egui::vec2(w, 2.8 * r), egui::Layout::left_to_right(egui::Align::Center), |ui| { ui.label(egui::RichText::new(label).size(0.9 * r).color(ui_kit::TEXT())); });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui_kit::button(ui, "EDIT", Kind::Plain, accent, r).clicked() { self.art_edit = Some(i); }
                    ui.label(ui_kit::muted(if custom { "Custom" } else { "Default" }, 0.78 * r));
                });
            });
        }
        None
    }

    /// One store's artwork lists in a dialog of their own (PlayDate's "<Store> artwork"): per art type the sources in order with a tick to switch one off and arrows to move it.
    /// Every change is returned (and saved) at once; RESET TO DEFAULT removes the store's entry; DONE, the cross or Esc closes.
    fn art_edit_dialog(&mut self, ctx: &egui::Context) -> Option<Value> {
        let i = self.art_edit?;
        let Some((plat, label, has_store)) = self.art_plats.get(i).cloned() else { self.art_edit = None; return None };
        let accent = ui_kit::accent();
        let mut lists: Vec<Vec<String>> = ART_KINDS.iter().map(|(k, _)| self.art_order(&plat, k, has_store)).collect();
        let (mut changed, mut reset, mut done) = (false, false, false);
        ui_kit::STACKED.store(true, std::sync::atomic::Ordering::Relaxed);
        let res = ui_kit::modal_footer(ctx, "art_edit", &format!("{label} artwork"), 30.0, |ui, r| {
            ui.label(ui_kit::muted("Sources are tried top to bottom until one has art. Use the arrows to reorder; untick a source to never use it.", 0.8 * r));
            for (ki, heading) in ["VERTICAL", "HORIZONTAL", "ICON"].into_iter().enumerate() {
                ui.add_space(0.3 * r);
                ui.label(egui::RichText::new(heading).size(0.72 * r).color(ui_kit::MUTED()));
                let on = lists[ki].clone();
                let off: Vec<&str> = ["store", "sgdb", "steam"].into_iter().filter(|s| (*s != "store" || has_store) && !on.iter().any(|o| o == s)).collect();
                let row = |ui: &mut egui::Ui, add: &mut dyn FnMut(&mut egui::Ui)| {
                    egui::Frame::new().fill(ui_kit::INPUT()).stroke(egui::Stroke::new(1.0_f32, ui_kit::EDGE())).corner_radius((0.3 * r) as u8).inner_margin(egui::Margin::symmetric((0.6 * r) as i8, (0.3 * r) as i8)).show(ui, |ui| { ui.set_width(ui.available_width()); ui.horizontal(|ui| add(ui)); });
                };
                for (n, src) in on.iter().enumerate() {
                    row(ui, &mut |ui| {
                        let mut tick = true;
                        if ui.checkbox(&mut tick, egui::RichText::new(art_source_label(src)).size(0.88 * r)).changed() { lists[ki].retain(|x| x != src); changed = true; }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.add_enabled_ui(n + 1 < on.len(), |ui| ui_kit::button(ui, "\u{25BC}", Kind::Plain, accent, 0.8 * r)).inner.clicked() { lists[ki].swap(n, n + 1); changed = true; }
                            if ui.add_enabled_ui(n > 0, |ui| ui_kit::button(ui, "\u{25B2}", Kind::Plain, accent, 0.8 * r)).inner.clicked() { lists[ki].swap(n, n - 1); changed = true; }
                        });
                    });
                }
                for src in off {
                    row(ui, &mut |ui| {
                        let mut tick = false;
                        if ui.checkbox(&mut tick, egui::RichText::new(format!("{} (off)", art_source_label(src))).size(0.88 * r).color(ui_kit::MUTED())).changed() { lists[ki].push(src.to_string()); changed = true; }
                    });
                }
            }
        }, |ui, r| {
            ui.horizontal(|ui| {
                if ui_kit::button(ui, "RESET TO DEFAULT", Kind::Plain, accent, r).clicked() { reset = true; }
                if ui_kit::button(ui, "DONE", Kind::Plain, accent, r).clicked() { done = true; }
            });
        });
        if res.closed || done { self.art_edit = None; }
        if !changed && !reset { return None; }
        let mut all = self.art_prefs.as_object().cloned().unwrap_or_default();
        if reset { all.remove(&plat); } else { all.insert(plat, json!({ "vertical": lists[0], "horizontal": lists[1], "icon": lists[2] })); }
        self.art_prefs = Value::Object(all);
        Some(self.art_prefs.clone())
    }

    /// Liked and disliked tags with pinning (PlayDate's Tuning panel). A button on an automatic entry sets it to ignore; on a pinned entry it removes the pin;
    /// the box adds a pin for any tag. Pins are kept until SAVE, like the sliders.
    fn pin_ui(&mut self, ui: &mut egui::Ui, r: f32, accent: egui::Color32) {
        self.refresh_preview();
        let Some((_, lists)) = self.preview.clone() else { return };
        if self.tag_vocab.is_none() { self.tag_vocab = Some(self.load_tag_vocab()); }
        let mut act: Option<(String, Option<&'static str>)> = None;
        ui.add_space(0.4 * r);
        ui.label(egui::RichText::new("Liked and disliked tags").size(0.85 * r).strong().color(ui_kit::heading()));
        ui.label(ui_kit::muted("The tags the Beaten/Completed games over- and under-represent compared with your Steam library (percentage points). Updates as you move the sliders. \u{25CF} marks a tag you pinned.", 0.78 * r));
        ui.add_space(0.3 * r);
        // a small square button with a cross, as PlayDate's: on an automatic tag it ignores it, on a pinned one it unpins
        let cross = |ui: &mut egui::Ui| -> bool {
            let (rect, resp) = ui.allocate_exact_size(egui::vec2(1.9 * r, 1.9 * r), egui::Sense::click());
            ui.painter().rect_filled(rect, 4.0, if resp.hovered() { ui_kit::HOVER() } else { ui_kit::INPUT() });
            ui.painter().rect_stroke(rect, 4.0, egui::Stroke::new(1.0_f32, ui_kit::EDGE()), egui::StrokeKind::Inside);
            ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, "\u{2715}", egui::FontId::proportional(0.8 * r), ui_kit::TEXT());
            if resp.hovered() { ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand); }
            resp.clicked()
        };
        ui.columns(2, |cols| {
            for (i, (title, list)) in [("Liked tags", &lists.liked), ("Disliked tags", &lists.disliked)].into_iter().enumerate() {
                let ui = &mut cols[i];
                ui.label(ui_kit::muted(title, 0.78 * r));
                if list.is_empty() { ui.label(ui_kit::muted("(none)", 0.8 * r)); }
                for e in list {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 0.3 * r;
                        let w = ui.available_width();
                        ui.allocate_ui_with_layout(egui::vec2((w - 7.0 * r).max(4.0 * r), 1.9 * r), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                            ui.set_min_width((w - 7.0 * r).max(4.0 * r));   // a cell keeps its width, so the numbers and the crosses line up in columns
                            if e.pinned { ui.label(egui::RichText::new("\u{25CF}").size(0.7 * r).color(accent)); }
                            ui.add(egui::Label::new(egui::RichText::new(&e.tag).size(0.88 * r).color(ui_kit::TEXT())).truncate());
                        });
                        ui.allocate_ui_with_layout(egui::vec2(3.6 * r, 1.9 * r), egui::Layout::right_to_left(egui::Align::Center), |ui| { ui.set_min_width(3.6 * r); ui.label(ui_kit::muted(format!("{:+.1}", e.affinity * 100.0), 0.78 * r)); });
                        if cross(ui) { act = Some((e.tag.clone(), if e.pinned { None } else { Some("ignore") })); }
                    });
                }
            }
        });
        if !lists.ignored.is_empty() {
            ui.add_space(0.3 * r);
            ui.horizontal_wrapped(|ui| {
                ui.label(ui_kit::muted("Ignored:", 0.78 * r));
                for t in &lists.ignored { if ui_kit::pill(ui, &format!("{} \u{2715}", t.to_uppercase()), false, accent, 0.75 * r).clicked() { act = Some((t.clone(), None)); } }
            });
        }
        ui.add_space(0.4 * r);
        // one box for a tag to like, dislike or ignore; typing shows the library's matching tags to pick from
        ui.horizontal_wrapped(|ui| {
            let resp = ui.add(egui::TextEdit::singleline(&mut self.pin_input).desired_width(13.0 * r).hint_text("Add a tag").font(egui::FontId::proportional(0.9 * r)));
            let _ = resp;
            for (name, mode) in [("LIKE", "like"), ("DISLIKE", "dislike"), ("IGNORE", "ignore")] {
                if ui_kit::button(ui, name, Kind::Plain, accent, 0.8 * r).clicked() && !self.pin_input.trim().is_empty() { act = Some((self.pin_input.trim().to_string(), Some(mode))); self.pin_input.clear(); }
            }
        });
        let q = self.pin_input.trim().to_lowercase();
        if !q.is_empty() {
            let vocab = self.tag_vocab.as_deref().unwrap_or(&[]);
            if !vocab.iter().any(|t| t.to_lowercase() == q) {
                let mut hits: Vec<&String> = vocab.iter().filter(|t| t.to_lowercase().starts_with(&q)).collect();
                hits.extend(vocab.iter().filter(|t| { let l = t.to_lowercase(); !l.starts_with(&q) && l.contains(&q) }));
                ui.horizontal_wrapped(|ui| {
                    for t in hits.into_iter().take(12) { if ui_kit::pill(ui, &t.to_uppercase(), false, accent, 0.72 * r).clicked() { self.pin_input = t.clone(); } }
                });
            }
        }
        if let Some((tag, mode)) = act {
            match mode { Some(m) => { self.pins.insert(tag, json!(m)); } None => { self.pins.remove(&tag); } }
            self.pins = pd_filter::tagaffinity::normalize_overrides(&Value::Object(std::mem::take(&mut self.pins))).as_object().cloned().unwrap_or_default();
            self.preview = None;
        }
        ui.add_space(0.3 * r);
    }

    /// The Steam library's tags, the most common first: what the box above offers while you type.
    fn load_tag_vocab(&self) -> Vec<String> {
        let Some(db) = &self.db else { return Vec::new() };
        let Ok(c) = rusqlite::Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY) else { return Vec::new() };
        let mut counts: HashMap<String, usize> = HashMap::new();
        if let Ok(mut st) = c.prepare("SELECT tags FROM games WHERE platform = 'steam' AND tags IS NOT NULL AND tags != ''") {
            if let Ok(rows) = st.query_map([], |r| r.get::<_, String>(0)) { for t in rows.flatten() { for tag in t.split(',') { let tag = tag.trim(); if !tag.is_empty() { *counts.entry(tag.to_string()).or_default() += 1; } } } }
        }
        let mut v: Vec<(String, usize)> = counts.into_iter().collect();
        v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        v.into_iter().map(|x| x.0).collect()
    }
}

/// How a slider shows its value: the blend as "65/35", the others with their unit, curves with two decimals.
fn fmt_value(key: &str, v: f64, unit: &str) -> String {
    match key {
        "pick6_smart_tag_weight" => format!("{}/{}", v as i64, 100 - v as i64),
        k if k.ends_with("_curve") => format!("{v:.2}"),
        "tag_similarity_min_library_rate" => format!("{v}%"),
        _ => format!("{}{unit}", v as i64),
    }
}

/// The JSON number PlayDate saves for a knob: whole numbers as integers, the rare-tag floor as a float, curves rounded to two decimals.
fn knob_json(k: &Knob, v: f64) -> Value {
    if k.key.ends_with("_curve") { json!((v * 100.0).round() / 100.0) }
    else if k.key == "tag_similarity_min_library_rate" { json!(v) }
    else { json!(v.round() as i64) }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn every_knob_default_is_inside_its_range_and_saved_as_python_saves_it() {
        for s in SECTIONS { for k in s.knobs { assert!(k.def >= k.min && k.def <= k.max, "{}", k.key); } }
        let all: Vec<&Knob> = SECTIONS.iter().flat_map(|s| s.knobs.iter()).collect();
        let j = |key: &str, v: f64| knob_json(all.iter().find(|k| k.key == key).unwrap(), v);
        assert_eq!(j("pick6_hltb_long_curve", 0.5000001), json!(0.5));
        assert_eq!(j("tag_similarity_min_library_rate", 2.5), json!(2.5));
        assert_eq!(j("tag_similarity_smoothing_k", 4.0), json!(4));
        assert_eq!(j("review_half_trust_count", 10.0), json!(10));
        assert_eq!(fmt_value("pick6_smart_tag_weight", 65.0, "/"), "65/35");
        assert_eq!(fmt_value("pick6_staleness_curve", 1.0, ""), "1.00");
        assert_eq!(fmt_value("pick6_staleness_cap_days", 730.0, " days"), "730 days");
    }
    #[test] fn the_tooltip_setting_reads_like_python_normalizes_it() {
        let all: Vec<&str> = TIP_FIELDS.iter().map(|(k, _)| *k).collect();
        assert_eq!(read_tip(&Value::Null), ([false, false, false], all.clone()), "nothing saved: off everywhere, every detail");
        let (pages, f) = read_tip(&json!({"library": true, "home": true, "pick6": false, "fields": ["metacritic", "description", "bogus"]}));
        assert_eq!((pages, f), ([true, true, false], vec!["description", "metacritic"]), "fields come back in PlayDate's order, unknown ones dropped");
        assert_eq!(read_tip(&json!({"enabled": true, "home": true})).0, [true, true, false], "the old shape: enabled meant the Library page");
        assert_eq!(read_tip(&json!({"enabled": false, "home": true})).0, [false, false, false], "and Home only showed with it on");
        assert_eq!(read_tip(&json!({"library": false, "home": true})).0, [false, true, false], "the new shape has its own Home flag");
    }
    #[test] fn the_dialog_starts_from_the_saved_state_and_clamps() {
        let st = json!({"pick6_staleness_cap_days": 99999, "tag_similarity_smoothing_k": 7, "auto_complete_on_100pct": false});
        let d = SettingsDialog::open(std::env::temp_dir(), &st, vec!["steam".into(), "gog".into()]);
        assert_eq!(d.vals["pick6_staleness_cap_days"], 2000.0, "out of range: clamped to the slider");
        assert_eq!((d.vals["tag_similarity_smoothing_k"], d.vals["review_half_trust_count"]), (7.0, 10.0));
        assert_eq!((d.flags["auto_complete_on_100pct"], d.flags["auto_promote_unfinished"], d.flags["art_prefer_fit"]), (false, true, true));
    }
}

#[cfg(test)]
mod chart_tests {
    use super::*;
    fn defaults(k: &str) -> f64 { SECTIONS.iter().flat_map(|s| s.knobs.iter()).find(|n| n.key == k).map_or(0.0, |n| n.def) }
    #[test] fn the_charts_follow_the_formulas() {
        let v = &defaults as &dyn Fn(&str) -> f64;
        assert!((chart_y("smoothing", 0, 4.0, v) - 0.5).abs() < 1e-9);                      // x / (x + k) with k = 4
        assert!((chart_y("playtime", 0, 60.0, v) - 1.0).abs() < 1e-9 && chart_y("playtime", 0, 0.0, v) == 0.0);
        assert!((chart_y("review", 0, 10.0, v) - (0.95 - 0.45 * 2f64.powf(-11f64.ln() / 10f64.ln()))).abs() < 1e-9);   // 95% positive, 10 reviews, trust at 10
        assert!(chart_y("review", 2, 1.0, v) > 0.3 && chart_y("review", 2, 1.0, v) < 0.5);  // a 30% score is pulled up toward 50% with one review
        assert_eq!(chart_y("staleness", 0, 730.0, v), 1.0);
        assert_eq!(chart_y("recency", 0, 10.0, v), 0.0);
        assert_eq!(chart_y("hltbLong", 0, 10.0, v), 0.0); assert_eq!(chart_y("hltbLong", 0, 110.0, v), 1.0);
        assert_eq!(chart_y("hltbShort", 0, 10.0, v), 1.0);
        // every chart is attached to a real slider
        for c in CHARTS { assert!(SECTIONS.iter().flat_map(|s| s.knobs.iter()).any(|k| chart_of(k.key).map_or(false, |x| x.name == c.name)), "{}", c.name); }
    }
}

/// Convert a picked image to an RGBA PNG no larger than 128 px on its longest side (aspect kept) in the badges folder; returns the new file name.
fn save_badge_icon(path: &std::path::Path, dir: &std::path::Path, slot: &str) -> Result<String, String> {
    if !slot.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_') || slot.is_empty() { return Err("Invalid store id.".into()); }
    let len = std::fs::metadata(path).map_err(|e| format!("Could not read the file: {e}"))?.len();
    if len > 5 * 1024 * 1024 { return Err("File too large (max 5 MB).".into()); }
    let data = std::fs::read(path).map_err(|e| format!("Could not read the file: {e}"))?;
    let im = image::load_from_memory(&data).map_err(|_| "Could not process image.".to_string())?.to_rgba8();
    let (w, h) = im.dimensions();
    let k = (128.0 / w.max(h) as f32).min(1.0);
    let im = if k < 1.0 { image::imageops::resize(&im, ((w as f32 * k).round() as u32).max(1), ((h as f32 * k).round() as u32).max(1), image::imageops::FilterType::Lanczos3) } else { im };
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis());
    let name = format!("{slot}_{ms}.png");
    let tmp = dir.join(format!("{name}.tmp"));
    im.save_with_format(&tmp, image::ImageFormat::Png).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, dir.join(&name)).map_err(|e| e.to_string())?;
    Ok(name)
}

#[cfg(test)]
mod badge_icon_tests {
    use super::*;
    #[test]
    fn a_picked_image_is_shrunk_to_fit_128_and_slots_point_at_it() {
        let tmp = std::env::temp_dir().join(format!("zest-badge-{}", std::process::id())); let _ = std::fs::remove_dir_all(&tmp); std::fs::create_dir_all(&tmp).unwrap();
        let src = tmp.join("wide.png");
        image::RgbaImage::from_pixel(400, 100, image::Rgba([10, 20, 30, 128])).save(&src).unwrap();
        let name = save_badge_icon(&src, &tmp.join("static/img/badges"), "gog").unwrap();
        assert!(name.starts_with("gog_") && name.ends_with(".png"));
        let out = image::open(tmp.join("static/img/badges").join(&name)).unwrap().to_rgba8();
        assert_eq!(out.dimensions(), (128, 32)); assert_eq!(out.get_pixel(5, 5)[3], 128, "alpha kept");
        assert!(save_badge_icon(&src, &tmp, "Bad Id").is_err());
        std::fs::write(tmp.join("junk.png"), b"nope").unwrap();
        assert_eq!(save_badge_icon(&tmp.join("junk.png"), &tmp, "gog").unwrap_err(), "Could not process image.");
        let mut d = SettingsDialog::open(tmp.clone(), &json!({}), vec![]);
        d.set_icon("installed", Some(name.clone())); d.set_icon("gog", Some(name.clone()));
        assert_eq!(d.badges["icons"]["installed"], json!(name)); assert_eq!(d.get_icon("gog"), Some(name.clone()));
        d.set_icon("gog", None);
        assert_eq!(d.get_icon("gog"), None);
        assert!(!tmp.join("static/img/badges").join(&name).exists(), "the old file is removed when a slot lets go of it");
        let _ = std::fs::remove_dir_all(&tmp);
    }
}

#[cfg(test)]
mod edit_button_tests {
    use super::*;
    #[test]
    fn the_edit_button_defaults_to_top_right_and_claiming_a_corner_clears_its_badge() {
        let mut d = SettingsDialog::open(std::env::temp_dir(), &json!({ "card_badges": { "slots": { "bottom_left": "installed", "top_right": null } } }), vec![]);
        assert_eq!(d.edit_corner(), "top_right");
        assert_eq!(d.edit_button["pages"]["library"], json!(true));
        d.edit_button["corner"] = json!("bottom_left"); d.badges["slots"]["bottom_left"] = Value::Null;
        assert_eq!(d.edit_corner(), "bottom_left");
        assert_eq!(SettingsDialog::open(std::env::temp_dir(), &json!({ "edit_button": { "corner": "nowhere" } }), vec![]).edit_corner(), "top_right", "an unknown corner falls back");
    }
}

/// The columns a Home shelf can be sorted by (the Library's sort list; \"RANDOM()\" is the shuffle shelf).
pub(crate) const SHELF_SORTS: &[(&str, &str)] = &[("name", "Alphabetical"), ("playtime_forever", "Hours Played"), ("release_date", "Release Date"), ("last_played", "Recently Played"), ("date_added", "Date Added"),
    ("review_percentage", "Review Score"), ("weighted_percentage", "Weighted Score"), ("metacritic_score", "Metacritic Score"), ("hltb_main", "HLTB: Main Story"), ("hltb_extras", "HLTB: Main + Extras"),
    ("hltb_completionist", "HLTB: Completionist"), ("hltb_min", "HLTB: Shortest"), ("hltb_max", "HLTB: Longest"), ("achievement_percent", "Achievement %"), ("achievement_remaining", "Achievements Remaining"),
    ("total_reviews", "Total Reviews"), ("tag_similarity", "Tag Similarity"), ("RANDOM()", "Shuffle")];

/// The add-rule form of the Card Outlines tab.
#[derive(Clone)]
struct CoForm { label: String, color: [u8; 3], saved: bool, preset: usize, saved_pick: usize }

/// The built-in filters a rule can use (PlayDate's `_CO_BUILTIN_PRESETS`; its two "Beaten" entries are told apart here).
pub(crate) const OUTLINE_PRESETS: [(&str, &str); 10] = [("all_games", "All Games"), ("installed", "Installed"), ("not_installed", "Not Installed"), ("never_played", "Never Played"), ("unfinished", "Unfinished"),
    ("not_beaten", "Not Beaten"), ("beaten", "Beaten or Completed"), ("completed", "Completed"), ("beaten_only", "Beaten"), ("wont_play", "Won't Play")];

/// PlayDate's `DEFAULT_CARD_OUTLINES` (the colors of the completion statuses).
fn default_outlines() -> Value {
    json!({ "enabled": { "library": true, "home": true, "pick6": true }, "rules": [
        { "id": "completed", "label": "Completed", "color": "#5BC0DE", "priority": 0, "filter": { "type": "preset", "preset_key": "completed" } },
        { "id": "beaten", "label": "Beaten", "color": "#5CB85C", "priority": 1, "filter": { "type": "preset", "preset_key": "beaten_only" } },
        { "id": "unfinished", "label": "Unfinished", "color": "#F0AD4E", "priority": 2, "filter": { "type": "preset", "preset_key": "unfinished" } },
        { "id": "wont_play", "label": "Won't Play", "color": "#D9534F", "priority": 3, "filter": { "type": "preset", "preset_key": "wont_play" } },
        { "id": "never_played", "label": "Never Played", "color": "#EEEEEE", "priority": 4, "filter": { "type": "preset", "preset_key": "never_played" } } ] })
}

fn parse_hex(s: &str) -> Option<[u8; 3]> {
    let h = s.strip_prefix('#')?; if h.len() != 6 { return None; }
    Some([u8::from_str_radix(&h[0..2], 16).ok()?, u8::from_str_radix(&h[2..4], 16).ok()?, u8::from_str_radix(&h[4..6], 16).ok()?])
}
fn to_hex(c: [u8; 3]) -> String { format!("#{:02X}{:02X}{:02X}", c[0], c[1], c[2]) }

/// A fresh rule id (PlayDate uses a UUID; anything unique does).
fn new_rule_id() -> String {
    static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
    format!("zest-{t:x}-{}", N.fetch_add(1, std::sync::atomic::Ordering::Relaxed))
}

#[cfg(test)]
mod outline_tests {
    use super::*;
    #[test]
    fn hex_colors_round_trip_and_bad_ones_are_refused() {
        assert_eq!(parse_hex("#5BC0DE"), Some([0x5b, 0xc0, 0xde])); assert_eq!(to_hex([0x5b, 0xc0, 0xde]), "#5BC0DE");
        assert_eq!(parse_hex("5BC0DE"), None); assert_eq!(parse_hex("#5BC0"), None); assert_eq!(parse_hex("#GGGGGG"), None);
    }
    #[test]
    fn the_tab_edits_rules_in_place_and_every_preset_key_is_a_real_filter() {
        for (k, _) in OUTLINE_PRESETS { assert!(pd_filter::shelf::BUILTIN_WHERE.iter().any(|(n, _)| *n == k), "{k}"); }
        let d = SettingsDialog::open(std::env::temp_dir(), &json!({ "saved_filters": { "Wishlist": { "id": "abc", "tree": {} } } }), vec![]);
        assert_eq!(d.outlines["rules"].as_array().unwrap().len(), 5);
        assert_eq!(d.outline_filter_label(Some(&json!({ "type": "saved", "filter_id": "abc" }))), "Wishlist");
        assert_eq!(d.outline_filter_label(Some(&json!({ "type": "saved", "filter_id": "gone" }))), "(deleted filter)");
        assert_eq!(d.outline_filter_label(Some(&json!({ "type": "preset", "preset_key": "wont_play" }))), "Won't Play");
        assert_ne!(new_rule_id(), new_rule_id());
    }
}

impl Drop for SettingsDialog {
    /// Closing the dialog while it waits for a button must not leave the pad thread swallowing the next press.
    fn drop(&mut self) { if self.pad_capture.is_some() { crate::gamepad::cancel_capture(); } crate::gamepad::set_test(false); crate::gamepad::watch(false); }
}
