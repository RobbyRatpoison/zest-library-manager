//! The PAGYWOSG filter builder (Community Tools -> PAGYWOSG Filter Builder). Fetches an event from pagywosg.xyz in a thread, turns it into criteria with the
//! tested `pd_filter::pagywosg_*` code, lets the user adjust them and saves a named filter. The web builder is a long modal with an SQL box; this one
//! shows how many library games the filter lists instead, and keeps the SQL one click away.
use crate::filter_dialog::FilterDialog;
use crate::ui_kit::{self, Kind, MUTED, TEXT};
use egui::{Color32, RichText, Ui};
use pd_filter::pagywosg::op;
use pd_filter::pagywosg_filter::civil;
use pd_filter::pagywosg_event::{build_auto, current_event, fetch_event, load_gifts, load_personal_defaults, Auto, Gift, Supplements};
use pd_filter::pagywosg_filter::{count_matches, needs_dlc_note, spec_from_auto, Qual, Selection, Spec};
use serde_json::{json, Map, Value};
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::mpsc;

const BUNDLED_SUPPLEMENT: &str = include_str!("../../assets/pagywosg_supplement.json");
const BUNDLED_PERSONAL: &str = include_str!("../../assets/pagywosg_personal_defaults.json");
const STATUSES: [&str; 5] = ["Never Played", "Unfinished", "Beaten", "Completed", "Won't Play"];
const GOOD: Color32 = Color32::from_rgb(0x5c, 0xb8, 0x5c);
const BAD: Color32 = Color32::from_rgb(0xe0, 0x6c, 0x6c);

/// Columns a manual condition can look at: (column, label).
const COLUMNS: [(&str, &str); 14] = [
    ("name", "Title"), ("release_date", "Release Date"), ("appid", "AppID"), ("developers", "Developer"), ("publishers", "Publisher"), ("playtime_forever", "Playtime (mins)"),
    ("review_score", "Review Score"), ("review_percentage", "Review %"), ("groups", "Groups"), ("tags", "Tags"), ("hltb_main", "HLTB Main (mins)"),
    ("hltb_extras", "HLTB Main+Extras (mins)"), ("hltb_completionist", "HLTB Completionist (mins)"), ("total_achievements", "Achievements"),
];
const DATE_COLUMNS: [&str; 3] = ["release_date", "date_added", "last_played"];
/// Operators for ordinary columns and for dates, with the sentence the dropdown shows and what a value looks like.
const GENERAL_OPS: [(&str, &str, &str); 19] = [
    ("contains", "contains", "text"), ("not_contains", "does not contain", "text"), ("equals", "equals", "value"), ("starts_with", "starts with", "text"), ("ends_with", "ends with", "text"),
    ("gt", ">", "number"), ("lt", "<", "number"), ("gte", ">=", "number"), ("title_word", "title word", "word"), ("length_eq", "length is (characters)", "number"),
    ("digit_count_gte", "digit occurs N+ times", "D:N, e.g. 7:3"), ("consecutive_repeat", "N identical digits in a row", "number"), ("has_special_char", "has a special character", "(no value)"),
    ("range_incl", "between (inclusive)", "MIN:MAX"), ("all_caps", "is all-uppercase", "(no value)"), ("contains_all", "contains all of these letters", "letters, no separators"),
    ("tag_substring", "any tag containing", "text"), ("tag_count_eq", "has exactly N tags", "number"), ("single_word", "is a single word", "(no value)"),
];
const DATE_OPS: [(&str, &str, &str); 7] = [
    ("month_is", "month is (1-12)", "1-12"), ("day_is", "day is (1-31)", "1-31"), ("year_is", "year is", "YYYY"), ("weekday_is", "weekday is (0=Sun ... 6=Sat)", "0-6"),
    ("gt", "after", "YYYY-MM-DD"), ("lt", "before", "YYYY-MM-DD"), ("nth_weekday", "Nth weekday of month", "N:W, e.g. 2:4 (W 0=Sun)"),
];

/// What the dialog needs from the app when it opens.
pub struct PagEnv {
    pub db: PathBuf, pub root: PathBuf, pub sg_user: String,
    /// The library's tag vocabulary and groups.
    pub tags: Vec<String>, pub groups: Vec<String>,
    pub saved: Map<String, Value>,
    /// `state.json` `pagywosg_sg_group`: `None` = never chosen.
    pub sg_group: Option<Option<String>>,
    pub comp_defaults: Vec<String>,
}

/// What the dialog asks the app to do.
#[derive(Default)]
pub struct PagOut { pub close: bool, pub saved: Option<Map<String, Value>>, pub prefs: Vec<(&'static str, Value)>, pub toast: Option<String> }

#[derive(Clone, PartialEq)]
struct Row { col: String, op: String, val: String }

enum Phase { Idle, Loading(&'static str), Failed(String), Ready }
#[derive(Clone, Copy)]
enum Target { Current, Upcoming, Event(i64) }

pub struct PagDialog {
    redraws: u8, env: PagEnv, phase: Phase, rx: Option<mpsc::Receiver<Result<Auto, String>>>, auto: Option<Auto>,
    tags: [BTreeSet<String>; 2], search: [String; 2], rows: [Vec<Row>; 2],
    personal: BTreeSet<String>, personal_known: Vec<String>,
    santa_on: bool, gifts: Vec<Gift>, sg_group: Option<String>, statuses: BTreeSet<String>,
    name: String, status: Option<(String, bool)>, confirm_replace: bool, refresh_pick: String,
    preview: Option<Result<i64, String>>, dirty: bool, show_sql: bool, old_filters: Vec<String>,
    /// The saved filter being refreshed: its criteria are put back over the fresh event data.
    pending_refresh: Option<Spec>,
}

impl PagDialog {
    pub fn open(env: PagEnv, ctx: &egui::Context) -> PagDialog {
        let gifts = load_gifts(&env.root);
        let default_group = env.groups.iter().find(|g| g.eq_ignore_ascii_case("won on steamgifts")).cloned();
        let sg_group = match &env.sg_group { Some(g) => g.clone(), None => default_group };
        let statuses: BTreeSet<String> = if env.comp_defaults.is_empty() { ["Never Played", "Unfinished"].iter().map(|s| s.to_string()).collect() } else { env.comp_defaults.iter().cloned().collect() };
        let mut d = Self::blank(env, gifts, sg_group, statuses);
        d.load(ctx, Target::Current);
        d
    }

    fn blank(env: PagEnv, gifts: Vec<Gift>, sg_group: Option<String>, statuses: BTreeSet<String>) -> PagDialog {
        PagDialog { redraws: 0, env, phase: Phase::Idle, rx: None, auto: None, tags: Default::default(), search: Default::default(), rows: Default::default(), personal: BTreeSet::new(), personal_known: vec![],
            santa_on: false, gifts, sg_group, statuses, name: String::new(), status: None, confirm_replace: false, refresh_pick: String::new(), preview: None, dirty: false, show_sql: false,
            old_filters: vec![], pending_refresh: None }
    }

    fn load(&mut self, ctx: &egui::Context, target: Target) {
        let (db, root, user) = (self.env.db.clone(), self.env.root.clone(), self.env.sg_user.clone());
        let (tx, rx) = mpsc::channel();
        let ctx2 = ctx.clone();
        self.phase = Phase::Loading(match target { Target::Current => "Loading the current event...", Target::Upcoming => "Loading the upcoming event...", Target::Event(_) => "Loading the event..." });
        self.rx = Some(rx);
        std::thread::spawn(move || {
            let res = (|| -> Result<Auto, String> {
                let http = pd_filter::net::UreqHttp::new();
                let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
                let (y, m, d, _) = civil(now);
                let today = format!("{y:04}-{m:02}-{d:02}");
                let ev = match target {
                    Target::Current => current_event(&http, y as i32, m as i32, &today, false)?,
                    Target::Upcoming => current_event(&http, y as i32, m as i32, &today, true)?,
                    Target::Event(id) => fetch_event(&http, id)?,
                };
                let supp = Supplements::load(&root, BUNDLED_SUPPLEMENT);
                let personal = load_personal_defaults(&root, BUNDLED_PERSONAL, ev.id);
                let conn = rusqlite::Connection::open(&db).map_err(|e| e.to_string())?;
                conn.busy_timeout(std::time::Duration::from_secs(10)).ok();
                build_auto(&ev, &supp, &personal, &conn, &user)
            })();
            let _ = tx.send(res);
            ctx2.request_repaint();
        });
    }

    pub fn is_ready(&self) -> bool { self.auto.is_some() }
    /// Save under the current name without asking (the headless check).
    pub fn save_now(&mut self, out: &mut PagOut) { self.confirm_replace = true; self.save(out); }
    pub fn saved_name(&self) -> Option<String> { self.status.as_ref().filter(|s| s.1).and_then(|_| Some(self.name.trim().to_string())).filter(|n| self.env.saved.contains_key(n)) }
    /// False while an event is being fetched (the headless screenshot waits for it).
    pub fn settled(&self) -> bool { !matches!(self.phase, Phase::Loading(_)) }

    fn vocab_match(&self, tag: &str) -> Option<String> { self.env.tags.iter().find(|t| t.eq_ignore_ascii_case(tag)).cloned() }

    /// Fill the form from freshly loaded event data (what "Auto-fill" does in the web builder).
    fn apply_auto(&mut self, auto: Auto) {
        self.tags = Default::default();
        for (i, list) in [&auto.tags_wins, &auto.tags_all].into_iter().enumerate() { for t in list { if let Some(m) = self.vocab_match(t) { self.tags[i].insert(m); } } }
        let rows = |c: &[(String, String, String)]| c.iter().map(|(col, o, v)| Row { col: col.clone(), op: o.clone(), val: v.clone() }).collect::<Vec<_>>();
        self.rows = [rows(&auto.conds_wins), rows(&auto.conds_all)];
        self.personal = auto.default_personal.iter().cloned().collect();
        let mut known: BTreeSet<String> = BTreeSet::new();
        for g in auto.wins.games.iter().chain(auto.all.games.iter()) { for c in &g.categories { if !c.auto { known.insert(c.cat.clone()); } } }
        for c in &auto.personal_candidates { known.insert(c.clone()); }
        self.personal_known = known.into_iter().collect();
        let month_year = auto.event_name.split(" - ").next().unwrap_or("").trim().to_string();
        self.name = format!("PAGYWOSG {month_year}");
        self.status = None; self.confirm_replace = false; self.old_filters.clear();
        // a saved filter being refreshed puts its own criteria and choices back over the new event data
        if let Some(spec) = self.pending_refresh.take() {
            self.tags = [spec.wins.tags.iter().cloned().collect(), spec.all.tags.iter().cloned().collect()];
            let to_rows = |c: &[(String, String, String)]| c.iter().map(|(col, o, v)| Row { col: col.clone(), op: o.clone(), val: v.clone() }).collect::<Vec<_>>();
            let fresh = (auto.conds_wins.len() + auto.conds_all.len() + auto.tags_wins.len() + auto.tags_all.len()) as i64;
            let kept = (spec.wins.conds.len() + spec.all.conds.len() + spec.wins.tags.len() + spec.all.tags.len()) as i64;
            self.rows = [to_rows(&spec.wins.conds), to_rows(&spec.all.conds)];
            self.personal = spec.personal.iter().cloned().collect();
            self.statuses = if spec.statuses.is_empty() { STATUSES.iter().map(|s| s.to_string()).collect() } else { spec.statuses.iter().cloned().collect() };
            self.sg_group = spec.sg_group.clone();
            self.santa_on = spec.lists.iter().any(|l| l.santa);
            self.refresh_pick.clear();
            self.status = Some((if fresh != kept { format!("Refreshed from live data. The event now has {fresh} automatic criteria, this filter kept {kept}: Auto-fill again to start over.") } else { "Refreshed from live data. Review below, then Save.".into() }, true));
        } else {
            self.status = Some(("Auto-filled from the event.".into(), true));
        }
        self.auto = Some(auto);
        self.phase = Phase::Ready;
        self.dirty = true;
    }

    fn poll(&mut self) {
        let Some(rx) = &self.rx else { return };
        match rx.try_recv() {
            Ok(Ok(a)) => { self.rx = None; self.apply_auto(a); }
            Ok(Err(e)) => { self.rx = None; self.pending_refresh = None; self.phase = Phase::Failed(e); }
            Err(mpsc::TryRecvError::Empty) => {}
            Err(mpsc::TryRecvError::Disconnected) => { self.rx = None; self.phase = Phase::Failed("the loader stopped unexpectedly".into()); }
        }
    }

    fn convert(&self, r: &Row) -> Option<(String, String, String)> {
        let (col, o) = (r.col.as_str(), r.op.as_str());
        let val = r.val.trim();
        let no_value = matches!(o, "has_special_char" | "all_caps" | "single_word");
        if val.is_empty() && !no_value { return None; }
        if !value_ok(o, val) { return None; }
        // dates are typed as YYYY-MM-DD but stored as Unix times
        let val = if DATE_COLUMNS.contains(&col) && matches!(o, "gt" | "lt" | "gte") { ymd_to_ts(val)?.to_string() } else if no_value { "1".to_string() } else { val.to_string() };
        Some((col.to_string(), o.to_string(), val))
    }

    fn selection(&self) -> Selection {
        let conds = |i: usize| self.rows[i].iter().filter_map(|r| self.convert(r)).collect::<Vec<_>>();
        Selection {
            tags_wins: self.tags[0].iter().cloned().collect(), tags_all: self.tags[1].iter().cloned().collect(), conds_wins: conds(0), conds_all: conds(1),
            personal: self.personal.clone(), sg_group: self.sg_group.clone(),
            statuses: if self.statuses.len() == 5 { vec![] } else { STATUSES.iter().filter(|s| self.statuses.contains(**s)).map(|s| s.to_string()).collect() },
            santa: if self.santa_on && !self.gifts.is_empty() { Some(self.gifts.iter().map(|g| (g.appid, g.year)).collect()) } else { None },
        }
    }

    fn spec(&self) -> Option<Spec> { self.auto.as_ref().map(|a| spec_from_auto(a, &self.selection(), &self.env.sg_user)) }

    fn recount(&mut self) {
        self.dirty = false;
        let Some(spec) = self.spec() else { return };
        self.preview = Some(rusqlite::Connection::open_with_flags(&self.env.db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(|e| e.to_string()).and_then(|c| count_matches(&c, &spec)));
    }

    fn has_criteria(&self) -> bool {
        let sel = self.selection();
        !(sel.tags_wins.is_empty() && sel.tags_all.is_empty() && sel.conds_wins.is_empty() && sel.conds_all.is_empty()) || self.spec().map_or(false, |s| s.lists.iter().any(|l| !l.groups.is_empty()))
    }

    fn save(&mut self, out: &mut PagOut) {
        let name = self.name.trim().to_string();
        if name.is_empty() { self.status = Some(("Give the filter a name first.".into(), false)); return; }
        if !self.has_criteria() { self.status = Some(("No criteria selected: pick a tag or add a condition.".into(), false)); return; }
        if self.env.saved.contains_key(&name) && !self.confirm_replace {
            self.confirm_replace = true;
            self.status = Some((format!("A filter named \"{name}\" exists. Press Save again to replace it."), false));
            return;
        }
        let Some(spec) = self.spec() else { return };
        let id = self.env.saved.get(&name).and_then(|e| e.get("id")).and_then(|v| v.as_str()).map(String::from).unwrap_or_else(crate::filter_model::new_id);
        self.env.saved.insert(name.clone(), json!({"id": id, "tree": spec.to_tree()}));
        out.saved = Some(self.env.saved.clone());
        self.confirm_replace = false;
        let this_event = spec.event_id;
        self.old_filters = self.env.saved.iter().filter(|(n, _)| **n != name).filter_map(|(n, e)| {
            let s = pd_filter::pagywosg_filter::spec_of(e.get("tree").unwrap_or(e))?;
            (s.event_id.is_some() && s.event_id != this_event).then(|| n.clone())
        }).collect();
        self.status = Some((format!("Saved as \"{name}\"."), true));
    }

    fn pag_filters(&self) -> Vec<String> { self.env.saved.iter().filter(|(_, e)| pd_filter::pagywosg_filter::spec_of(e.get("tree").unwrap_or(e)).is_some()).map(|(n, _)| n.clone()).collect() }

    pub fn show(&mut self, ctx: &egui::Context, sql_preview: &dyn Fn(&Value) -> String) -> PagOut {
        let was_loading = self.rx.is_some();
        self.poll();
        // the answer changes the dialog's size: draw again at once (and once more for egui's sizing pass) rather than wait for the mouse to move
        if was_loading && self.rx.is_none() { self.redraws = 8; }
        if self.redraws > 0 { self.redraws -= 1; ctx.request_repaint(); }   // a few frames in a row: the dialog and its list take their new size over two or three of them
        if matches!(self.phase, Phase::Loading(_)) { ctx.request_repaint_after(std::time::Duration::from_millis(200)); }
        let mut out = PagOut::default();
        let accent = ui_kit::accent();
        // the footer (name, Save, status) lives outside the body so it stays put while the criteria scroll
        let mut name = std::mem::take(&mut self.name);
        let (status, old) = (self.status.clone(), self.old_filters.clone());
        let ready = self.auto.is_some();
        let (mut save, mut close, mut del_old, mut keep_old) = (false, false, false, false);
        let screen_h = ctx.content_rect().height();
        let o = ui_kit::modal_full(ctx, "pagywosg", "PAGYWOSG Filter Builder", 50.0, |_, _| {}, |ui, r| self.body(ui, r, accent, &mut out, sql_preview, screen_h), |ui, r| {
            if ready {
                ui.horizontal(|ui| {
                    FilterDialog::input(ui, &mut name, 22.0 * r, r, "Filter name...");
                    if ui_kit::button(ui, "Save", Kind::Primary, accent, r).clicked() { save = true; }
                    if ui_kit::button(ui, "Close", Kind::Plain, accent, r).clicked() { close = true; }
                });
            } else if ui_kit::button(ui, "Close", Kind::Plain, accent, r).clicked() { close = true; }
            if let Some((t, good)) = &status { ui.label(RichText::new(t).size(0.85 * r).color(if *good { GOOD } else { BAD })); }
            if !old.is_empty() {
                ui.horizontal_wrapped(|ui| {
                    ui.label(ui_kit::muted(format!("{} saved filter{} from earlier events: {}", old.len(), if old.len() == 1 { "" } else { "s" }, old.join(", ")), 0.82 * r));
                    if ui_kit::button(ui, "Delete them", Kind::Danger, accent, r).clicked() { del_old = true; }
                    if ui_kit::button(ui, "Keep", Kind::Plain, accent, r).clicked() { keep_old = true; }
                });
            }
        });
        self.name = name;
        if o.closed || close { out.close = true; }
        if save { self.save(&mut out); }
        if keep_old { self.old_filters.clear(); }
        if del_old {
            for n in std::mem::take(&mut self.old_filters) { self.env.saved.shift_remove(&n); }
            out.saved = Some(self.env.saved.clone());
            self.status = Some(("Removed the filters from earlier events.".into(), true));
        }
        if self.dirty { self.recount(); }
        out
    }

    fn body(&mut self, ui: &mut Ui, r: f32, accent: Color32, out: &mut PagOut, sql_preview: &dyn Fn(&Value) -> String, screen_h: f32) {
        let ctx = ui.ctx().clone();
        // event bar
        ui.horizontal_wrapped(|ui| {
            let busy = matches!(self.phase, Phase::Loading(_));
            if ui_kit::button(ui, "Current event", Kind::Primary, accent, r).clicked() && !busy { self.pending_refresh = None; self.load(&ctx, Target::Current); }
            if ui_kit::button(ui, "Upcoming event", Kind::Plain, accent, r).clicked() && !busy { self.pending_refresh = None; self.load(&ctx, Target::Upcoming); }
            if let Some(a) = &self.auto { ui.label(RichText::new(&a.event_name).italics().size(0.85 * r).color(MUTED())); }
        });
        // refresh a saved filter
        let saved = self.pag_filters();
        if !saved.is_empty() {
            ui.horizontal(|ui| {
                let opts: Vec<(String, String)> = saved.iter().map(|n| (n.clone(), n.clone())).collect();
                let shown = if self.refresh_pick.is_empty() { "Refresh a saved filter..." } else { self.refresh_pick.as_str() }.to_string();
                if let Some(p) = FilterDialog::combo(ui, egui::Id::new("pag_refresh"), &shown, &opts, 17.0 * r, r) { self.refresh_pick = p; }
                if ui_kit::button(ui, "Refresh", Kind::Plain, accent, r).clicked() && !self.refresh_pick.is_empty() && !matches!(self.phase, Phase::Loading(_)) {
                    let name = self.refresh_pick.clone();
                    let spec = self.env.saved.get(&name).and_then(|e| pd_filter::pagywosg_filter::spec_of(e.get("tree").unwrap_or(e)));
                    match spec {
                        Some(s) => match s.event_id {
                            Some(id) => { self.pending_refresh = Some(s); self.load(&ctx, Target::Event(id)); self.name = name; }
                            None => self.status = Some(("That filter does not say which event it was built for; rebuild it with Auto-fill.".into(), false)),
                        },
                        None => {}
                    }
                }
            });
        }
        match &self.phase {
            Phase::Idle => {}
            Phase::Loading(t) => { ui.label(ui_kit::muted(*t, 0.9 * r)); }
            Phase::Failed(e) => { ui.label(RichText::new(format!("Could not load the event: {e}")).color(BAD).size(0.9 * r)); }
            Phase::Ready => {}
        }
        if !matches!(self.phase, Phase::Ready) && self.auto.is_none() { return; }
        ui.add_space(0.3 * r);
        egui::ScrollArea::vertical().max_height((screen_h - 24.0 * r).max(14.0 * r)).auto_shrink([false, true]).show(ui, |ui| {
            for pool in 0..2 { self.criteria(ui, r, accent, pool); ui.add_space(0.6 * r); ui.separator(); ui.add_space(0.4 * r); }
            self.personal_ui(ui, r);
            self.group_and_status_ui(ui, r, accent, out);
        });
        ui.add_space(0.4 * r);
        ui.separator();
        // preview
        ui.horizontal_wrapped(|ui| {
            match &self.preview {
                Some(Ok(n)) => { ui.label(RichText::new(format!("This filter lists {n} games in your library.")).color(TEXT()).size(0.9 * r)); }
                Some(Err(e)) => { ui.label(RichText::new(format!("Could not count: {e}")).color(BAD).size(0.85 * r)); }
                None => {}
            }
            if ui_kit::button(ui, if self.show_sql { "Hide SQL" } else { "Show SQL" }, Kind::Plain, accent, r).clicked() { self.show_sql = !self.show_sql; }
        });
        if self.show_sql {
            if let Some(spec) = self.spec() {
                let text = sql_preview(&spec.to_tree());
                egui::ScrollArea::vertical().max_height(8.0 * r).id_salt("pag_sql").show(ui, |ui| { ui.add(egui::Label::new(RichText::new(text).monospace().size(0.78 * r).color(accent)).wrap()); });
            }
        }
    }

    /// One pool: wins-only (0) or all games (1).
    fn criteria(&mut self, ui: &mut Ui, r: f32, accent: Color32, pool: usize) {
        let (title, sub) = if pool == 0 { ("CRITERIA: WINS ONLY", "only applies to games won on SteamGifts") } else { ("CRITERIA: ALL GAMES", "applies to your whole library") };
        ui.horizontal(|ui| { ui.label(RichText::new(title).strong().size(0.8 * r).color(if pool == 0 { accent } else { TEXT() })); ui.label(RichText::new(sub).italics().size(0.78 * r).color(MUTED())); });
        if pool == 0 {
            ui.horizontal_wrapped(|ui| {
                let n = self.gifts.len();
                let was = self.santa_on;
                ui.add_enabled(n > 0, egui::Checkbox::new(&mut self.santa_on, RichText::new(format!("Include Secret Santa / Snowballs gifts ({n})")).size(0.85 * r).color(TEXT())));
                if n == 0 { ui.label(ui_kit::muted("add gifts under Community Tools > Secret Santa / Snowballs", 0.78 * r)); }
                if was != self.santa_on { self.dirty = true; }
            });
        }
        let sel: Vec<String> = self.tags[pool].iter().cloned().collect();
        ui.label(ui_kit::muted(if sel.is_empty() { "No tags selected".to_string() } else { format!("Tags: {}", sel.join(", ")) }, 0.8 * r));
        FilterDialog::input(ui, &mut self.search[pool], f32::INFINITY, r, "Search tags...");
        let q = self.search[pool].to_lowercase();
        let shown: Vec<String> = self.env.tags.iter().filter(|t| self.tags[pool].contains(*t) || (!q.is_empty() && t.to_lowercase().contains(&q))).cloned().collect();
        let shown = if q.is_empty() { self.env.tags.iter().take(0).cloned().chain(sel.iter().cloned()).collect() } else { shown };
        if q.is_empty() { ui.label(ui_kit::muted("Type to find a tag. Selected tags stay listed.", 0.76 * r)); }
        ui.horizontal_wrapped(|ui| {
            for t in shown.iter().take(200) {
                let on = self.tags[pool].contains(t);
                if ui_kit::pill(ui, t, on, accent, 0.78 * r).clicked() { if on { self.tags[pool].remove(t); } else { self.tags[pool].insert(t.clone()); } self.dirty = true; }
            }
        });
        // conditions
        let mut remove = None;
        for i in 0..self.rows[pool].len() {
            let mut row = self.rows[pool][i].clone();
            let (gone, changed) = cond_row(ui, egui::Id::new(("pag_row", pool, i)), &mut row, r);
            if gone { remove = Some(i); }
            if changed { self.rows[pool][i] = row; self.dirty = true; }
        }
        if let Some(i) = remove { self.rows[pool].remove(i); self.dirty = true; }
        if ui_kit::button(ui, "+ Add condition", Kind::Plain, accent, r).clicked() { self.rows[pool].push(Row { col: "name".into(), op: "contains".into(), val: String::new() }); }
        // appids named by the event
        if let Some(a) = &self.auto {
            let p = if pool == 0 { &a.wins } else { &a.all };
            let personal = &self.personal;
            let extra: Vec<_> = p.games.iter().filter(|g| g.in_library && !g.redundant && g.categories.iter().any(|c| c.auto || !personal.contains(&c.cat))).collect();
            if !extra.is_empty() {
                let head = egui::CollapsingHeader::new(RichText::new(format!("{} additional game{} included", extra.len(), if extra.len() == 1 { "" } else { "s" })).color(accent).size(0.82 * r)).id_salt(("pag_extra", pool)).show(ui, |ui| {
                    let list = egui::ScrollArea::vertical().max_height(8.0 * r).id_salt(("pag_extra_scroll", pool)).show(ui, |ui| {
                        for g in extra.iter().take(400) {
                            let cats: Vec<String> = g.categories.iter().map(|c| match &c.verifiers { Some(v) if !v.is_empty() => format!("{} (via {})", c.cat, v.join(", ")), _ => c.cat.clone() }).collect();
                            ui.label(ui_kit::muted(format!("{}  -  {}", g.name, cats.join(", ")), 0.78 * r));
                        }
                    });
                    ui.ctx().data_mut(|d| d.insert_temp(egui::Id::new(("pad_no_controls", list.id)), true));   // only text: the pad steps over it
                    list.id
                });
                if head.header_response.has_focus() && std::env::var_os("ZEST_PAD_DEBUG").is_some() { eprintln!("HEADER FOCUSED pool={pool} id={:?} body={:?}", head.header_response.id, head.body_returned); }
                // the pad's right stick scrolls this list while its header has the focus (None while collapsed)
                ui.ctx().data_mut(|d| d.insert_temp(egui::Id::new(("pad_header_list", head.header_response.id)), head.body_returned));
            }
        }
    }

    fn personal_ui(&mut self, ui: &mut Ui, r: f32) {
        if self.personal_known.is_empty() { return; }
        ui.label(RichText::new("PERSONAL CATEGORIES").strong().size(0.78 * r).color(MUTED()));
        ui.label(ui_kit::muted("Categories where eligibility depends on your own history. Checking one keeps only the entries verified for your own SteamGifts name (Settings); an entry verified for someone else does not apply to you.", 0.78 * r));
        let known = self.personal_known.clone();
        for c in known {
            let mut on = self.personal.contains(&c);
            if ui.checkbox(&mut on, RichText::new(&c).size(0.85 * r).color(TEXT())).changed() { if on { self.personal.insert(c.clone()); } else { self.personal.remove(&c); } self.dirty = true; }
        }
        ui.add_space(0.4 * r);
    }

    fn group_and_status_ui(&mut self, ui: &mut Ui, r: f32, accent: Color32, out: &mut PagOut) {
        ui.horizontal_wrapped(|ui| {
            ui.label(ui_kit::muted("SteamGifts wins group", 0.85 * r));
            let none = "(none: no SteamGifts wins)".to_string();
            let mut opts = vec![(String::new(), none.clone())];
            opts.extend(self.env.groups.iter().map(|g| (g.clone(), g.clone())));
            let text = self.sg_group.clone().unwrap_or(none);
            if let Some(p) = FilterDialog::combo(ui, egui::Id::new("pag_group"), &text, &opts, 18.0 * r, r) {
                self.sg_group = if p.is_empty() { None } else { Some(p) };
                out.prefs.push(("pagywosg_sg_group", self.sg_group.as_ref().map_or(Value::Null, |g| json!(g))));
                self.dirty = true;
            }
        });
        if self.sg_group.is_none() && !self.santa_on { ui.label(RichText::new("Without a wins group, the wins-only criteria cannot match anything.").size(0.8 * r).color(BAD)); }
        ui.horizontal_wrapped(|ui| {
            ui.label(ui_kit::muted("Completion status", 0.85 * r));
            for s in STATUSES {
                let on = self.statuses.contains(s);
                if ui_kit::pill(ui, s, on, accent, 0.78 * r).clicked() {
                    if on { self.statuses.remove(s); } else { self.statuses.insert(s.to_string()); }
                    out.prefs.push(("pagywosg_comp_defaults", json!(self.statuses.iter().collect::<Vec<_>>())));
                    self.dirty = true;
                }
            }
        });
    }
}

/// A condition row: column, operator, value, remove. Returns (remove, changed).
fn cond_row(ui: &mut Ui, id: egui::Id, row: &mut Row, r: f32) -> (bool, bool) {
    let (mut gone, mut changed) = (false, false);
    ui.horizontal(|ui| {
        let cols: Vec<(String, String)> = COLUMNS.iter().map(|(v, l)| (v.to_string(), l.to_string())).collect();
        let label = COLUMNS.iter().find(|c| c.0 == row.col).map_or(row.col.as_str(), |c| c.1).to_string();
        if let Some(c) = FilterDialog::combo(ui, id.with("col"), &label, &cols, 11.0 * r, r) {
            row.col = c;
            if !ops_for(&row.col).iter().any(|o| o.0 == row.op) { row.op = ops_for(&row.col)[0].0.to_string(); }
            changed = true;
        }
        let ops = ops_for(&row.col);
        let opts: Vec<(String, String)> = ops.iter().map(|o| (o.0.to_string(), o.1.to_string())).collect();
        let olabel = ops.iter().find(|o| o.0 == row.op).map_or(row.op.as_str(), |o| o.1).to_string();
        if let Some(o) = FilterDialog::combo(ui, id.with("op"), &olabel, &opts, 15.0 * r, r) { row.op = o; changed = true; }
        let hint = ops.iter().find(|o| o.0 == row.op).map_or("value", |o| o.2);
        if FilterDialog::input(ui, &mut row.val, 10.0 * r, r, hint).changed() { changed = true; }
        if !row.val.trim().is_empty() && !value_ok(&row.op, row.val.trim()) { ui.label(RichText::new("?").color(BAD).size(0.9 * r)).on_hover_text(format!("Expected: {hint}")); }
        if ui_kit::button(ui, "\u{2715}", Kind::Plain, ui_kit::accent(), r).clicked() { gone = true; }
    });
    (gone, changed)
}

fn ops_for(col: &str) -> &'static [(&'static str, &'static str, &'static str)] { if DATE_COLUMNS.contains(&col) { &DATE_OPS } else { &GENERAL_OPS } }

/// Whether a typed value has the shape the operator needs (a bad one is left out of the filter rather than breaking it).
fn value_ok(o: &str, v: &str) -> bool {
    let num = |s: &str| s.trim().parse::<i64>().is_ok();
    match o {
        "has_special_char" | "all_caps" | "single_word" => true,
        "length_eq" | "consecutive_repeat" | "tag_count_eq" | "month_is" | "day_is" | "year_is" | "weekday_is" | "gte" => num(v),
        "gt" | "lt" => num(v) || ymd_to_ts(v).is_some(),
        "digit_count_gte" | "range_incl" | "nth_weekday" => v.split_once(':').map_or(false, |(a, b)| num(a) && num(b)),
        "contains_all" => !v.is_empty(),
        _ => op(o).is_some() && !v.is_empty(),
    }
}

/// `YYYY-MM-DD` as a Unix time (UTC midnight).
fn ymd_to_ts(s: &str) -> Option<i64> {
    let mut it = s.trim().splitn(3, '-');
    let (y, m, d): (i64, i64, i64) = (it.next()?.parse().ok()?, it.next()?.parse().ok()?, it.next()?.parse().ok()?);
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) { return None; }
    let (y2, m2) = if m <= 2 { (y - 1, m + 9) } else { (y, m - 3) };
    let era = y2.div_euclid(400);
    let yoe = y2.rem_euclid(400);
    let doy = (153 * m2 + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some((era * 146097 + doe - 719468) * 86400)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn dates_become_unix_times() { assert_eq!(ymd_to_ts("1970-01-01"), Some(0)); assert_eq!(ymd_to_ts("2019-01-01"), Some(1546300800)); assert_eq!(ymd_to_ts("2024-02-29"), Some(1709164800)); assert_eq!(ymd_to_ts("nope"), None); }
    #[test] fn values_are_checked_by_operator() {
        assert!(value_ok("digit_count_gte", "7:3") && !value_ok("digit_count_gte", "73"));
        assert!(value_ok("range_incl", "1:12") && !value_ok("range_incl", "a:b"));
        assert!(value_ok("month_is", "3") && !value_ok("month_is", "march"));
        assert!(value_ok("lt", "2020-05-01") && value_ok("lt", "100") && !value_ok("lt", "soon"));
        assert!(value_ok("single_word", "") && value_ok("contains", "x") && !value_ok("contains", ""));
    }
}


/// What the edit dialog, list view and card tooltip show about one game under a PAGYWOSG filter.
#[derive(Clone, Debug, PartialEq)]
pub struct PagPanel { pub header: String, pub lines: Vec<Qual>, pub dlc_note: bool, pub me: String, pub hltb_min: Option<f64> }

impl PagPanel {
    /// `month_year` is the event's name before " - ".
    pub fn new(event_name: &str, lines: Vec<Qual>, me: &str, hltb_min: Option<f64>) -> PagPanel {
        let month = event_name.split(" - ").next().unwrap_or("").trim();
        PagPanel { header: format!("PAGYWOSG - {} QUALIFICATIONS", if month.is_empty() { "EVENT".to_string() } else { month.to_uppercase() }), dlc_note: needs_dlc_note(&lines), lines, me: me.to_lowercase(), hltb_min }
    }
}

/// Draws the panel. `compact` is the tooltip form: no header, verifier names inline.
pub fn panel_ui(ui: &mut Ui, r: f32, p: &PagPanel, compact: bool, dim: Color32, bright: Color32) {
    if !compact { ui.label(RichText::new(&p.header).strong().size(0.78 * r).color(dim)); ui.add_space(0.2 * r); }
    for q in &p.lines {
        let mut job = egui::text::LayoutJob::default();
        let f = |c: Color32, size: f32| egui::text::TextFormat { font_id: egui::FontId::proportional(size), color: c, ..Default::default() };
        job.append("\u{2713} ", 0.0, f(GOOD, 0.9 * r));
        job.append(&q.desc, 0.0, f(bright, 0.9 * r));
        job.append(&format!("  {}", q.label), 0.0, f(dim, 0.8 * r));
        if q.verified {
            let own = !p.me.is_empty() && q.verifiers.iter().any(|v| v.to_lowercase() == p.me);
            let who = if compact { if q.verifiers.is_empty() { String::new() } else { format!(" ({})", q.verifiers.join(", ")) } }
                      else if q.verifiers.is_empty() { String::new() } else if own { " \u{2014} already submitted".into() } else { format!(" \u{2014} see {}'s entry", q.verifiers[0]) };
            job.append(&format!(" \u{2014} mod verified{who}"), 0.0, f(dim, 0.8 * r));
        }
        ui.add(egui::Label::new(job).wrap());
    }
    if p.dlc_note { ui.add_space(0.2 * r); ui.label(RichText::new("* Won a DLC, not the base game itself: mention that when submitting the game to PAGYWOSG.").size(0.74 * r).color(dim)); }
    if let (true, Some(m)) = (compact, p.hltb_min) { ui.add_space(0.2 * r); ui.label(RichText::new(format!("HLTB min: {}", crate::home::fmt_hours(m))).size(0.85 * r).color(dim)); }
}

#[cfg(test)]
mod flow_tests {
    use super::*;
    use pd_filter::pagywosg_event::{CatEntry, GameRow, Pool};

    fn env(saved: Map<String, Value>) -> PagEnv {
        PagEnv { db: PathBuf::from("/nonexistent.db"), root: PathBuf::from("/nonexistent"), sg_user: "alice".into(), tags: vec!["Roguelike".into(), "RPG".into()], groups: vec!["Won on SteamGifts".into()],
                 saved, sg_group: Some(Some("Won on SteamGifts".into())), comp_defaults: vec![] }
    }
    fn auto(event: i64) -> Auto {
        let ce = |c: &str| CatEntry { cat: c.into(), auto: false, verifiers: Some(vec!["Alice".into()]) };
        let mut a = Auto { event_id: event, event_name: format!("May 2027 - Test {event}"), tags_wins: vec!["roguelike".into(), "Unknown Tag".into()], tags_all: vec!["RPG".into()],
                           conds_wins: vec![("name".into(), "starts_with".into(), "Q".into())], default_personal: vec!["Cat A".into()], cat_order: vec!["Cat A".into()], ..Default::default() };
        a.wins = Pool { appids: vec![5], games: vec![GameRow { appid: 5, name: "Five".into(), categories: vec![ce("Cat A")], in_library: true, redundant: false }], ..Default::default() };
        a
    }
    fn dialog(saved: Map<String, Value>, a: Auto) -> PagDialog {
        let mut d = PagDialog::blank(env(saved), vec![], Some("Won on SteamGifts".into()), ["Never Played".to_string()].into_iter().collect());
        d.apply_auto(a);
        d
    }

    #[test]
    fn auto_fill_maps_tags_onto_the_library_vocabulary_and_names_the_filter() {
        let d = dialog(Map::new(), auto(91));
        assert_eq!(d.tags[0].iter().collect::<Vec<_>>(), vec!["Roguelike"]);   // case fixed, unknown tag dropped
        assert_eq!(d.tags[1].iter().collect::<Vec<_>>(), vec!["RPG"]);
        assert_eq!(d.rows[0].len(), 1);
        assert_eq!(d.name, "PAGYWOSG May 2027");
        assert!(d.personal.contains("Cat A"));
    }

    #[test]
    fn saving_writes_a_pag_filter_and_offers_to_remove_older_events() {
        let mut saved = Map::new();
        let old = spec_from_auto(&auto(90), &Selection { tags_all: vec!["RPG".into()], ..Default::default() }, "alice");
        saved.insert("PAGYWOSG April".into(), json!({"id": "x", "tree": old.to_tree()}));
        saved.insert("Plain".into(), json!({"id": "y", "tree": {"type": "group", "logic": "AND", "items": []}}));
        let mut d = dialog(saved, auto(91));
        let mut out = PagOut::default();
        d.save(&mut out);
        let map = out.saved.expect("saved");
        let e = &map["PAGYWOSG May 2027"];
        assert!(e["id"].as_str().unwrap().len() > 8);
        let spec = Spec::from_tree(&e["tree"]).unwrap();
        assert_eq!((spec.event_id, spec.wins.tags.clone(), spec.statuses.clone()), (Some(91), vec!["Roguelike".to_string()], vec!["Never Played".to_string()]));
        // the personal list kept only the entry verified for alice (verifier match is case-insensitive)
        assert_eq!(spec.lists[0].all_ids(), vec![5]);
        assert_eq!(d.old_filters, vec!["PAGYWOSG April".to_string()]);   // the plain filter is not a PAGYWOSG one
    }

    #[test]
    fn replacing_a_name_needs_a_second_press_and_nothing_to_save_is_refused() {
        let mut d = dialog(Map::new(), auto(91));
        let mut out = PagOut::default();
        d.save(&mut out);
        let mut d2 = PagDialog::blank(PagEnv { saved: out.saved.clone().unwrap(), ..env(Map::new()) }, vec![], None, BTreeSet::new());
        d2.apply_auto(auto(91));
        let mut out2 = PagOut::default();
        d2.save(&mut out2);
        assert!(out2.saved.is_none() && d2.confirm_replace, "same name: asks first");
        d2.save(&mut out2);
        assert!(out2.saved.is_some());
        // no tags, no conditions, no lists
        let mut empty = auto(92);
        empty.tags_wins.clear(); empty.tags_all.clear(); empty.conds_wins.clear(); empty.wins = Pool::default();
        let mut d3 = dialog(Map::new(), empty);
        let mut out3 = PagOut::default();
        d3.save(&mut out3);
        assert!(out3.saved.is_none() && d3.status.as_ref().map_or(false, |s| !s.1));
    }

    #[test]
    fn refreshing_puts_the_saved_choices_back_over_new_event_data() {
        let mut d = dialog(Map::new(), auto(91));
        d.tags[1].insert("Roguelike".into());
        let mut out = PagOut::default();
        d.save(&mut out);
        let saved_spec = Spec::from_tree(&out.saved.unwrap()["PAGYWOSG May 2027"]["tree"]).unwrap();
        let mut d2 = dialog(Map::new(), auto(91));
        d2.pending_refresh = Some(saved_spec);
        let mut fresh = auto(91);
        fresh.tags_all = vec!["RPG".into(), "RPG".into()];
        d2.apply_auto(fresh);
        assert!(d2.tags[1].contains("Roguelike") && d2.tags[1].contains("RPG"), "the saved tags win over the automatic ones");
        assert!(d2.status.as_ref().map_or(false, |s| s.0.starts_with("Refreshed")));
    }
}

/// The current event as a spec with every automatic criterion and the maintainer's personal defaults, for the Edit dialog's qualifications when no
/// PAGYWOSG filter is active (PlayDate's `/api/pagywosg-quals`). `sg_group`: the saved choice, else the group named like SteamGifts wins.
pub fn live_spec(db: &std::path::Path, root: &std::path::Path, sg_user: &str, sg_group: Option<Option<String>>) -> Result<Spec, String> {
    let http = pd_filter::net::UreqHttp::new();
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
    let (y, m, d, _) = civil(now);
    let ev = current_event(&http, y as i32, m as i32, &format!("{y:04}-{m:02}-{d:02}"), false)?;
    let supp = Supplements::load(root, BUNDLED_SUPPLEMENT);
    let personal = load_personal_defaults(root, BUNDLED_PERSONAL, ev.id);
    let conn = rusqlite::Connection::open_with_flags(db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(|e| e.to_string())?;
    let auto = build_auto(&ev, &supp, &personal, &conn, sg_user)?;
    let group = match sg_group { Some(g) => g, None => Some("Won on SteamGifts".to_string()) };
    let sel = Selection { tags_wins: auto.tags_wins.clone(), tags_all: auto.tags_all.clone(), conds_wins: auto.conds_wins.clone(), conds_all: auto.conds_all.clone(),
        personal: auto.default_personal.iter().cloned().collect(), sg_group: group, ..Default::default() };
    Ok(spec_from_auto(&auto, &sel, sg_user))
}
