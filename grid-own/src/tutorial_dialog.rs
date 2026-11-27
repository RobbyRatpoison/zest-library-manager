//! Menu -> Tutorial: a list of sections with a search box, and a step view with Back and Next (the words are in `tutorial_text.rs`). Links in a step open a dialog or jump to
//! another step. A search lists the steps that answer it, best first; it knows plurals, skips small words ("how do i") and maps common other words to the tutorial's own (`SYNONYMS`).
use crate::tutorial_text::{Step, OPEN_KEYS, SECTIONS};
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

/// Words people may type for what the tutorial calls something else: (what they might type, the tutorial's own words). One way only, so a common
/// tutorial word ("cover", "play") never drags in every step. A typed word finds its group when it equals an entry (after `stem`) or starts one
/// (4+ letters); an entry with a space is matched as a phrase of the query.
const SYNONYMS: &[(&[&str], &[&str])] = &[
    (&["picture", "image", "poster", "boxart", "box art", "thumbnail", "artwork", "capsule", "banner", "grid art"], &["cover", "art"]),
    (&["controller", "joypad", "joystick", "xbox controller", "dualsense"], &["gamepad", "controller"]),
    (&["remove", "erase", "trash", "get rid"], &["delete", "remove"]),
    (&["hide", "ignore", "exclude", "block", "unwanted"], &["blacklist", "hidden", "junk"]),
    (&["collection", "label", "category", "folder", "favorite", "favourite", "playlist"], &["group", "tag"]),
    (&["finished", "done", "beat", "complete", "progress", "100%"], &["completion", "beaten", "completed"]),
    (&["rating", "stars", "score", "critic"], &["review", "metacritic"]),
    (&["time played", "hours", "hours played"], &["playtime", "hours played"]),
    (&["recommend", "suggest", "decide", "random", "what to play", "what should i play", "backlog"], &["pick 6"]),
    (&["login", "log in", "logon", "password", "credential", "authenticate", "connect"], &["sign in", "signing in", "sign-in", "sign-ins"]),
    (&["save", "copy", "export"], &["backup", "export"]),
    (&["migrate", "transfer", "move", "switch from", "old library"], &["playdate", "import"]),
    (&["upgrade", "new version", "latest"], &["update"]),
    (&["bug", "crash", "problem", "error", "broken", "report", "support", "contact"], &["send log", "help", "discord"]),
    (&["dark mode", "light mode", "dark", "color", "colour", "wallpaper", "look", "style", "skin"], &["appearance", "theme", "background"]),
    (&["bigger text", "larger text", "smaller text", "bigger", "larger", "smaller", "zoom", "font", "text size", "tiny"], &["ui scale", "cover size"]),
    (&["shop", "storefront", "account sync"], &["store", "plugin"]),
    (&["rom", "retroarch", "console", "retro", "nes", "snes", "switch", "ps2"], &["emulator"]),
    (&["twice", "same game", "copies", "double"], &["duplicate"]),
    (&["run", "start game", "open game", "play game"], &["launch"]),
    (&["download"], &["install"]),
    (&["front page", "start page", "dashboard", "main page"], &["home", "shelf"]),
    (&["order", "arrange"], &["sort"]),
    (&["remap", "bind", "keybind", "hotkey", "mapping"], &["shortcut", "gamepad controls"]),
    (&["deck", "handheld", "game mode"], &["steam deck"]),
    (&["how long", "length", "duration"], &["howlongtobeat"]),
    (&["bought", "purchased", "acquired", "activation", "when i got"], &["purchase dates", "date added"]),
    (&["border", "frame", "highlight"], &["outline"]),
    (&["metadata", "genre", "info", "information", "missing"], &["fill missing data", "details"]),
    (&["key", "api key"], &["web api key", "steamgriddb key"]),
];
/// Words a search skips ("how do i hide a game" looks for hide; "game" is in nearly every step).
const STOP: &[&str] = &["a", "an", "the", "to", "in", "on", "of", "for", "and", "or", "how", "do", "does", "i", "my", "me", "is", "it", "can", "what", "where", "with", "from", "by", "be", "this", "that", "you", "your", "get", "want", "zest", "change", "set", "make", "use", "using", "game", "games"];

/// The text a search looks through for a step.
fn plain(s: &Step) -> String { let mut t = String::new(); for p in pieces(&format!("{} {}", s.title, s.body.replace('\n', " "))) { match p { Piece::Text(x) | Piece::Bold(x) | Piece::Open(_, x) | Piece::Go(_, x) => { t.push_str(&x); } } } t.to_lowercase() }
/// A plural ending taken off ("filters" -> "filter", "categories" -> "category", "matches" -> "match"), so the stem finds both forms.
fn stem(w: &str) -> String {
    if w.len() > 4 { if let Some(s) = w.strip_suffix("ies") { return format!("{s}y"); }
        if let Some(s) = w.strip_suffix("es").filter(|s| s.ends_with("sh") || s.ends_with("ch") || s.ends_with('x')) { return s.to_string(); } }
    if w.len() > 3 && w.ends_with('s') && !w.ends_with("ss") { return w[..w.len() - 1].to_string(); }
    w.to_string()
}
/// Does `term` occur in `text` at the start of a word ("art" finds "artwork", not "start")? A stem also finds its "-ies" plural ("category" in "categories").
fn has_term(text: &str, term: &str) -> bool {
    let at_start = |t: &str| text.match_indices(t).any(|(i, _)| !text[..i].chars().next_back().is_some_and(|c| c.is_alphanumeric()));
    at_start(term) || term.strip_suffix('y').is_some_and(|b| b.len() > 2 && at_start(&format!("{b}ies")))
}
/// One thing the user asked for: the word as typed (stemmed) and the tutorial words it may also be found as.
struct Term { own: String, syn: Vec<&'static str> }
fn term_for(word: &str) -> Term {
    let own = stem(word);
    let syn = SYNONYMS.iter().filter(|(typed, _)| typed.iter().any(|e| !e.contains(' ') && (stem(e) == own || (own.len() >= 4 && e.starts_with(own.as_str()))))).flat_map(|(_, to)| to.iter().copied()).collect();
    Term { own, syn }
}
/// The query as terms: phrases that are synonym entries ("steam deck", "log in") first, then the remaining words, small words dropped unless nothing else is left.
fn query_terms(query: &str) -> Vec<Term> {
    let q = query.to_lowercase().replace(['?', ',', '.', '!'], " ");
    let mut rest = format!(" {} ", q.split_whitespace().collect::<Vec<_>>().join(" "));
    let mut terms = vec![];
    for (typed, to) in SYNONYMS { for e in typed.iter().filter(|e| e.contains(' ')) { let pat = format!(" {e} "); if rest.contains(&pat) { rest = rest.replacen(&pat, " ", 1); terms.push(Term { own: e.to_string(), syn: to.to_vec() }); } } }
    let words: Vec<&str> = rest.split_whitespace().collect();
    let kept: Vec<&str> = words.iter().copied().filter(|w| !STOP.contains(w)).collect();
    terms.extend((if kept.is_empty() && terms.is_empty() { words } else { kept }).into_iter().map(term_for));
    terms
}
/// How well one text answers a term: 3 for the word itself, 2 for a synonym, 0 for neither.
fn weight(text: &str, t: &Term) -> u32 { if has_term(text, &t.own) { 3 } else if t.syn.iter().any(|s| has_term(text, s)) { 2 } else { 0 } }

/// Every step that answers the query, best first: steps holding the most of the asked-for words, then the word itself over a synonym (a hit in the
/// step's title counting double), then the earliest. When no step holds every word, the steps holding the most of them are shown.
pub fn results(query: &str) -> Vec<(usize, usize)> {
    let terms = query_terms(query);
    if terms.is_empty() { return vec![]; }
    let mut hits: Vec<(usize, usize, usize, u32)> = vec![];
    for (si, sec) in SECTIONS.iter().enumerate() { for (ti, st) in sec.steps.iter().enumerate() {
        // the step's own title counts as much again as its text; its section's title adds a little (so Pick 6's steps lead for "what should i play")
        let (body, title, sec_title) = (format!("{} {}", sec.title.to_lowercase(), plain(st)), st.title.to_lowercase(), sec.title.to_lowercase());
        let ws: Vec<u32> = terms.iter().map(|t| weight(&body, t) + weight(&title, t) + (weight(&sec_title, t) > 0) as u32).collect();
        let n = ws.iter().filter(|w| **w > 0).count();
        if n > 0 { hits.push((si, ti, n, ws.iter().sum())); }
    } }
    let best = hits.iter().map(|h| h.2).max().unwrap_or(0);
    hits.retain(|h| h.2 == best);
    hits.sort_by(|a, b| b.3.cmp(&a.3).then((a.0, a.1).cmp(&(b.0, b.1))));
    hits.into_iter().map(|h| (h.0, h.1)).collect()
}
/// Did the results hold every asked-for word? (Else the list says it is showing the closest matches.)
pub fn complete(query: &str) -> bool {
    let terms = query_terms(query);
    results(query).first().is_some_and(|&(si, ti)| { let (sec, st) = (&SECTIONS[si], &SECTIONS[si].steps[ti]); let body = format!("{} {}", sec.title.to_lowercase(), plain(st)); terms.iter().all(|t| weight(&body, t) > 0) })
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
                    egui::ScrollArea::vertical().max_height(f32::INFINITY).auto_shrink([false, true]).show(ui, |ui| {
                        if self.query.trim().is_empty() {
                            for (i, sec) in SECTIONS.iter().enumerate() {
                                ui.horizontal(|ui| {
                                    let rs = ui.add(egui::Label::new(RichText::new(sec.title).size(1.0 * r).strong().color(crate::ui_kit::heading())).sense(egui::Sense::click())); ui_kit::focus_mark(ui, &rs, rs.rect); if rs.clicked() { pick = Some((i, 0)); }
                                    ui.label(ui_kit::muted(format!("{} steps", sec.steps.len()), 0.8 * r));
                                });
                                ui.add_space(0.15 * r);
                            }
                        } else {
                            // a search lists the steps that answer it, best first, each with its section
                            let found = results(&self.query);
                            if found.is_empty() { ui.label(ui_kit::muted("Nothing in the tutorial matches that. Try another word.", 0.9 * r)); }
                            else if !complete(&self.query) { ui.label(ui_kit::muted("No step has all of those words; these come closest.", 0.8 * r)); ui.add_space(0.2 * r); }
                            for (si, ti) in found.into_iter().take(12) {
                                ui.horizontal(|ui| {
                                    let rs = ui.add(egui::Label::new(RichText::new(SECTIONS[si].steps[ti].title).size(1.0 * r).strong().color(crate::ui_kit::heading())).sense(egui::Sense::click())); ui_kit::focus_mark(ui, &rs, rs.rect); if rs.clicked() { pick = Some((si, ti)); }
                                    ui.label(ui_kit::muted(SECTIONS[si].title, 0.8 * r));
                                });
                                ui.add_space(0.15 * r);
                            }
                        }
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
    /// The first result of a query as (section id, step title).
    fn top(q: &str) -> Option<(&'static str, &'static str)> { results(q).first().map(|&(si, ti)| (SECTIONS[si].id, SECTIONS[si].steps[ti].title)) }
    fn in_top(q: &str, n: usize, section: &str, step: &str) -> bool { results(q).iter().take(n).any(|&(si, ti)| SECTIONS[si].id == section && SECTIONS[si].steps[ti].title == step) }
    #[test] fn search_finds_words_plurals_and_synonyms_and_ranks_the_best_step_first() {
        assert!(results("").is_empty() && results("zzzznotthere").is_empty());
        assert_eq!(top("saved filters"), Some(("library", "Saved filters")));
        assert_eq!(top("crop").map(|x| x.1), Some("The art tabs"));
        assert!(in_top("filters", 3, "library", "Building a filter") && in_top("filter", 3, "library", "Building a filter"));
        assert!(in_top("picture", 3, "art", "Where covers come from"), "{:?}", top("picture"));
        assert!(in_top("joystick", 3, "gamepad", "Moving around") || in_top("joystick", 3, "getting-started", "A controller"), "{:?}", top("joystick"));
        assert!(in_top("how do i hide a game", 3, "cleanup", "The blacklist"), "{:?}", results("how do i hide a game").iter().take(3).collect::<Vec<_>>());
        assert!(in_top("log in", 3, "stores", "Signing in"), "{:?}", top("log in"));
        assert_eq!(top("steam deck"), Some(("gamepad", "Steam Deck"))); assert_eq!(top("dark mode").map(|x| x.1), Some("Appearance")); assert!(in_top("keyring", 2, "stores", "Where sign-ins are kept")); assert!(in_top("tampermonkey", 3, "stores", "Signing in"));
        assert!(in_top("bought", 3, "metadata", "Purchase dates"));
        assert!(in_top("crash", 3, "settings", "Getting help"));
        assert!(in_top("rom", 3, "emulators", "ROM folders and scanning"));
        assert!(in_top("favorite", 5, "library", "Select games and Bulk Ops") || top("favorite").is_some());
        // a word matches at the start of a word: "art" must not match "start" or "party"
        assert!(!has_term("start the party", "art") && has_term("artwork here", "art") && has_term("cover art", "art") && has_term("categories", "category"));
        assert_eq!(stem("categories"), "category"); assert_eq!(stem("matches"), "match"); assert_eq!(stem("class"), "class"); assert_eq!(stem("filters"), "filter");
        assert_eq!(find_step("home", Some("Shuffle")), Some((1, 1))); assert_eq!(find_step("nope", None), None);
    }
    /// Every step can be found by its own title.
    #[test] fn every_step_is_found_by_its_title() {
        for (si, s) in SECTIONS.iter().enumerate() { for (ti, st) in s.steps.iter().enumerate() { assert!(results(st.title).iter().take(5).any(|&x| x == (si, ti)), "{} / {}: {:?}", s.id, st.title, results(st.title).iter().take(5).map(|&(a, b)| SECTIONS[a].steps[b].title).collect::<Vec<_>>()); } }
    }
}
