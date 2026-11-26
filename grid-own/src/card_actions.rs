//! Mouse actions on game cards: the click rules (state.json `card_click_rules`) and the engine that turns presses into actions.
//! A port of static/js/card_actions.js, independent of any page so Home, Library and Pick 6 share it.
//! A button with a double-click rule holds its single click back for DBL; a second press on the same card inside that window is the double.
use serde_json::Value;
use std::collections::HashMap;
use std::time::{Duration, Instant};

pub const DBL: Duration = Duration::from_millis(300);
pub const ACTIONS: &[&str] = &["launch", "tooltip", "context_menu", "store", "open_folder", "edit", "achievements", "community_hub", "none"];

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Button { Left, Middle, Right, Back, Forward }
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Click { Single, Double }
#[derive(Clone, PartialEq, Debug)]
pub struct Rule { pub button: Button, pub click: Click, pub action: String }

impl Button {
    pub fn from_name(n: &str) -> Option<Button> { Some(match n { "left" => Button::Left, "middle" => Button::Middle, "right" => Button::Right, "back" => Button::Back, "forward" => Button::Forward, _ => return None }) }
}

/// `config.DEFAULT_CARD_CLICK_RULES`
pub fn default_rules() -> Vec<Rule> {
    vec![Rule { button: Button::Left, click: Click::Single, action: "launch".into() }, Rule { button: Button::Right, click: Click::Single, action: "context_menu".into() }]
}

/// `config.normalize_card_click_rules`: valid rules only, in order, at most one per (button, click). A state without the key gets the defaults.
pub fn rules_from_state(state: &Value) -> Vec<Rule> {
    let Some(list) = state.get("card_click_rules").and_then(|v| v.as_array()) else { return default_rules() };
    let mut out: Vec<Rule> = Vec::new();
    for r in list {
        let (Some(b), Some(c), Some(a)) = (r.get("button").and_then(|v| v.as_str()).and_then(Button::from_name),
            r.get("click").and_then(|v| v.as_str()).and_then(|c| match c { "single" => Some(Click::Single), "double" => Some(Click::Double), _ => None }),
            r.get("action").and_then(|v| v.as_str()).filter(|a| ACTIONS.contains(a))) else { continue };
        if !out.iter().any(|x| x.button == b && x.click == c) { out.push(Rule { button: b, click: c, action: a.to_string() }); }
    }
    out
}

pub const BUTTONS: [Button; 5] = [Button::Left, Button::Middle, Button::Right, Button::Back, Button::Forward];
impl Button {
    pub fn name(self) -> &'static str { match self { Button::Left => "left", Button::Middle => "middle", Button::Right => "right", Button::Back => "back", Button::Forward => "forward" } }
    /// PlayDate's `_CARD_BTN_LABELS`.
    pub fn label(self) -> &'static str { match self { Button::Left => "Left button", Button::Middle => "Middle button", Button::Right => "Right button", Button::Back => "Back side button", Button::Forward => "Forward side button" } }
}
impl Click {
    pub fn name(self) -> &'static str { match self { Click::Single => "single", Click::Double => "double" } }
    pub fn label(self) -> &'static str { match self { Click::Single => "single click", Click::Double => "double click" } }
}
/// PlayDate's `_CARD_ACTION_LABELS`.
pub fn action_label(a: &str) -> &'static str {
    match a { "launch" => "Launch / install", "tooltip" => "Show info tooltip", "context_menu" => "Show context menu", "store" => "Open store page", "open_folder" => "Open install folder", "edit" => "Edit game",
        "achievements" => "Open Steam achievements", "community_hub" => "Open Steam Community Hub", "none" => "Do nothing", _ => "?" }
}
/// The rules as `state.json` keeps them: a list of `{button, click, action}`.
pub fn rules_json(rules: &[Rule]) -> Value { Value::Array(rules.iter().map(|r| serde_json::json!({"button": r.button.name(), "click": r.click.name(), "action": r.action})).collect()) }

/// An action to run on a card.
#[derive(Clone, Debug, PartialEq)]
pub struct Fired { pub action: String, pub appid: i64, pub pos: (f32, f32) }

struct Pending { id: u64, button: Button, appid: i64, due: Instant, action: String, pos: (f32, f32) }

pub struct Engine {
    rules: Vec<Rule>,
    last: HashMap<Button, (u64, i64, Instant)>,   // button -> (record id, card, time) of the previous press
    pending: Vec<Pending>,
    next_id: u64,
}

impl Engine {
    /// New rules (Settings -> Mouse): a click already waiting for its double-click window is dropped.
    pub fn set_rules(&mut self, rules: Vec<Rule>) { self.rules = rules; self.pending.clear(); self.last.clear(); }
    pub fn new(rules: Vec<Rule>) -> Engine { Engine { rules, last: HashMap::new(), pending: Vec::new(), next_id: 1 } }
    fn action(&self, b: Button, c: Click) -> Option<&str> { self.rules.iter().find(|r| r.button == b && r.click == c).map(|r| r.action.as_str()) }

    /// A completed click (press and release on the same card).
    pub fn click(&mut self, button: Button, appid: i64, pos: (f32, f32), now: Instant) -> Vec<Fired> {
        let (dbl, sgl) = (self.action(button, Click::Double).map(str::to_string), self.action(button, Click::Single).map(str::to_string));
        let fire = |a: &str| Fired { action: a.to_string(), appid, pos };
        if let (Some(d), Some((id, prev_app, t))) = (&dbl, self.last.get(&button).copied()) {
            if prev_app == appid && now.duration_since(t) < DBL {
                self.pending.retain(|p| p.id != id);
                self.last.remove(&button);
                return vec![fire(d)];
            }
        }
        if dbl.is_none() { return sgl.map(|s| vec![fire(&s)]).unwrap_or_default(); }
        // a double click exists for this button: hold the single back until it is clear no second press is coming
        let id = self.next_id; self.next_id += 1;
        if let Some(s) = sgl { self.pending.push(Pending { id, button, appid, due: now + DBL, action: s, pos }); }
        self.last.insert(button, (id, appid, now));
        vec![]
    }

    /// Singles whose wait for a second press has run out. An earlier card's pending single still fires on its own timer.
    pub fn tick(&mut self, now: Instant) -> Vec<Fired> {
        let mut out = Vec::new();
        let (due, rest): (Vec<Pending>, Vec<Pending>) = std::mem::take(&mut self.pending).into_iter().partition(|p| p.due <= now);
        self.pending = rest;
        for p in due {
            if self.last.get(&p.button).map_or(false, |l| l.0 == p.id) { self.last.remove(&p.button); }
            out.push(Fired { action: p.action, appid: p.appid, pos: p.pos });
        }
        out
    }
    pub fn deadline(&self) -> Option<Instant> { self.pending.iter().map(|p| p.due).min() }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn rules(list: &[(&str, &str, &str)]) -> Vec<Rule> { list.iter().map(|(b, c, a)| Rule { button: Button::from_name(b).unwrap(), click: if *c == "single" { Click::Single } else { Click::Double }, action: a.to_string() }).collect() }
    #[test]
    fn defaults_fire_at_once() {
        let mut e = Engine::new(default_rules());
        let t = Instant::now();
        assert_eq!(e.click(Button::Left, 7, (1.0, 2.0), t), vec![Fired { action: "launch".into(), appid: 7, pos: (1.0, 2.0) }]);
        assert_eq!(e.click(Button::Right, 7, (0.0, 0.0), t)[0].action, "context_menu");
        assert!(e.click(Button::Middle, 7, (0.0, 0.0), t).is_empty(), "no rule, nothing happens");
        assert!(e.deadline().is_none());
    }
    #[test]
    fn single_waits_when_a_double_rule_exists() {
        let mut e = Engine::new(rules(&[("left", "single", "context_menu"), ("left", "double", "launch")]));
        let t = Instant::now();
        assert!(e.click(Button::Left, 1, (0.0, 0.0), t).is_empty(), "held back");
        assert!(e.tick(t + Duration::from_millis(299)).is_empty());
        let fired = e.tick(t + Duration::from_millis(300));
        assert_eq!(fired.len(), 1);
        assert_eq!(fired[0].action, "context_menu");
        assert!(e.deadline().is_none());
    }
    #[test]
    fn second_press_on_the_same_card_is_the_double() {
        let mut e = Engine::new(rules(&[("left", "single", "context_menu"), ("left", "double", "launch")]));
        let t = Instant::now();
        e.click(Button::Left, 1, (0.0, 0.0), t);
        let fired = e.click(Button::Left, 1, (0.0, 0.0), t + Duration::from_millis(120));
        assert_eq!(fired.iter().map(|f| f.action.as_str()).collect::<Vec<_>>(), vec!["launch"]);
        assert!(e.tick(t + Duration::from_secs(1)).is_empty(), "the held single must not fire after the double");
    }
    #[test]
    fn another_card_or_a_late_press_is_not_a_double() {
        let mut e = Engine::new(rules(&[("left", "single", "store"), ("left", "double", "launch")]));
        let t = Instant::now();
        e.click(Button::Left, 1, (0.0, 0.0), t);
        assert!(e.click(Button::Left, 2, (0.0, 0.0), t + Duration::from_millis(50)).is_empty());
        let mut got: Vec<(String, i64)> = e.tick(t + Duration::from_millis(400)).into_iter().map(|f| (f.action, f.appid)).collect();
        got.sort();
        assert_eq!(got, vec![("store".to_string(), 1), ("store".to_string(), 2)], "each card's own single still fires");
        // a second press after the window is a new first press (held again), not a double
        let t2 = t + Duration::from_secs(2);
        e.click(Button::Left, 3, (0.0, 0.0), t2);
        assert!(e.click(Button::Left, 3, (0.0, 0.0), t2 + Duration::from_millis(400)).is_empty());
    }
    #[test]
    fn double_only_rule_does_nothing_on_a_single_click() {
        let mut e = Engine::new(rules(&[("right", "double", "edit")]));
        let t = Instant::now();
        assert!(e.click(Button::Right, 1, (0.0, 0.0), t).is_empty());
        assert!(e.tick(t + Duration::from_secs(1)).is_empty());
        assert_eq!(e.click(Button::Right, 1, (0.0, 0.0), t + Duration::from_millis(100))[0].action, "edit");
    }
    #[test]
    fn rules_are_normalized_like_the_python() {
        let s: Value = serde_json::json!({"card_click_rules": [
            {"button": "left", "click": "single", "action": "store"}, {"button": "left", "click": "single", "action": "launch"},
            {"button": "bogus", "click": "single", "action": "launch"}, {"button": "back", "click": "double", "action": "nope"}, {"button": "forward", "click": "double", "action": "edit"}, 7]});
        assert_eq!(rules_from_state(&s), rules(&[("left", "single", "store"), ("forward", "double", "edit")]));
        assert_eq!(rules_from_state(&serde_json::json!({})), default_rules());
        assert!(rules_from_state(&serde_json::json!({"card_click_rules": []})).is_empty(), "an empty list means no rules at all");
    }
    #[test] fn rules_round_trip_through_their_saved_form_and_the_engine_takes_new_ones() {
        let r = rules(&[("left", "double", "launch"), ("back", "single", "edit"), ("middle", "single", "none")]);
        assert_eq!(rules_from_state(&serde_json::json!({"card_click_rules": rules_json(&r)})), r);
        for b in BUTTONS { assert_eq!(Button::from_name(b.name()), Some(b)); }
        assert!(ACTIONS.iter().all(|a| action_label(a) != "?"));
        let t0 = Instant::now();
        let mut e = Engine::new(default_rules());
        assert_eq!(e.click(Button::Left, 1, (0.0, 0.0), t0).iter().map(|f| f.action.as_str()).collect::<Vec<_>>(), vec!["launch"]);
        e.set_rules(rules(&[("left", "single", "edit")]));
        assert_eq!(e.click(Button::Left, 1, (0.0, 0.0), t0 + Duration::from_secs(5)).iter().map(|f| f.action.as_str()).collect::<Vec<_>>(), vec!["edit"]);
        assert!(e.click(Button::Right, 1, (0.0, 0.0), t0 + Duration::from_secs(9)).is_empty(), "a button with no rule does nothing");
    }
}
