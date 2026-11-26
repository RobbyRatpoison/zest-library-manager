//! Menu -> Tutorial: a list of sections with a search box, and a step view with Back and Next (the words are in `tutorial_text.rs`). Links in a step open a dialog or jump to
//! another step. The search shows only sections that hold every typed word and opens one at its first matching step.
use crate::tutorial_text::{Section, Step, OPEN_KEYS, SECTIONS};
use crate::ui_kit::{self, Kind};
use egui::{RichText, Ui};

pub enum TutOut { None, Close, /// Close the tutorial and open this dialog (a menu key).
    Open(&'static str) }

pub struct TutorialDialog { at: Option<(usize, usize)>, query: String, jump: Option<(usize, usize)> }

/// One piece of a paragraph: plain text, bold text or a link.
#[derive(Debug, PartialEq)]
pub enum Piece { Text(String), Bold(String), Open(String, String), Go(String, String) }

/// A line of text cut into pieces at `**bold**` and `[[kind:target|label]]` (a link's label may itself be `**bold**`).
pub fn pieces(line: &str) -> Vec<Piece> {
    let mut out = vec![]; let mut rest = line;
    let flush = |buf: &mut String, out: &mut Vec<Piece>| if !buf.is_empty() { out.push(Piece::Text(std::mem::take(buf))); };
    let mut buf = String::new();
    while !rest.is_empty() {
        if let Some(r) = rest.strip_prefix("[[") {
            if let Some(end) = r.find("]]") {
                let inner = &r[..end]; let (spec, label) = inner.split_once('|').unwrap_or((inner, inner));
                let label = label.trim_matches('*').to_string();
                if let Some((k, t)) = spec.split_once(':') { flush(&mut buf, &mut out); out.push(if k == "open" { Piece::Open(t.to_string(), label) } else { Piece::Go(t.to_string(), label) }); rest = &r[end + 2..]; continue; }
            }
        }
        if let Some(r) = rest.strip_prefix("**") { if let Some(end) = r.find("**") { flush(&mut buf, &mut out); out.push(Piece::Bold(r[..end].to_string())); rest = &r[end + 2..]; continue; } }
        let c = rest.chars().next().unwrap(); buf.push(c); rest = &rest[c.len_utf8()..];
    }
    flush(&mut buf, &mut out); out
}

/// The text a search looks through for a step.
fn plain(s: &Step) -> String { let mut t = String::new(); for p in pieces(&format!("{} {}", s.title, s.body.replace('\n', " "))) { match p { Piece::Text(x) | Piece::Bold(x) | Piece::Open(_, x) | Piece::Go(_, x) => { t.push_str(&x); } } } t.to_lowercase() }
/// Does the section hold every word, and at which step does the first of them appear?
pub fn search(sec: &Section, query: &str) -> Option<(usize, usize)> {
    let words: Vec<String> = query.to_lowercase().split_whitespace().map(String::from).collect();
    if words.is_empty() { return Some((0, 0)); }
    let texts: Vec<String> = sec.steps.iter().map(plain).collect();
    let all = format!("{} {}", sec.title.to_lowercase(), texts.join(" "));
    if !words.iter().all(|w| all.contains(w)) { return None; }
    // the step holding the most of the words (the earliest on a tie), so "saved filters" lands on the Saved filters step and not on any step that says "filters"
    let score = |t: &String| words.iter().filter(|w| t.contains(w.as_str())).count();
    let first = texts.iter().enumerate().fold((0usize, 0usize), |best, (i, t)| if score(t) > best.1 { (i, score(t)) } else { best }).0;
    Some((first, texts.iter().filter(|t| words.iter().any(|w| t.contains(w))).count()))
}

fn find_step(section: &str, step: Option<&str>) -> Option<(usize, usize)> {
    let si = SECTIONS.iter().position(|s| s.id == section)?;
    Some((si, step.and_then(|t| SECTIONS[si].steps.iter().position(|x| x.title == t)).unwrap_or(0)))
}

impl TutorialDialog {
    /// `ZEST_TUT_AT=<section>,<step>` opens at a step (for screenshots).
    pub fn new() -> Self { let at = std::env::var("ZEST_TUT_AT").ok().and_then(|v| { let (a, b) = v.split_once(',')?; let (a, b): (usize, usize) = (a.parse().ok()?, b.parse().ok()?); SECTIONS.get(a)?.steps.get(b)?; Some((a, b)) }); TutorialDialog { at, query: String::new(), jump: None } }

    fn paragraph(&mut self, ui: &mut Ui, line: &str, r: f32, open: &mut Option<&'static str>) {
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            for p in pieces(line) {
                match p {
                    Piece::Text(t) => { ui.label(RichText::new(t).size(0.92 * r).color(ui_kit::TEXT())); }
                    Piece::Bold(t) => { ui.label(RichText::new(t).size(0.92 * r).strong().color(crate::ui_kit::heading())); }
                    Piece::Open(k, label) => { if ui.link(RichText::new(label).size(0.92 * r)).clicked() { *open = OPEN_KEYS.iter().copied().find(|x| *x == k); } }
                    Piece::Go(t, label) => { if ui.link(RichText::new(label).size(0.92 * r)).clicked() { let (sec, step) = t.split_once(':').map_or((t.as_str(), None), |(a, b)| (a, Some(b))); self.jump = find_step(sec, step); } }
                }
            }
        });
    }

    pub fn show(&mut self, ctx: &egui::Context, accent: egui::Color32) -> TutOut {
        let mut out = TutOut::None; let mut open = None::<&'static str>;
        let (mut back_to_list, mut prev, mut next) = (false, false, false);
        let mut pick: Option<(usize, usize)> = None;
        let at = self.at;
        let res = ui_kit::modal(ctx, "tutorial", "Tutorial", 46.0, |ui, r| {
            match at {
                None => {
                    ui_kit::style_fields(ui);
                    ui.add(egui::TextEdit::singleline(&mut self.query).hint_text("Search the tutorial").desired_width(f32::INFINITY));
                    ui.add_space(0.3 * r);
                    let mut shown = 0;
                    egui::ScrollArea::vertical().max_height(f32::INFINITY).auto_shrink([false, true]).show(ui, |ui| {
                        for (i, sec) in SECTIONS.iter().enumerate() {
                            let Some((first, n)) = search(sec, &self.query) else { continue };
                            shown += 1;
                            ui.horizontal(|ui| {
                                let rs = ui.add(egui::Label::new(RichText::new(sec.title).size(1.0 * r).strong().color(crate::ui_kit::heading())).sense(egui::Sense::click())); ui_kit::focus_mark(ui, &rs, rs.rect); if rs.clicked() { pick = Some((i, first)); }
                                let note = if self.query.trim().is_empty() { format!("{} steps", sec.steps.len()) } else { format!("{n} match{}", if n == 1 { "" } else { "es" }) };
                                ui.label(ui_kit::muted(note, 0.8 * r));
                            });
                            ui.add_space(0.15 * r);
                        }
                        if shown == 0 { ui.label(ui_kit::muted("Nothing in the tutorial has all those words.", 0.9 * r)); }
                    });
                }
                Some((si, ti)) => {
                    let sec = &SECTIONS[si]; let step = &sec.steps[ti];
                    ui.label(ui_kit::muted(format!("{}  -  step {} of {}", sec.title, ti + 1, sec.steps.len()), 0.8 * r));
                    ui.label(RichText::new(step.title).size(1.25 * r).strong().color(crate::ui_kit::heading()));
                    ui.add_space(0.4 * r);
                    egui::ScrollArea::vertical().max_height(f32::INFINITY).auto_shrink([false, true]).show(ui, |ui| {
                        for para in step.body.split("\n\n") {
                            for line in para.split('\n') {
                                if let Some(b) = line.strip_prefix("- ") { ui.horizontal_wrapped(|ui| { ui.label(RichText::new("\u{2022} ").size(0.92 * r).color(ui_kit::MUTED())); self.paragraph(ui, b, r, &mut open); }); }
                                else { self.paragraph(ui, line, r, &mut open); }
                            }
                            ui.add_space(0.5 * r);
                        }
                    });
                    ui.add_space(0.3 * r);
                    ui.horizontal(|ui| {
                        if ui_kit::button(ui, "CONTENTS", Kind::Plain, accent, r).clicked() { back_to_list = true; }
                        if ti > 0 && ui_kit::button(ui, "BACK", Kind::Plain, accent, r).clicked() { prev = true; }
                        let last = ti + 1 == sec.steps.len();
                        if ui_kit::button(ui, if last { if si + 1 < SECTIONS.len() { "NEXT SECTION" } else { "DONE" } } else { "NEXT" }, Kind::Primary, accent, r).clicked() { next = true; }
                    });
                }
            }
        });
        if let Some(p) = pick { self.at = Some(p); }
        if let Some(j) = self.jump.take() { self.at = Some(j); }
        if back_to_list { self.at = None; }
        if let Some((si, ti)) = self.at {
            if prev { self.at = Some((si, ti - 1)); }
            if next { self.at = if ti + 1 < SECTIONS[si].steps.len() { Some((si, ti + 1)) } else if si + 1 < SECTIONS.len() { Some((si + 1, 0)) } else { None }; }
        }
        if let Some(k) = open { out = TutOut::Open(k); }
        if res.closed && matches!(out, TutOut::None) { return TutOut::Close; }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn text_is_cut_into_plain_bold_and_link_pieces() {
        assert_eq!(pieces("a **b** [[open:data|**Data**]] and [[go:home:Shuffle|there]]."), vec![Piece::Text("a ".into()), Piece::Bold("b".into()), Piece::Text(" ".into()), Piece::Open("data".into(), "Data".into()), Piece::Text(" and ".into()), Piece::Go("home:Shuffle".into(), "there".into()), Piece::Text(".".into())]);
        assert_eq!(pieces("no markup, 2 * 3 ** unbalanced"), vec![Piece::Text("no markup, 2 * 3 ** unbalanced".into())]);
    }
    #[test] fn search_needs_every_word_and_opens_at_the_first_step_that_has_one() {
        let lib = SECTIONS.iter().find(|s| s.id == "library").unwrap();
        assert_eq!(search(lib, ""), Some((0, 0)));
        let (first, n) = search(lib, "saved filters").unwrap(); assert_eq!(lib.steps[first].title, "Saved filters"); assert!(n >= 1);
        assert!(search(lib, "zzzznotthere").is_none());
        assert!(search(lib, "saved zzzznotthere").is_none(), "every word must be somewhere in the section");
        // every section can be reached by some search of its own title's first word
        for s in SECTIONS { assert!(search(s, s.title.split_whitespace().next().unwrap()).is_some(), "{}", s.id); }
        assert_eq!(find_step("home", Some("Shuffle")), Some((1, 1))); assert_eq!(find_step("nope", None), None);
    }
}
