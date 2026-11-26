//! Gamepad input. A reader thread (gilrs, which already applies SDL mappings, so no layout wizard is needed for known pads) turns
//! buttons and the left stick into `Nav` presses with PlayDate's timing: 0.35 dead zone, 400 ms before the first repeat, 150 ms between
//! repeats. The thread blocks until the pad does something (or a repeat is due), so an idle pad costs no CPU. Presses go into a shared
//! queue and wake the window loop; the frame turns them into egui key events, so egui's own focus navigation drives every dialog.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};


// Settings > Gamepad (`gamepad_enabled`, `gamepad_deadzone` %, `gamepad_repeat_initial_ms`, `gamepad_repeat_rate_ms`), read by the pad thread and the window loop.
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering::Relaxed};
static ENABLED: AtomicBool = AtomicBool::new(true);
static DEAD_PCT: AtomicU32 = AtomicU32::new(35);
static FIRST_MS: AtomicU32 = AtomicU32::new(400);
static RATE_MS: AtomicU32 = AtomicU32::new(150);
pub fn enabled() -> bool { ENABLED.load(Relaxed) }
pub fn dead_zone() -> f32 { DEAD_PCT.load(Relaxed) as f32 / 100.0 }
pub fn first_repeat() -> Duration { Duration::from_millis(FIRST_MS.load(Relaxed) as u64) }
pub fn repeat() -> Duration { Duration::from_millis(RATE_MS.load(Relaxed) as u64) }
// ───────────────────────── button remapping (Settings > Gamepad > Buttons) ─────────────────────────

/// PlayDate's action names, in the order its Gamepad Controls dialog lists them, with their labels and the standard button each starts on
/// (W3C standard numbering: 0 A, 1 B, 2 X, 3 Y, 4 LB, 5 RB, 8 Back, 9 Start, 12 to 15 the d-pad). Shortcuts have no default.
pub const ACTIONS: &[(&str, &str, Option<u8>)] = &[
    ("a", "Confirm / Select", Some(0)), ("b", "Back / Cancel", Some(1)), ("x", "Context Menu", Some(2)), ("y", "Edit Game", Some(3)),
    ("lb", "Previous Page", Some(4)), ("rb", "Next Page", Some(5)), ("back", "Open Menu", Some(8)), ("start", "Launch Game", Some(9)),
    ("up", "Navigate Up", Some(12)), ("down", "Navigate Down", Some(13)), ("left", "Navigate Left", Some(14)), ("right", "Navigate Right", Some(15)),
    ("sc_store", "Open Store Page", None), ("sc_folder", "Open Install Folder", None), ("sc_achievements", "Open Achievements", None), ("sc_community_hub", "Open Community Hub", None),
];
/// How a pad's buttons are named (PlayDate's Gamepad Setup "label style"): the same 17 standard positions, other words.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LabelStyle { Xbox, Ps, Nintendo, Other }
impl LabelStyle {
    pub const ALL: [LabelStyle; 4] = [LabelStyle::Xbox, LabelStyle::Ps, LabelStyle::Nintendo, LabelStyle::Other];
    pub fn key(self) -> &'static str { match self { LabelStyle::Xbox => "xbox", LabelStyle::Ps => "ps", LabelStyle::Nintendo => "nintendo", LabelStyle::Other => "other" } }
    pub fn title(self) -> &'static str { match self { LabelStyle::Xbox => "Xbox", LabelStyle::Ps => "PlayStation", LabelStyle::Nintendo => "Nintendo", LabelStyle::Other => "Other" } }
    pub fn from_key(k: &str) -> Option<LabelStyle> { LabelStyle::ALL.into_iter().find(|s| s.key() == k) }
}
/// The style's own names for standard buttons 0..16 (PlayDate's `BTN_LABELS*`; the d-pad keeps Zest's "D-pad" prefix in every style).
pub fn style_names(style: LabelStyle) -> [&'static str; 17] {
    match style {
        LabelStyle::Xbox => ["A", "B", "X", "Y", "LB", "RB", "LT", "RT", "Back", "Start", "L3", "R3", "D-pad Up", "D-pad Down", "D-pad Left", "D-pad Right", "Guide"],
        LabelStyle::Ps => ["\u{2715}", "\u{25CB}", "\u{25A1}", "\u{25B3}", "L1", "R1", "L2", "R2", "Share", "Options", "L3", "R3", "D-pad Up", "D-pad Down", "D-pad Left", "D-pad Right", "PS"],
        LabelStyle::Nintendo => ["B", "A", "Y", "X", "L", "R", "ZL", "ZR", "Minus", "Plus", "L3", "R3", "D-pad Up", "D-pad Down", "D-pad Left", "D-pad Right", "Home"],
        LabelStyle::Other => ["Bottom", "Right", "Left", "Top", "L1", "R1", "L2", "R2", "Select", "Start", "L3", "R3", "D-pad Up", "D-pad Down", "D-pad Left", "D-pad Right", "Home"],
    }
}
/// A guess from the USB vendor id (054c Sony, 057e Nintendo) or the name, else Xbox (PlayDate's `_labelStyleFor`).
pub fn guess_style(vendor: Option<u16>, name: &str) -> LabelStyle {
    let n = name.to_lowercase();
    if vendor == Some(0x054c) || ["dualshock", "dualsense", "playstation"].iter().any(|k| n.contains(k)) { return LabelStyle::Ps; }
    if vendor == Some(0x057e) || ["nintendo", "switch", "joy-con", "joycon"].iter().any(|k| n.contains(k)) { return LabelStyle::Nintendo; }
    LabelStyle::Xbox
}
/// What one controller is called in `state.json` `gamepad_labels`: `vvvv:pppp` when the pad reports its ids, else its lower-case name.
pub fn pad_key(vendor: Option<u16>, product: Option<u16>, name: &str) -> String {
    match vendor.zip(product) { Some((v, p)) => format!("{v:04x}:{p:04x}"), None => name.trim().to_lowercase() }
}
/// A controller's saved label choice: a style (None = guess it) and names for standard buttons that replace the style's own.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PadLabels { pub style: Option<LabelStyle>, pub names: std::collections::BTreeMap<u8, String> }
/// `gamepad_labels` -> per-controller choices. Unknown styles, buttons beyond 16 and empty names are dropped; names are cut to 12 printable characters (as PlayDate does).
pub fn parse_labels(state: &serde_json::Value) -> std::collections::HashMap<String, PadLabels> {
    let mut out = std::collections::HashMap::new();
    let Some(m) = state.get("gamepad_labels").and_then(|v| v.as_object()) else { return out };
    for (id, v) in m {
        let style = v.get("style").and_then(|s| s.as_str()).and_then(LabelStyle::from_key);
        let mut names = std::collections::BTreeMap::new();
        if let Some(n) = v.get("names").and_then(|n| n.as_object()) {
            for (k, name) in n {
                let (Ok(i), Some(t)) = (k.parse::<u8>(), name.as_str()) else { continue };
                let t: String = t.chars().filter(|c| !c.is_control()).take(12).collect::<String>().trim().to_string();
                if i <= 16 && !t.is_empty() { names.insert(i, t); }
            }
        }
        if style.is_some() || !names.is_empty() { out.insert(id.clone(), PadLabels { style, names }); }
    }
    out
}
pub fn labels_to_storage(map: &std::collections::HashMap<String, PadLabels>) -> serde_json::Value {
    let mut o = serde_json::Map::new();
    let mut ids: Vec<&String> = map.keys().collect();
    ids.sort();
    for id in ids {
        let l = &map[id];
        if l.style.is_none() && l.names.is_empty() { continue; }
        let mut e = serde_json::Map::new();
        if let Some(s) = l.style { e.insert("style".into(), serde_json::json!(s.key())); }
        if !l.names.is_empty() { e.insert("names".into(), serde_json::Value::Object(l.names.iter().map(|(k, v)| (k.to_string(), serde_json::json!(v))).collect())); }
        o.insert(id.clone(), serde_json::Value::Object(e));
    }
    serde_json::Value::Object(o)
}
/// What button `idx` is called: the player's own name, else the style's.
pub fn label_name(idx: u8, style: LabelStyle, labels: &PadLabels) -> String {
    labels.names.get(&idx).cloned().unwrap_or_else(|| style_names(style).get(idx as usize).copied().unwrap_or("?").to_string())
}
static LABELS: std::sync::LazyLock<std::sync::RwLock<std::collections::HashMap<String, PadLabels>>> = std::sync::LazyLock::new(Default::default);
/// Loaded from state.json at start and when Settings changes them.
pub fn set_labels(m: std::collections::HashMap<String, PadLabels>) { *LABELS.write().unwrap() = m; }
/// The style and choices in force for the first connected controller (an Xbox-style default when none is connected).
pub fn active_labels() -> (LabelStyle, PadLabels) {
    let live = LIVE.lock().unwrap();
    match live.pads.first() {
        Some(p) => {
            let saved = LABELS.read().unwrap().get(&pad_key(p.vendor, p.product, &p.name)).cloned().unwrap_or_default();
            (saved.style.unwrap_or_else(|| guess_style(p.vendor, &p.name)), saved)
        }
        None => (LabelStyle::Xbox, PadLabels::default()),
    }
}
/// What a standard button is called on the connected pad (its style, and any name the player gave it).
pub fn button_name(idx: u8) -> String { let (s, l) = active_labels(); label_name(idx, s, &l) }
fn nav_of(action: &str) -> Option<Nav> {
    Some(match action { "a" => Nav::Confirm, "b" => Nav::Back, "x" => Nav::Menu, "y" => Nav::Edit, "lb" => Nav::PagePrev, "rb" => Nav::PageNext, "back" => Nav::Select, "start" => Nav::Start,
        "up" => Nav::Up, "down" => Nav::Down, "left" => Nav::Left, "right" => Nav::Right,
        "sc_store" => Nav::Store, "sc_folder" => Nav::Folder, "sc_achievements" => Nav::Achievements, "sc_community_hub" => Nav::Hub, _ => return None })
}
/// `button_remaps` from state.json: `{ "<button>": "<action>" }`. A button with no entry keeps its default action; `"none"` silences it.
pub fn parse_remaps(state: &serde_json::Value) -> std::collections::HashMap<u8, String> {
    state.get("button_remaps").and_then(|v| v.as_object()).map(|m| m.iter().filter_map(|(k, v)| { let i = k.parse::<u8>().ok()?; let a = v.as_str()?; (a == "none" || ACTIONS.iter().any(|x| x.0 == a)).then(|| (i, a.to_string())) }).collect()).unwrap_or_default()
}
/// What a button does under a set of remaps.
pub fn effective(remaps: &std::collections::HashMap<u8, String>, idx: u8) -> Option<Nav> {
    match remaps.get(&idx) { Some(a) => nav_of(a), None => ACTIONS.iter().find(|x| x.2 == Some(idx)).and_then(|x| nav_of(x.0)) }
}
/// Each action's button under a set of remaps (`None` = unset), as the editor shows it.
pub fn assignments(remaps: &std::collections::HashMap<u8, String>) -> Vec<Option<u8>> {
    ACTIONS.iter().map(|(a, _, def)| {
        if let Some((k, _)) = remaps.iter().find(|(_, v)| v.as_str() == *a) { return Some(*k); }
        def.filter(|d| !remaps.contains_key(d))   // still on its default unless that button was given away or silenced
    }).collect()
}
/// The `button_remaps` that gives each action the button in `assigned` (what PlayDate's `_grmStateToStorage` writes): a changed action is stored on its button, and a
/// default button nothing took is silenced with "none" so it stops doing its old job.
pub fn to_storage(assigned: &[Option<u8>]) -> serde_json::Value {
    let mut out = serde_json::Map::new();
    let taken: Vec<u8> = assigned.iter().flatten().copied().collect();
    for ((a, _, def), now) in ACTIONS.iter().zip(assigned) {
        if now == def { continue; }
        if let Some(b) = now { out.insert(b.to_string(), serde_json::json!(a)); }
        if let Some(d) = def { if !taken.contains(d) { out.insert(d.to_string(), serde_json::json!("none")); } }
    }
    serde_json::Value::Object(out)
}
static REMAPS: std::sync::LazyLock<std::sync::RwLock<std::collections::HashMap<u8, String>>> = std::sync::LazyLock::new(Default::default);
/// Set by the Buttons editor: the next button press is not an action but the answer to "press the button for this", kept in `CAPTURED`.
static CAPTURE: AtomicBool = AtomicBool::new(false);
static CAPTURED: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(-1);

/// What Settings > Gamepad > "Controller test" shows: the pads gilrs sees, which standard buttons are down, the axes, and the last raw events (for a pad the system does not know,
/// the events say which physical button is which). Updated by the pad thread; the thread wakes the window on every event only while the test is on screen (`watch`).
#[derive(Clone, Default)]
pub struct Live { pub pads: Vec<PadInfo>, pub pressed: u32, pub axes: [f32; 6], pub raw: VecDeque<String> }
#[derive(Clone, Default)]
pub struct PadInfo { pub name: String, pub mapping: String, pub uuid: String, pub vendor: Option<u16>, pub product: Option<u16> }
static LIVE: std::sync::LazyLock<Mutex<Live>> = std::sync::LazyLock::new(Default::default);
static WATCH: AtomicBool = AtomicBool::new(false);
/// Settings > Gamepad > Controller test "TEST BUTTONS": while on, the pad drives nothing in Zest (so B can be tried without closing the dialog); holding B for 1.5 s, or the button, ends it.
static TEST: AtomicBool = AtomicBool::new(false);
pub fn set_test(on: bool) { TEST.store(on, Relaxed); }
pub fn testing() -> bool { TEST.load(Relaxed) }
/// `gamepad_swap_xy`: for a controller whose face buttons come out in the other order, X and Y change places everywhere (actions, remaps, the test).
static SWAP_XY: AtomicBool = AtomicBool::new(false);
/// The swap only means anything for a pad with no layout (a layout puts X and Y in order); false while every connected pad has one.
static SWAP_APPLIES: AtomicBool = AtomicBool::new(true);
static RELOAD: AtomicBool = AtomicBool::new(false);
/// Rebuild the controller context with the current mapping lines (the pad thread does it within 300 ms while Settings > Gamepad is open).
pub fn reload() { RELOAD.store(true, Relaxed); }
/// The mapping lines gilrs is built with; set before `reload()`.
pub fn set_sdl_lines(lines: &[String]) { *SDL_MAPPINGS.lock().unwrap() = lines.join("\n"); }
/// Takes effect at once (Settings applies it as it is ticked; the same value is saved as `gamepad_swap_xy`).
pub fn apply_swap(on: bool) { SWAP_XY.store(on, Relaxed); }
pub fn live() -> Live { LIVE.lock().unwrap().clone() }
/// Test hook (`ZEST_FAKE_PAD=1`): the Controller test as it looks with an 8BitDo connected and some events seen, without a pad, to check layout and scrolling headless.
pub fn fake_live() -> Live {
    let mut l = Live::default();
    l.pads = vec![PadInfo { name: "8BitDo Ultimate 2 Wireless Controller".into(), mapping: "SdlMappings".into(), uuid: "03000000c82d00000b3100001401000".into(), vendor: Some(0x2dc8), product: Some(0x310b) }];
    for t in ["pressed South (code 304)", "released South (code 304)", "axis LeftStickX +0.71 (code 0)", "pressed DPadDown (code 545)", "released DPadDown (code 545)", "pressed LeftTrigger2 (code 312)", "released LeftTrigger2 (code 312)", "axis RightStickY -0.52 (code 5)"] { l.raw.push_back(t.to_string()); }
    l
}
pub fn watch(on: bool) { WATCH.store(on, Relaxed); }
/// SDL mapping lines (`GUID,Name,a:b0,b:b1,...,platform:Linux,`) the user pasted: `gamepad_sdl_mappings`, an array of strings. Read once when the pad thread starts.
static SDL_MAPPINGS: std::sync::LazyLock<Mutex<String>> = std::sync::LazyLock::new(Default::default);
pub fn set_sdl_mappings(state: &serde_json::Value) { *SDL_MAPPINGS.lock().unwrap() = parse_sdl_mappings(state).join("\n"); }
/// The saved lines, trimmed, without blanks or comments.
pub fn parse_sdl_mappings(state: &serde_json::Value) -> Vec<String> {
    state.get("gamepad_sdl_mappings").and_then(|v| v.as_array()).map(|a| a.iter().filter_map(|x| x.as_str()).map(|l| l.trim().to_string()).filter(|l| !l.is_empty() && !l.starts_with('#')).collect()).unwrap_or_default()
}
/// A line SDL would accept: a 32-hex-digit GUID, a name, and at least one `field:value` pair. Anything else is left out (gilrs ignores a bad line, but the user should be told).
pub fn sdl_line_ok(line: &str) -> bool {
    let mut parts = line.trim().split(',');
    let guid = parts.next().unwrap_or("").trim();
    let name = parts.next().unwrap_or("").trim();
    guid.len() == 32 && guid.chars().all(|c| c.is_ascii_hexdigit()) && !name.is_empty() && parts.any(|f| { let f = f.trim(); f.split_once(':').map_or(false, |(k, v)| !k.is_empty() && !v.is_empty()) })
}
fn short_event(e: &gilrs::EventType) -> Option<String> {
    use gilrs::EventType::*;
    match e { ButtonPressed(b, c) => Some(format!("pressed {b:?} (code {c})")), ButtonReleased(b, c) => Some(format!("released {b:?} (code {c})")), AxisChanged(a, v, c) if v.abs() > 0.3 => Some(format!("axis {a:?} {v:+.2} (code {c})")), Connected => Some("connected".into()), Disconnected => Some("disconnected".into()), _ => None }
}
pub fn capture_next() { CAPTURED.store(-1, Relaxed); CAPTURE.store(true, Relaxed); }
pub fn cancel_capture() { CAPTURE.store(false, Relaxed); CAPTURED.store(-1, Relaxed); }
/// The standard button pressed since `capture_next`, once.
pub fn take_captured() -> Option<u8> { let v = CAPTURED.swap(-1, Relaxed); (v >= 0).then_some(v as u8) }
/// The standard (W3C) number of a gilrs button; the two triggers and sticks' clicks count too so they can be bound.
fn std_index(b: gilrs::Button) -> Option<u8> {
    use gilrs::Button::*;
    let sw = SWAP_XY.load(Relaxed) && SWAP_APPLIES.load(Relaxed);
    Some(match b { South => 0, East => 1, West => if sw { 3 } else { 2 }, North => if sw { 2 } else { 3 }, LeftTrigger => 4, RightTrigger => 5, LeftTrigger2 => 6, RightTrigger2 => 7, Select => 8, Start => 9, LeftThumb => 10, RightThumb => 11,
        DPadUp => 12, DPadDown => 13, DPadLeft => 14, DPadRight => 15, Mode => 16, _ => return None })
}

/// The saved settings as (enabled, dead zone %, first repeat ms, repeat ms), clamped to the ranges PlayDate's `save_state` allows.
pub fn parse_state(state: &serde_json::Value) -> (bool, u32, u32, u32) {
    let n = |k: &str, d: i64, lo: i64, hi: i64| state.get(k).and_then(|v| v.as_i64().or_else(|| v.as_f64().map(|f| f as i64))).unwrap_or(d).clamp(lo, hi) as u32;
    (state.get("gamepad_enabled").and_then(|v| v.as_bool()).unwrap_or(true), n("gamepad_deadzone", 35, 0, 90), n("gamepad_repeat_initial_ms", 400, 50, 2000), n("gamepad_repeat_rate_ms", 150, 20, 1000))
}
pub fn apply_state(state: &serde_json::Value) {
    *REMAPS.write().unwrap() = parse_remaps(state);
    set_labels(parse_labels(state));
    let (on, dead, first, rate) = parse_state(state);
    SWAP_XY.store(state.get("gamepad_swap_xy").and_then(|v| v.as_bool()).unwrap_or(false), Relaxed);
    ENABLED.store(on, Relaxed); DEAD_PCT.store(dead, Relaxed); FIRST_MS.store(first, Relaxed); RATE_MS.store(rate, Relaxed);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Nav { Up, Down, Left, Right, Confirm, Back, Menu, Edit, PagePrev, PageNext, Select,
    /// PlayDate's Start: launches the game under the cursor (like Confirm, but only on a card).
    Start,
    /// The unset-by-default shortcuts, acting on the game under the cursor.
    Store, Folder, Achievements, Hub }

impl Nav {
    /// The card action a shortcut button runs (`card_actions` names).
    pub fn shortcut(self) -> Option<&'static str> { match self { Nav::Store => Some("store"), Nav::Folder => Some("open_folder"), Nav::Achievements => Some("achievements"), Nav::Hub => Some("community_hub"), _ => None } }
    /// Directions repeat while held; everything else fires once per press.
    pub fn repeats(self) -> bool { matches!(self, Nav::Up | Nav::Down | Nav::Left | Nav::Right | Nav::PagePrev | Nav::PageNext) }
}

/// What the reader hands over: a press (`true`) or release (`false`) of a Nav action, repeats arriving as extra presses.
pub type Queue = Arc<Mutex<VecDeque<(Nav, bool)>>>;
/// The right stick's vertical position in thousandths (up is positive), written by the reader thread and read every frame to scroll.
pub type Scroll = Arc<std::sync::atomic::AtomicI32>;

/// Held-input tracking with repeat. Pure: the caller supplies the clock, so timing is testable.
#[derive(Default)]
pub struct Repeater { held: Vec<(Nav, Instant)> }   // action and when its next repeat is due

impl Repeater {
    pub fn press(&mut self, n: Nav, now: Instant, out: &mut Vec<(Nav, bool)>) {
        if self.held.iter().any(|(h, _)| *h == n) { return; }
        out.push((n, true));
        if n.repeats() { self.held.push((n, now + first_repeat())); }
    }
    pub fn release(&mut self, n: Nav, out: &mut Vec<(Nav, bool)>) {
        self.held.retain(|(h, _)| *h != n);
        out.push((n, false));
    }
    /// Emit every repeat that is due.
    pub fn tick(&mut self, now: Instant, out: &mut Vec<(Nav, bool)>) {
        for (n, due) in self.held.iter_mut() {
            if now >= *due { out.push((*n, true)); *due = now + repeat(); }
        }
    }
    /// How long until the next repeat, `None` when nothing is held (the thread then sleeps until the pad moves).
    pub fn next_due(&self, now: Instant) -> Option<Duration> {
        self.held.iter().map(|(_, d)| d.saturating_duration_since(now)).min()
    }
}

/// Stick position to a direction, with the dead zone. The dominant axis wins so a diagonal never presses two directions.
pub fn stick_dir(x: f32, y_up: f32) -> Option<Nav> {
    if x.abs().max(y_up.abs()) < dead_zone() { return None; }
    if x.abs() >= y_up.abs() { Some(if x > 0.0 { Nav::Right } else { Nav::Left }) } else { Some(if y_up > 0.0 { Nav::Up } else { Nav::Down }) }
}

/// gilrs with the user's mapping lines plus, for every pad it still has no layout for ("from the driver"), the one from the bundled database: used without any step by the user.
fn build_gilrs(user: &str) -> Result<gilrs::Gilrs, String> {
    let make = |m: &str| match gilrs::GilrsBuilder::new().add_mappings(m).build() { Ok(g) => Ok(g), Err(gilrs::Error::NotImplemented(g)) => Ok(g), Err(e) => Err(e.to_string()) };
    let first = make(user)?;
    let auto: Vec<String> = first.gamepads().filter(|(_, p)| p.mapping_source() != gilrs::MappingSource::SdlMappings).filter_map(|(_, p)| {
        let uuid: String = p.uuid().iter().map(|b| format!("{b:02x}")).collect();
        let line = pd_filter::gamepad_db::mapping_line(&uuid, p.vendor_id()?, p.product_id()?)?;
        eprintln!("gamepad: {} has no layout from the system; using the bundled one", p.name());
        Some(line)
    }).collect();
    if auto.is_empty() { return Ok(first); }
    drop(first);
    make(&format!("{user}\n{}", auto.join("\n")))
}

/// Start the reader. `wake` is called after every push so the window loop redraws; returns immediately. If no gamepad backend can be
/// opened (no udev, no permission) the thread ends quietly and the app simply has no pad.
pub fn spawn(queue: Queue, scroll: Scroll, wake: impl Fn() + Send + 'static) {
    std::thread::Builder::new().name("gamepad".into()).spawn(move || {
        let extra = SDL_MAPPINGS.lock().unwrap().clone();
        let mut gilrs = match build_gilrs(&extra) {
            Ok(g) => g,
            Err(e) => { eprintln!("gamepad: disabled ({e})"); return; }
        };
        for (_, pad) in gilrs.gamepads() { eprintln!("gamepad: {} (mapping: {:?}, {:?})", pad.name(), pad.mapping_source(), pad.power_info()); }
        let refresh_pads = |g: &gilrs::Gilrs| { LIVE.lock().unwrap().pads = g.gamepads().map(|(_, p)| PadInfo { name: p.name().to_string(), mapping: format!("{:?}", p.mapping_source()), uuid: p.uuid().iter().map(|b| format!("{b:02x}")).collect(), vendor: p.vendor_id(), product: p.product_id() }).collect(); };
        let update_swap = |g: &gilrs::Gilrs| { SWAP_APPLIES.store(g.gamepads().next().is_none() || g.gamepads().any(|(_, p)| p.mapping_source() != gilrs::MappingSource::SdlMappings), Relaxed); };
        refresh_pads(&gilrs); update_swap(&gilrs);
        let debug = std::env::var_os("ZEST_PAD_DEBUG").is_some();   // prints every raw event, to diagnose a pad that does nothing
        let mut rep = Repeater::default();
        let (mut sx, mut sy) = (0.0f32, 0.0f32);
        let mut stick: Option<Nav> = None;
        let mut rs = 0.0f32;   // right stick Y
        let (mut lt_down, mut rt_down) = (false, false);   // triggers that arrive as axes (-1 or 0 at rest, +1 pulled) count as the LT / RT buttons
        let mut b_since: Option<Instant> = None;
        loop {
            let now = Instant::now();
            // while the right stick is held the window must redraw every frame to keep scrolling, so the thread wakes on a ~60 Hz tick
            let tick = (rs.abs() >= dead_zone()).then_some(Duration::from_millis(16));
            let wait = match (rep.next_due(now), tick) { (Some(a), Some(b)) => Some(a.min(b)), (a, b) => a.or(b) };
            let wait = match (wait, b_since.map(|t| (t + Duration::from_millis(1500)).saturating_duration_since(now))) { (Some(a), Some(b)) => Some(a.min(b)), (a, b) => a.or(b) };
            // while Settings > Gamepad is open: poll a few times a second so a saved mapping is applied at once (otherwise the thread sleeps until the pad does something)
            let wait = if WATCH.load(Relaxed) { Some(wait.map_or(Duration::from_millis(300), |w| w.min(Duration::from_millis(300)))) } else { wait };
            if RELOAD.swap(false, Relaxed) {
                let extra = SDL_MAPPINGS.lock().unwrap().clone();
                match build_gilrs(&extra) { Ok(g) => { gilrs = g; refresh_pads(&gilrs); update_swap(&gilrs); lt_down = false; rt_down = false; } Err(e) => eprintln!("gamepad: reload failed ({e})") }
            }
            let ev = gilrs.next_event_blocking(wait);
            let mut out = Vec::new();
            let now = Instant::now();
            if let Some(gilrs::Event { event, .. }) = ev {
                if debug { eprintln!("gamepad event: {event:?}"); }
                {
                    let mut l = LIVE.lock().unwrap();
                    match &event {
                        gilrs::EventType::ButtonPressed(b, _) => if let Some(i) = std_index(*b) { l.pressed |= 1 << i; },
                        gilrs::EventType::ButtonReleased(b, _) => if let Some(i) = std_index(*b) { l.pressed &= !(1 << i); },
                        gilrs::EventType::AxisChanged(a, v, _) => { use gilrs::Axis::*; match a { LeftStickX => l.axes[0] = *v, LeftStickY => l.axes[1] = *v, RightStickX => l.axes[2] = *v, RightStickY => l.axes[3] = *v, LeftZ => l.axes[4] = *v, RightZ => l.axes[5] = *v, _ => {} } }
                        // a pad with an SDL layout reports its triggers as buttons; the analog value arrives as a change of that button
                        gilrs::EventType::ButtonChanged(b, v, _) => { use gilrs::Button::*; match b { LeftTrigger2 => l.axes[4] = *v, RightTrigger2 => l.axes[5] = *v, _ => {} } }
                        _ => {}
                    }
                    if let Some(t) = short_event(&event) { if l.raw.back() != Some(&t) { l.raw.push_back(t); while l.raw.len() > 8 { l.raw.pop_front(); } } }
                }
                if matches!(event, gilrs::EventType::Connected | gilrs::EventType::Disconnected) { refresh_pads(&gilrs); update_swap(&gilrs); }
                if WATCH.load(Relaxed) { wake(); }
                use gilrs::{Axis, Button, EventType};
                let button = |b: Button| std_index(b).and_then(|i| effective(&REMAPS.read().unwrap(), i));
                // a trigger that reports an axis (an unmapped or driver-mapped pad) becomes the LT / RT button on crossing 0.3 (works for -1..1 and 0..1 ranges)
                let mut trig: Option<(u8, bool)> = None;
                if let EventType::AxisChanged(ax @ (Axis::LeftZ | Axis::RightZ), v, _) = event {
                    let (i, st) = if ax == Axis::LeftZ { (6u8, &mut lt_down) } else { (7u8, &mut rt_down) };
                    let d = v > 0.3;
                    if d != *st { *st = d; trig = Some((i, d)); }
                }
                if let Some((i, d)) = trig {
                    { let mut l = LIVE.lock().unwrap(); if d { l.pressed |= 1 << i; } else { l.pressed &= !(1 << i); } }
                    if d && CAPTURE.load(Relaxed) { CAPTURED.store(i as i32, Relaxed); CAPTURE.store(false, Relaxed); }
                    else if let Some(n) = effective(&REMAPS.read().unwrap(), i) { if d { rep.press(n, now, &mut out); } else { rep.release(n, &mut out); } }
                }
                match event {
                    EventType::ButtonPressed(b, _) if CAPTURE.load(Relaxed) => { if let Some(i) = std_index(b) { CAPTURED.store(i as i32, Relaxed); CAPTURE.store(false, Relaxed); } }
                    EventType::ButtonPressed(b, _) => if let Some(n) = button(b) { rep.press(n, now, &mut out); },
                    EventType::ButtonReleased(b, _) => if let Some(n) = button(b) { rep.release(n, &mut out); },
                    EventType::AxisChanged(a, v, _) => {
                        match a { Axis::LeftStickX => sx = v, Axis::LeftStickY => sy = v, Axis::RightStickY => { rs = if enabled() { v } else { 0.0 }; scroll.store((rs * 1000.0) as i32, std::sync::atomic::Ordering::Relaxed); } _ => {} }
                        if matches!(a, Axis::LeftStickX | Axis::LeftStickY) {
                            let now_dir = stick_dir(sx, sy);
                            if now_dir != stick {
                                if let Some(old) = stick { rep.release(old, &mut out); }
                                if let Some(new) = now_dir { rep.press(new, now, &mut out); }
                                stick = now_dir;
                            }
                        }
                    }
                    EventType::Disconnected => {   // a held direction must not repeat forever
                        for n in [Nav::Up, Nav::Down, Nav::Left, Nav::Right, Nav::PagePrev, Nav::PageNext] { rep.release(n, &mut out); }
                        stick = None; sx = 0.0; sy = 0.0; rs = 0.0; scroll.store(0, std::sync::atomic::Ordering::Relaxed);
                    }
                    _ => {}
                }
            }
            // B held (button 1) while testing: 1.5 s ends the test
            if testing() && LIVE.lock().unwrap().pressed & 2 != 0 { b_since.get_or_insert(now); } else { b_since = None; }
            if let Some(t) = b_since { if now.duration_since(t) >= Duration::from_millis(1500) { set_test(false); b_since = None; wake(); } }
            rep.tick(now, &mut out);
            if !enabled() && rs != 0.0 { rs = 0.0; scroll.store(0, std::sync::atomic::Ordering::Relaxed); }
            if testing() { out.clear(); rep = Repeater::default(); stick = None; if rs != 0.0 { rs = 0.0; scroll.store(0, std::sync::atomic::Ordering::Relaxed); } }
            if !out.is_empty() { queue.lock().unwrap().extend(out); wake(); } else if rs.abs() >= dead_zone() { wake(); }
        }
    }).ok();
}

/// egui key for a Nav action. Confirm is Enter (egui activates the focused widget with it), Back is Escape (every dialog closes on it),
/// directions move egui's focus by geometry.
pub fn egui_key(n: Nav) -> Option<egui::Key> {
    Some(match n {
        Nav::Up => egui::Key::ArrowUp, Nav::Down => egui::Key::ArrowDown, Nav::Left => egui::Key::ArrowLeft, Nav::Right => egui::Key::ArrowRight,
        Nav::Confirm => egui::Key::Enter, Nav::Back => egui::Key::Escape,
        Nav::PagePrev => egui::Key::PageUp, Nav::PageNext => egui::Key::PageDown,
        Nav::Menu | Nav::Edit | Nav::Select | Nav::Start | Nav::Store | Nav::Folder | Nav::Achievements | Nav::Hub => return None,   // wired to app actions by the page that owns them
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remaps_follow_playdates_storage_rules() {
        let none = std::collections::HashMap::new();
        assert_eq!(effective(&none, 0), Some(Nav::Confirm)); assert_eq!(effective(&none, 8), Some(Nav::Select)); assert_eq!(effective(&none, 6), None);
        // Y moved to the right trigger (7): stored on 7, and Y's own button is silenced
        let mut a = assignments(&none); a[3] = Some(7);
        let st = to_storage(&a);
        assert_eq!(st, serde_json::json!({ "7": "y", "3": "none" }));
        let m = parse_remaps(&serde_json::json!({ "button_remaps": st }));
        assert_eq!(effective(&m, 7), Some(Nav::Edit)); assert_eq!(effective(&m, 3), None);
        assert_eq!(assignments(&m)[3], Some(7));
        // swapping X and Y stores both and silences nothing
        let mut a = assignments(&none); a.swap(2, 3);
        assert_eq!(to_storage(&a), serde_json::json!({ "3": "x", "2": "y" }));
        // a shortcut on Select, which Open Menu gave up
        let mut a = assignments(&none); a[6] = None; a[12] = Some(8);
        let m = parse_remaps(&serde_json::json!({ "button_remaps": to_storage(&a) }));
        assert_eq!(effective(&m, 8), Some(Nav::Store)); assert_eq!(assignments(&m)[6], None);
        assert_eq!(parse_remaps(&serde_json::json!({ "button_remaps": { "x": "a", "1": "bogus", "2": "none" } })).len(), 1);
    }

    #[test]
    fn saved_settings_are_clamped_and_default_when_missing() {
        assert_eq!(parse_state(&serde_json::json!({})), (true, 35, 400, 150));
        assert_eq!(parse_state(&serde_json::json!({ "gamepad_enabled": false, "gamepad_deadzone": 200, "gamepad_repeat_initial_ms": 5, "gamepad_repeat_rate_ms": 99999.0 })), (false, 90, 50, 1000));
    }

    #[test]
    fn pasted_sdl_lines_are_checked_and_comments_skipped() {
        let good = "030000005e040000ea02000000000000,Xbox Wireless Controller,a:b0,b:b1,x:b2,y:b3,platform:Linux,";
        assert!(sdl_line_ok(good));
        assert!(!sdl_line_ok("not a mapping"));
        assert!(!sdl_line_ok("0300,Short guid,a:b0"));
        assert!(!sdl_line_ok("030000005e040000ea02000000000000,,a:b0"));
        assert!(!sdl_line_ok("030000005e040000ea02000000000000,Name"));
        let st = serde_json::json!({ "gamepad_sdl_mappings": [format!("  {good}  "), "", "# a note", 5] });
        assert_eq!(parse_sdl_mappings(&st), vec![good.to_string()]);
        assert!(parse_sdl_mappings(&serde_json::json!({})).is_empty());
    }

    #[test]
    fn dead_zone_and_dominant_axis() {
        assert_eq!(stick_dir(0.2, 0.3), None);
        assert_eq!(stick_dir(0.5, 0.1), Some(Nav::Right));
        assert_eq!(stick_dir(-0.9, 0.5), Some(Nav::Left));
        assert_eq!(stick_dir(0.2, -0.8), Some(Nav::Down));
        assert_eq!(stick_dir(0.1, 0.6), Some(Nav::Up));
        assert_eq!(stick_dir(0.5, 0.5), Some(Nav::Right));   // a tie goes to the horizontal axis
    }

    #[test]
    fn repeat_timing() {
        let t0 = Instant::now();
        let mut r = Repeater::default();
        let mut out = Vec::new();
        r.press(Nav::Down, t0, &mut out);
        assert_eq!(out, vec![(Nav::Down, true)]);
        out.clear();
        r.tick(t0 + Duration::from_millis(399), &mut out);
        assert!(out.is_empty());
        r.tick(t0 + Duration::from_millis(400), &mut out);
        assert_eq!(out, vec![(Nav::Down, true)]);
        out.clear();
        r.tick(t0 + Duration::from_millis(540), &mut out);
        assert!(out.is_empty());
        r.tick(t0 + Duration::from_millis(550), &mut out);
        assert_eq!(out.len(), 1);
        out.clear();
        r.release(Nav::Down, &mut out);
        assert_eq!(out, vec![(Nav::Down, false)]);
        assert_eq!(r.next_due(t0), None);   // nothing held: the thread sleeps until the pad moves
    }

    #[test]
    fn buttons_fire_once_and_a_double_press_is_ignored() {
        let t0 = Instant::now();
        let mut r = Repeater::default();
        let mut out = Vec::new();
        r.press(Nav::Confirm, t0, &mut out);
        assert_eq!(r.next_due(t0), None);
        r.tick(t0 + Duration::from_secs(5), &mut out);
        assert_eq!(out.len(), 1);
        out.clear();
        r.press(Nav::Left, t0, &mut out);
        r.press(Nav::Left, t0, &mut out);   // the d-pad and the stick can both say Left
        assert_eq!(out.len(), 1);
    }
}

#[cfg(test)]
mod real_pad_tests {
    /// Needs a real controller plugged in (run with `--ignored`): with no saved mapping at all, `build_gilrs` still ends up with the pad mapped, from the bundled database.
    #[test]
    #[ignore]
    fn the_pad_is_mapped_automatically() {
        let g = super::build_gilrs("").expect("gilrs");
        let (_, pad) = g.gamepads().next().expect("a connected controller");
        println!("{} -> {:?}", pad.name(), pad.mapping_source());
        assert_eq!(pad.mapping_source(), gilrs::MappingSource::SdlMappings);
    }

    /// Needs a real controller plugged in (run with `--ignored`): the layout line built from the database and the pad's own GUID makes gilrs treat it as mapped.
    #[test]
    #[ignore]
    fn a_connected_pad_takes_the_known_layout() {
        let g = gilrs::Gilrs::new().expect("gilrs");
        let (_, pad) = g.gamepads().next().expect("a connected controller");
        let uuid: String = pad.uuid().iter().map(|b| format!("{b:02x}")).collect();
        let (v, p) = (pad.vendor_id().unwrap(), pad.product_id().unwrap());
        println!("pad {} uuid {uuid} {v:04x}:{p:04x} source before: {:?}", pad.name(), pad.mapping_source());
        let line = pd_filter::gamepad_db::mapping_line(&uuid, v, p).expect("a layout in the database");
        println!("line: {line}");
        assert!(super::sdl_line_ok(&line));
        let g2 = gilrs::GilrsBuilder::new().add_mappings(&line).build().expect("gilrs with the mapping");
        let (_, pad2) = g2.gamepads().next().expect("pad again");
        println!("source after: {:?}", pad2.mapping_source());
        assert_eq!(format!("{:?}", pad2.mapping_source()), "SdlMappings");
    }
}

#[cfg(test)]
mod label_tests {
    use super::*;
    #[test] fn styles_name_the_same_positions_differently() {
        assert_eq!(style_names(LabelStyle::Xbox)[0], "A");
        assert_eq!(style_names(LabelStyle::Ps)[0], "\u{2715}");
        assert_eq!((style_names(LabelStyle::Nintendo)[0], style_names(LabelStyle::Nintendo)[1]), ("B", "A"));
        assert_eq!(style_names(LabelStyle::Other)[3], "Top");
    }
    #[test] fn the_style_is_guessed_from_vendor_or_name() {
        assert_eq!(guess_style(Some(0x054c), "Wireless Controller"), LabelStyle::Ps);
        assert_eq!(guess_style(None, "DualSense Wireless"), LabelStyle::Ps);
        assert_eq!(guess_style(Some(0x057e), "Pro Controller"), LabelStyle::Nintendo);
        assert_eq!(guess_style(None, "Nintendo Switch Pro"), LabelStyle::Nintendo);
        assert_eq!(guess_style(Some(0x2dc8), "8BitDo Ultimate 2 Wireless Controller"), LabelStyle::Xbox);
    }
    #[test] fn a_pad_is_keyed_by_ids_else_name() {
        assert_eq!(pad_key(Some(0x2dc8), Some(0x310b), "x"), "2dc8:310b");
        assert_eq!(pad_key(None, Some(1), " Some Pad "), "some pad");
    }
    #[test] fn labels_round_trip_and_bad_entries_drop() {
        let state = serde_json::json!({ "gamepad_labels": {
            "2dc8:310b": { "style": "ps", "names": { "0": "Jump", "13": "  ", "99": "x", "x": "y", "3": "a very long button name" } },
            "bad": { "style": "weird" }, "none": {} } });
        let m = parse_labels(&state);
        assert_eq!(m.len(), 1);
        let l = &m["2dc8:310b"];
        assert_eq!(l.style, Some(LabelStyle::Ps));
        assert_eq!(l.names.get(&0).map(String::as_str), Some("Jump"));
        assert_eq!(l.names.get(&3).map(String::as_str), Some("a very long "[..12].trim()));
        assert_eq!(l.names.len(), 2, "blank, out-of-range and non-numeric names are dropped");
        assert_eq!(parse_labels(&serde_json::json!({ "gamepad_labels": labels_to_storage(&m) })), m);
        assert_eq!(label_name(0, LabelStyle::Xbox, l), "Jump");
        assert_eq!(label_name(1, LabelStyle::Ps, l), "\u{25CB}");
    }
}
