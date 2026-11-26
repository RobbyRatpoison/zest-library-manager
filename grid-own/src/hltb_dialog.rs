//! Menu -> HLTB: the HowLongToBeat matches of the library. Games whose match is below the threshold wait for review (confirm it, choose another, or say there is no page), confirmed
//! ones are listed for reference, and games with no match can be searched by hand or by pasting an address. Confirming many at once and re-scraping the unmatched run as jobs
//! (they go on when the dialog is closed); one game's actions run on a thread and the lists are read again afterwards.
use crate::ui_kit::{self, Kind};
use pd_filter::hltb_ops::{self, Overview};
use pd_filter::hltbclient::{Hltb, HltbData};
use std::path::PathBuf;
use std::sync::{mpsc, Arc};

pub enum HltbOut { None, Close, /// Confirm these games' stored matches as a job.
    ConfirmAll(Vec<i64>), /// Look these games up again as a job.
    Rescrape(Vec<i64>) }

enum Msg { Loaded(Result<Overview, String>), Results(i64, Vec<HltbData>), Op(Result<String, String>) }

struct Choose { appid: i64, name: String, query: String, results: Option<Vec<HltbData>>, manual: String }

pub struct HltbDialog {
    db: PathBuf, ov: Overview, filter: String, threshold: i64, status: String, loading: bool, busy: bool, open: [bool; 3],
    choose: Option<Choose>, was_job: bool,
    hltb: Arc<Hltb>, tx: mpsc::Sender<Msg>, rx: mpsc::Receiver<Msg>, ctx: egui::Context,
}

fn hours(m: Option<i64>) -> String { match m { None => "-".into(), Some(m) => { let h = m as f64 / 60.0; if h >= 10.0 { format!("{:.0}h", h) } else { format!("{:.1}h", h) } } } }
fn today() -> String {
    let s = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
    pd_filter::backup::ts_to_date(&rusqlite::types::Value::Integer(s))
}

impl HltbDialog {
    pub fn new(ctx: &egui::Context, db: PathBuf, threshold: i64) -> Self {
        let (tx, rx) = mpsc::channel();
        let mut d = HltbDialog { db, ov: Overview::default(), filter: String::new(), threshold: threshold.clamp(0, 100), status: String::new(), loading: false, busy: false, open: [true, false, true], choose: None, was_job: false,
            hltb: Arc::new(Hltb::new(Arc::new(pd_filter::net::UreqHttp::new()))), tx, rx, ctx: ctx.clone() };
        d.reload(); d
    }
    fn conn(db: &PathBuf) -> Result<rusqlite::Connection, String> { let c = rusqlite::Connection::open(db).map_err(|e| e.to_string())?; c.busy_timeout(std::time::Duration::from_secs(10)).ok(); Ok(c) }
    fn reload(&mut self) {
        self.loading = true;
        let (db, tx, ctx) = (self.db.clone(), self.tx.clone(), self.ctx.clone());
        std::thread::spawn(move || { let r = Self::conn(&db).and_then(|c| hltb_ops::overview(&c).map_err(|e| e.to_string())); let _ = tx.send(Msg::Loaded(r)); ctx.request_repaint(); });
    }
    fn op(&mut self, label: &str, f: impl FnOnce(&rusqlite::Connection, &Hltb) -> Result<String, String> + Send + 'static) {
        self.busy = true; self.status = label.to_string();
        let (db, tx, ctx, h) = (self.db.clone(), self.tx.clone(), self.ctx.clone(), self.hltb.clone());
        std::thread::spawn(move || { let r = Self::conn(&db).and_then(|c| f(&c, &h)); let _ = tx.send(Msg::Op(r)); ctx.request_repaint(); });
    }
    fn open_choose(&mut self, appid: i64, name: String) {
        let q = name.clone();
        self.choose = Some(Choose { appid, name, query: q.clone(), results: None, manual: String::new() });
        self.search(appid, q);
    }
    fn search(&mut self, appid: i64, q: String) {
        if let Some(c) = &mut self.choose { c.results = None; }
        let (tx, ctx, h) = (self.tx.clone(), self.ctx.clone(), self.hltb.clone());
        std::thread::spawn(move || { let r = hltb_ops::search(&h, &q); let _ = tx.send(Msg::Results(appid, r)); ctx.request_repaint(); });
    }

    pub fn show(&mut self, ctx: &egui::Context, accent: egui::Color32, job_active: bool) -> HltbOut {
        while let Ok(m) = self.rx.try_recv() {
            match m {
                Msg::Loaded(r) => { self.loading = false; match r { Ok(o) => self.ov = o, Err(e) => self.status = e } }
                Msg::Results(id, r) => { if let Some(c) = self.choose.as_mut().filter(|c| c.appid == id) { c.results = Some(r); } }
                Msg::Op(r) => { self.busy = false; self.status = match r { Ok(s) => s, Err(e) => e }; self.choose = None; self.reload(); }
            }
        }
        if self.was_job && !job_active { self.reload(); }
        self.was_job = job_active;
        let mut out = HltbOut::None;
        let (mut confirm, mut clear, mut other, mut pick, mut select_manual): (Option<i64>, Option<i64>, Option<(i64, String)>, Option<(i64, i64, Option<i64>)>, Option<i64>) = (None, None, None, None, None);
        let (mut back, mut research) = (false, None::<String>);
        let res = ui_kit::modal(ctx, "hltb", "HLTB Data", 58.0, |ui, r| {
            ui_kit::style_fields(ui);
            let review: Vec<usize> = self.ov.matches.iter().enumerate().filter(|(_, m)| m.fetched == "unconfirmed").map(|(i, _)| i).collect();
            let done_rows: Vec<usize> = self.ov.matches.iter().enumerate().filter(|(_, m)| m.fetched != "unconfirmed").map(|(i, _)| i).collect();
            if let Some(ch) = self.choose.as_mut() {
                // choosing another match for one game
                ui.label(egui::RichText::new(format!("Choose the HowLongToBeat page for {}", ch.name)).color(ui_kit::TEXT()).strong().size(r));
                ui.horizontal(|ui| {
                    let resp = ui.add(egui::TextEdit::singleline(&mut ch.query).desired_width(24.0 * r));
                    if (ui_kit::button(ui, "SEARCH", Kind::Primary, accent, r).clicked() || (resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)))) && !ch.query.trim().is_empty() { research = Some(ch.query.trim().to_string()); }
                    if ui_kit::button(ui, "BACK", Kind::Plain, accent, r).clicked() { back = true; }
                });
                ui.add_space(0.3 * r);
                match &ch.results {
                    None => { ui.label(ui_kit::muted("Searching...", 0.85 * r)); }
                    Some(v) if v.is_empty() => { ui.label(ui_kit::muted("Nothing found. Try other words, or paste the page's address below.", 0.85 * r)); }
                    Some(v) => { egui::ScrollArea::vertical().max_height(15.0 * r).auto_shrink([false, true]).show(ui, |ui| for d in v {
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new(&d.matched_name).color(ui_kit::TEXT()).size(0.9 * r));
                            ui.label(ui_kit::muted(format!("{}%  {} / {} / {}", d.score, hours(d.main), hours(d.extras), hours(d.completionist)), 0.78 * r));
                            if ui_kit::button(ui, "USE THIS", Kind::Plain, accent, r).clicked() { pick = Some((ch.appid, d.id, Some(d.score))); }
                        });
                        ui.separator();
                    }); }
                }
                ui.add_space(0.4 * r);
                ui.label(ui_kit::muted("Or paste a HowLongToBeat address or game number:", 0.8 * r));
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(&mut ch.manual).desired_width(24.0 * r).hint_text("https://howlongtobeat.com/game/..."));
                    if ui_kit::button(ui, "USE", Kind::Plain, accent, r).clicked() { select_manual = Some(ch.appid); }
                });
                return;
            }
            ui.horizontal(|ui| {
                ui.label(ui_kit::muted("Confirm matches scoring at least", 0.82 * r));
                let mut t = self.threshold as f64;
                if crate::ui_kit::add_slider(ui, egui::Slider::new(&mut t, 0.0..=100.0).integer()).changed() { self.threshold = t as i64; }
                crate::ui_kit::slider_value(ui, format!("{}", t as i64), 0.9 * r);
            });
            let above: Vec<i64> = review.iter().map(|&i| &self.ov.matches[i]).filter(|m| m.score.unwrap_or(0) >= self.threshold).map(|m| m.appid).collect();
            let unmatched: Vec<i64> = self.ov.no_match.iter().map(|(a, _)| *a).collect();
            ui.horizontal(|ui| {
                if ui_kit::button(ui, &format!("CONFIRM ALL ABOVE ({})", above.len()), Kind::Primary, accent, r).clicked() && !above.is_empty() && !job_active { out = HltbOut::ConfirmAll(above.clone()); }
                if ui_kit::button(ui, &format!("RE-SCRAPE UNMATCHED ({})", unmatched.len()), Kind::Plain, accent, r).clicked() && !unmatched.is_empty() && !job_active { out = HltbOut::Rescrape(unmatched.clone()); }
                ui.add(egui::TextEdit::singleline(&mut self.filter).hint_text("Filter by name").desired_width(14.0 * r));
            });
            let line = if self.loading { "Loading...".to_string() } else if !self.status.is_empty() { self.status.clone() } else { format!("{} to review, {} confirmed, {} with no match, {} not looked up yet", review.len(), done_rows.len(), self.ov.no_match.len(), self.ov.unfetched) };
            ui.label(ui_kit::muted(line, 0.8 * r));
            ui.add_space(0.3 * r);
            let f = self.filter.to_lowercase();
            let hit = |n: &str| f.is_empty() || n.to_lowercase().contains(&f);
            // one flat list so the scroll area only lays out what is on screen (a library can hold thousands of confirmed matches)
            enum L { Head(usize, String), Row(usize), No(usize) }
            let mut lines: Vec<L> = vec![];
            lines.push(L::Head(0, format!("Needs review ({})", review.len()))); if self.open[0] { lines.extend(review.iter().filter(|&&i| hit(&self.ov.matches[i].name)).map(|&i| L::Row(i))); }
            lines.push(L::Head(1, format!("Confirmed ({})", done_rows.len()))); if self.open[1] { lines.extend(done_rows.iter().filter(|&&i| hit(&self.ov.matches[i].name)).map(|&i| L::Row(i))); }
            lines.push(L::Head(2, format!("No match found ({})", self.ov.no_match.len()))); if self.open[2] { lines.extend(self.ov.no_match.iter().enumerate().filter(|(_, (_, n))| hit(n)).map(|(i, _)| L::No(i))); }
            let rh = 1.7 * r;
            egui::ScrollArea::vertical().max_height(f32::INFINITY).auto_shrink([false, true]).show_rows(ui, rh, lines.len(), |ui, range| {
                for k in range {
                    match &lines[k] {
                        L::Head(i, t) => { let arrow = if self.open[*i] { "\u{25BE}" } else { "\u{25B8}" }; if ui.add(egui::Label::new(egui::RichText::new(format!("{arrow} {t}")).strong().color(crate::ui_kit::heading()).size(0.9 * r)).sense(egui::Sense::click())).clicked() { self.open[*i] = !self.open[*i]; } }
                        L::Row(i) => { let m = &self.ov.matches[*i]; ui.horizontal(|ui| {
                            ui.set_height(rh);
                            ui.label(egui::RichText::new(&m.name).color(ui_kit::TEXT()).size(0.88 * r));
                            if m.matched_name != m.name { ui.label(ui_kit::muted(format!("\u{2192} {}", m.matched_name), 0.78 * r)); }
                            if let Some(s) = m.score { ui.label(ui_kit::muted(format!("{s}%"), 0.78 * r)); }
                            if m.fetched != "unconfirmed" { ui.label(ui_kit::muted(format!("{} / {} / {}", hours(m.main), hours(m.extras), hours(m.completionist)), 0.78 * r)); }
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui_kit::button(ui, "NO PAGE", Kind::Plain, accent, r * 0.85).clicked() { clear = Some(m.appid); }
                                if ui_kit::button(ui, "OTHER MATCH", Kind::Plain, accent, r * 0.85).clicked() { other = Some((m.appid, m.name.clone())); }
                                if m.fetched == "unconfirmed" && ui_kit::button(ui, "CONFIRM", Kind::Primary, accent, r * 0.85).clicked() { confirm = Some(m.appid); }
                            });
                        }); }
                        L::No(i) => { let (id, n) = &self.ov.no_match[*i]; ui.horizontal(|ui| {
                            ui.set_height(rh);
                            ui.label(egui::RichText::new(n).color(ui_kit::TEXT()).size(0.88 * r));
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| { if ui_kit::button(ui, "SEARCH", Kind::Plain, accent, r * 0.85).clicked() { other = Some((*id, n.clone())); } });
                        }); }
                    }
                }
            });
        });
        if res.closed { return HltbOut::Close; }
        if back { self.choose = None; }
        if let Some(q) = research { if let Some(a) = self.choose.as_ref().map(|c| c.appid) { self.search(a, q); } }
        let day = today();
        if !self.busy {
            if let Some(id) = confirm { let d = day.clone(); self.op("Confirming...", move |c, h| hltb_ops::confirm(c, h, id, &d).map(|r| match r { hltb_ops::Confirmed::Saved => "Confirmed".to_string(), hltb_ops::Confirmed::Cleared => "HowLongToBeat has no time data for that entry. Moved to no match.".into() })); }
            if let Some(id) = clear { self.op("Clearing...", move |c, _| hltb_ops::clear(c, id).map(|_| "Marked as having no HowLongToBeat page".to_string())); }
            if let Some((id, name)) = other { self.open_choose(id, name); }
            if let Some((id, hid, score)) = pick { let d = day.clone(); self.op("Saving...", move |c, h| hltb_ops::select(c, h, id, hid, score, &d).map(|_| "Saved".to_string())); }
            if let Some(id) = select_manual {
                let text = self.choose.as_ref().map(|c| c.manual.clone()).unwrap_or_default();
                match hltb_ops::parse_hltb_id(&text) { Some(hid) => { let d = day.clone(); self.op("Saving...", move |c, h| hltb_ops::select(c, h, id, hid, None, &d).map(|_| "Saved".to_string())); } None => self.status = "That is not a HowLongToBeat address or number.".into() }
            }
        }
        out
    }
}
