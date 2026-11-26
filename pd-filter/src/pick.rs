//! Port of Pick 6 (`pick.py`): the data query and the scoring/selection engine. Time and randomness are inputs
//! (`now`, an `rng` closure) so the engine is deterministic under test; golden cases feed Python the same fixed clock
//! and the same list of random numbers, and record how many it consumed.
use crate::{build_tree_sql, is_safe_sql, strip_sql_wrapper, truthy, Ctx};
use serde_json::Value;
use std::collections::HashMap;

pub const PIN_FALLBACK_STRENGTH: f64 = 0.25;

/// CPython's `sum()` over floats (compensated, Neumaier), which differs from a naive loop in the last bits.
pub fn py_sum(xs: impl IntoIterator<Item = f64>) -> f64 {
    let (mut f, mut c) = (0.0f64, 0.0f64);
    for x in xs {
        let t = f + x;
        if f.abs() >= x.abs() { c += (f - t) + x } else { c += (x - t) + f }
        f = t;
    }
    if c != 0.0 && c.is_finite() { f += c; }
    f
}

/// Python's `round()` (half to even) for non-negative-ish values.
fn py_round(x: f64) -> i64 { let r = x.round(); if (x - x.trunc()).abs() == 0.5 && (r as i64) % 2 != 0 { (r - x.signum()) as i64 } else { r as i64 } }

/// Python's `str.capitalize()`: first character upper-cased, the rest lower-cased.
fn capitalize(s: &str) -> String {
    let mut ch = s.chars();
    match ch.next() { Some(f) => f.to_uppercase().collect::<String>() + &ch.as_str().to_lowercase(), None => String::new() }
}

fn num(v: &Value) -> Option<f64> {
    match v { Value::Number(n) => n.as_f64(), Value::String(s) => s.trim().parse().ok(), Value::Bool(b) => Some(*b as i64 as f64), _ => None }
}
fn field<'a>(g: &'a Value, k: &str) -> &'a Value { g.get(k).unwrap_or(&Value::Null) }
fn tags_of(g: &Value) -> Vec<String> {
    field(g, "tags").as_str().unwrap_or("").split(',').map(|t| t.trim().to_string()).filter(|t| !t.is_empty()).collect()
}
fn state_f(state: &Value, k: &str, default: f64) -> f64 { state.get(k).and_then(num).unwrap_or(default) }

fn civil_from_days(z: i64) -> i64 {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let (y, doy) = (yoe + era * 400, doe - (365 * yoe + yoe / 4 - yoe / 100));
    let mp = (5 * doy + 2) / 153;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    y + if m <= 2 { 1 } else { 0 }
}
/// The UTC year of a timestamp, None where Python's `datetime.fromtimestamp` would raise.
pub fn year_of(ts: f64) -> Option<i64> {
    if !ts.is_finite() || ts < -62135596800.0 || ts > 253402300799.0 { return None; }
    Some(civil_from_days((ts / 86400.0).floor() as i64))
}

// ---------------------------------------------------------------- the data query

/// `pick._build_pick_where`: (where, params) for the active filter, hidden platforms and duplicate hiding.
pub fn build_pick_where(state: &Value, use_filtered: bool, supplements: &HashMap<String, Vec<i64>>, priority: Option<&[String]>) -> (String, Vec<Value>) {
    let mut ctx = Ctx { supplements: supplements.clone(), horizontal: state.get("artwork_orientation").and_then(|v| v.as_str()) == Some("horizontal"), ..Default::default() };
    if let Some(sf) = state.get("saved_filters").and_then(|v| v.as_object()) { ctx.saved_filters = sf.clone(); }
    let (mut params, mut where_) = (Vec::<Value>::new(), "1=1".to_string());
    if use_filtered {
        // resolve_active_filter_tree: a {saved_filter: name} sentinel points at a saved filter, a deleted one resolves to nothing
        let raw = state.get("filter_tree");
        let tree: Option<Value> = match raw {
            Some(Value::Object(o)) if o.contains_key("saved_filter") => {
                let name = o["saved_filter"].as_str().unwrap_or("");
                ctx.saved_filters.get(name).filter(|v| truthy(v)).map(|sf| match sf.as_object() { Some(o) if o.contains_key("tree") => o["tree"].clone(), _ => sf.clone() })
            }
            Some(v) => Some(v.clone()),
            None => None,
        };
        if let Some(tree) = tree.filter(truthy) {
            let custom = strip_sql_wrapper(tree.get("custom_sql").and_then(|v| v.as_str()).unwrap_or(""));
            if !custom.is_empty() { where_ = if is_safe_sql(&custom) { custom } else { "1=0".into() }; }
            else {
                let sql = build_tree_sql(&tree, &mut params, &ctx);
                if !sql.is_empty() && sql != "1=1" { where_ = sql; }
            }
        }
    }
    let hidden: Vec<&str> = state.get("hidden_platforms").and_then(|v| v.as_array()).map(|a| a.iter().filter_map(|p| p.as_str()).collect()).unwrap_or_default();
    if !hidden.is_empty() {
        let mut conds = Vec::new();
        if hidden.contains(&"steam") { conds.push("platform != 'steam'".to_string()); }
        let non_steam: Vec<&&str> = hidden.iter().filter(|p| **p != "steam").collect();
        if !non_steam.is_empty() {
            conds.push(format!("platform NOT IN ({})", non_steam.iter().map(|_| "?").collect::<Vec<_>>().join(",")));
            params.extend(non_steam.iter().map(|p| Value::String((**p).to_string())));
        }
        if !conds.is_empty() { where_ = format!("({where_}) AND ({})", conds.join(" AND ")); }
    }
    if state.get("hide_duplicates").map_or(true, truthy) {
        let (w, p) = crate::query::hide_duplicates_where(&where_, &params, priority);
        where_ = w; params = p;
    }
    (where_, params)
}

// ---------------------------------------------------------------- the engine

pub struct PickParams {
    pub mode: String,
    pub w_tags: f64, pub w_review: f64, pub w_staleness: f64, pub w_recency: f64, pub w_hltb: f64,
    pub b_review: Option<f64>, pub b_staleness: Option<f64>, pub b_recency: Option<f64>, pub b_hltb: Option<f64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Pick { pub appid: i64, pub reason: Option<String>, pub bounds_relaxed: bool }
#[derive(Debug, Clone, PartialEq)]
pub struct PickResult { pub picks: Vec<Pick>, pub pool_size: usize, pub bounded_pool_size: usize, pub bounds_relaxed: bool }

/// The tag-profile inputs: (tags, playtime) of the liked pool, tags of the whole Steam library, and whether the liked pool is the
/// most-played fallback (used for the wording of the reason).
pub struct Profile<'a> { pub liked: &'a [(Option<String>, Value)], pub library_tags: &'a [Option<String>], pub using_fallback: bool }

/// `database.apply_tag_overrides`
pub fn apply_tag_overrides(mut aff: HashMap<String, f64>, overrides: Option<&Value>) -> HashMap<String, f64> {
    let Some(ov) = overrides.and_then(|v| v.as_object()).filter(|o| !o.is_empty()) else { return aff };
    let rest: Vec<f64> = aff.iter().filter(|(t, _)| !ov.contains_key(*t)).map(|(_, a)| *a).collect();
    let pos = rest.iter().cloned().filter(|a| *a > 0.0).fold(None, |m: Option<f64>, a| Some(m.map_or(a, |x| x.max(a)))).unwrap_or(PIN_FALLBACK_STRENGTH);
    let neg = rest.iter().cloned().filter(|a| *a < 0.0).fold(None, |m: Option<f64>, a| Some(m.map_or(a, |x| x.min(a)))).unwrap_or(-PIN_FALLBACK_STRENGTH);
    for (tag, mode) in ov {
        match mode.as_str() { Some("like") => { aff.insert(tag.clone(), pos); } Some("dislike") => { aff.insert(tag.clone(), neg); } Some("ignore") => { aff.insert(tag.clone(), 0.0); } _ => {} }
    }
    aff
}

fn tag_pool_rate(rows: &[Vec<String>], weights: Option<&[f64]>) -> Vec<(String, f64)> {
    let n = match weights { None => rows.len() as f64, Some(w) => py_sum(w.iter().cloned()) };
    if n == 0.0 { return vec![]; }
    let (mut order, mut counts): (Vec<String>, HashMap<String, f64>) = (vec![], HashMap::new());
    for (i, tags) in rows.iter().enumerate() {
        let w = weights.map_or(1.0, |w| w[i]);
        if w <= 0.0 { continue; }
        for t in tags {
            let e = counts.entry(t.clone()).or_insert_with(|| { order.push(t.clone()); 0.0 });
            *e += w;
        }
    }
    order.into_iter().map(|t| { let c = counts[&t]; (t, c / n) }).collect()
}

fn split_tags(s: &Option<String>) -> Vec<String> { s.as_deref().unwrap_or("").split(',').map(|t| t.trim().to_string()).filter(|t| !t.is_empty()).collect() }

/// The score of one candidate for a (possibly relaxed) bounds setting: `None` = filtered out, else (final, similarity, matched tags).
struct Scorer<'a> {
    state: &'a Value, p: &'a PickParams, affinity: HashMap<String, f64>, k: f64,
    raw: HashMap<i64, Option<f64>>, raw_min: f64, raw_span: f64, now: f64, now_year: i64, b_review: Option<f64>,
}

impl<'a> Scorer<'a> {
    fn raw_tag_score(&self, g: &Value) -> Option<f64> {
        let tags = tags_of(g);
        if tags.is_empty() { return None; }
        let total = py_sum(tags.iter().map(|t| self.affinity.get(t).copied().unwrap_or(0.0)));
        Some(total / (tags.len() as f64 + self.k))
    }
    fn tag_similarity(&self, g: &Value) -> (f64, Vec<String>) {
        let tags = tags_of(g);
        if tags.is_empty() { return (0.0, vec![]); }
        let raw = self.raw.get(&field(g, "appid").as_i64().unwrap_or(0)).copied().flatten();
        let sim = match raw { Some(r) if self.raw_span != 0.0 => (r - self.raw_min) / self.raw_span, _ => 0.5 };
        let mut matched: Vec<String> = tags.into_iter().filter(|t| self.affinity.get(t).copied().unwrap_or(0.0) > 0.0).collect();
        matched.sort_by(|a, b| self.affinity[b].partial_cmp(&self.affinity[a]).unwrap_or(std::cmp::Ordering::Equal));
        (sim, matched)
    }
    fn review_score(g: &Value) -> Option<f64> {
        for k in ["weighted_percentage", "review_percentage"] {
            let v = field(g, k);
            if !v.is_null() && v.as_str() != Some("") { return num(v).map(|x| x / 100.0); }
        }
        None
    }
    fn staleness(&self, g: &Value) -> f64 {
        let (cap, curve) = (state_f(self.state, "pick6_staleness_cap_days", 730.0), state_f(self.state, "pick6_staleness_curve", 1.0));
        let lp = field(g, "last_played");
        if truthy(lp) {
            return match num(lp) { Some(lp) => (((self.now - lp) / 86400.0).min(cap) / cap).powf(curve), None => 0.5 };
        }
        1.0
    }
    fn hltb_times(g: &Value) -> Vec<f64> { ["hltb_main", "hltb_extras", "hltb_completionist"].iter().map(|k| field(g, k)).filter(|v| truthy(v)).filter_map(num).collect() }
    fn hltb_length(&self, g: &Value) -> Option<f64> {
        let times = Self::hltb_times(g);
        if times.is_empty() { return None; }
        let floor_h = state_f(self.state, "pick6_hltb_long_floor_hours", 10.0);
        let floor_min = floor_h * 60.0;
        let span_min = (state_f(self.state, "pick6_hltb_long_cap_hours", 110.0) - floor_h).max(1.0) * 60.0;
        let short_cap = state_f(self.state, "pick6_hltb_short_cap_hours", 10.0) * 60.0;
        if self.p.w_hltb >= 0.0 {
            let val = times.iter().cloned().fold(f64::MIN, f64::max);
            Some(((val - floor_min).min(span_min) / span_min).max(0.0).powf(state_f(self.state, "pick6_hltb_long_curve", 0.5)))
        } else {
            let val = times.iter().cloned().fold(f64::MAX, f64::min);
            Some((val.min(short_cap) / short_cap).powf(state_f(self.state, "pick6_hltb_short_curve", 0.5)))
        }
    }
    fn recency(&self, g: &Value) -> f64 {
        let rd = field(g, "release_date");
        if !truthy(rd) { return 0.5; }
        let cap = state_f(self.state, "pick6_recency_cap_years", 10.0);
        match num(rd).and_then(year_of) {
            Some(y) => { let age = (self.now_year - y).max(0) as f64; (1.0 - age.min(cap) / cap).powf(state_f(self.state, "pick6_recency_curve", 1.0)) }
            None => 0.5,
        }
    }

    fn score(&self, g: &Value, relax: f64) -> Option<(f64, f64, Vec<String>)> {
        let (sim, matched) = self.tag_similarity(g);
        let (rev, stal, rec, hltb) = (Self::review_score(g), self.staleness(g), self.recency(g), self.hltb_length(g));
        let p = self.p;
        if relax < 1.0 {
            let w_rev_dir = if p.mode == "weighted" { p.w_review } else { 35.0 };
            if let Some(b) = self.b_review {
                let rev = rev?;
                let pct = rev * 100.0;
                if w_rev_dir >= 0.0 { if pct < b * (1.0 - relax) { return None; } } else if pct > b + (100.0 - b) * relax { return None; }
            }
            if let Some(b) = p.b_staleness {
                let lp = field(g, "last_played");
                let days = if truthy(lp) { (self.now - num(lp).unwrap_or(0.0)) / 86400.0 } else { 999999.0 };
                if p.w_staleness >= 0.0 { if days < b * (1.0 - relax) { return None; } } else if days > b * (1.0 + 9.0 * relax) { return None; }
            }
            if let Some(b) = p.b_recency {
                let rd = field(g, "release_date");
                if !truthy(rd) { return None; }
                let yr = num(rd).and_then(year_of)? as f64;
                if p.w_recency >= 0.0 { if yr < b - relax * (b - 1970.0) { return None; } } else if yr > b + relax * (self.now_year as f64 - b) { return None; }
            }
            if let Some(b) = p.b_hltb {
                let times = Self::hltb_times(g);
                if times.is_empty() { return None; }
                if p.w_hltb >= 0.0 { if times.iter().cloned().fold(f64::MIN, f64::max) / 60.0 < b * (1.0 - relax) { return None; } }
                else if times.iter().cloned().fold(f64::MAX, f64::min) / 60.0 > b * (1.0 + 9.0 * relax) { return None; }
            }
        }
        let final_ = if p.mode == "weighted" {
            let total_w = { let t = p.w_tags.abs() + p.w_review.abs() + p.w_staleness.abs() + p.w_recency.abs() + p.w_hltb.abs(); if t == 0.0 { 1.0 } else { t } };
            let sig = |w: f64, score: Option<f64>, unknown: f64| -> f64 {
                if w == 0.0 { return 0.0; }
                let s = score.unwrap_or(unknown);
                let norm = w.abs() / total_w;
                if w > 0.0 { norm * s } else { norm * (1.0 - s) }
            };
            sig(p.w_tags, Some(sim), 0.5) + sig(p.w_review, rev, 0.1) + sig(p.w_staleness, Some(stal), 0.5) + sig(p.w_recency, Some(rec), 0.1) + sig(p.w_hltb, hltb, 0.1)
        } else {
            let tag_w = state_f(self.state, "pick6_smart_tag_weight", 65.0) / 100.0;
            tag_w * sim + (1.0 - tag_w) * rev.unwrap_or(0.1)
        };
        Some((final_, sim, matched))
    }
}

/// `random.sample` stand-in used by both sides of the golden test: k draws without replacement, one random number each.
pub fn random_sample(n: usize, k: usize, rng: &mut dyn FnMut() -> f64) -> Vec<usize> {
    let mut pool: Vec<usize> = (0..n).collect();
    (0..k.min(n)).map(|_| { let i = (rng() * pool.len() as f64) as usize; pool.remove(i.min(pool.len() - 1)) }).collect()
}

/// `pick.pick_game` minus the I/O: choose up to six games from `games` (rows as JSON objects) for these parameters.
pub fn pick_games(games: &[Value], profile: &Profile, state: &Value, p: &PickParams, now: f64, now_year: i64, rng: &mut dyn FnMut() -> f64) -> PickResult {
    const NUM_PICKS: usize = 6;
    let mut p_eff = PickParams { mode: p.mode.clone(), b_review: p.b_review, ..*p };
    if p.mode == "smart" && p.b_review.is_none() { p_eff.b_review = Some(70.0); }
    let p = &p_eff;
    let appid = |g: &Value| field(g, "appid").as_i64().unwrap_or(0);
    if games.is_empty() { return PickResult { picks: vec![], pool_size: 0, bounded_pool_size: 0, bounds_relaxed: false }; }
    if p.mode != "smart" && p.mode != "weighted" {
        let picks = random_sample(games.len(), NUM_PICKS, rng).into_iter().map(|i| Pick { appid: appid(&games[i]), reason: None, bounds_relaxed: false }).collect();
        return PickResult { picks, pool_size: games.len(), bounded_pool_size: games.len(), bounds_relaxed: false };
    }

    // --- the tag profile
    let cap_h = state_f(state, "tag_similarity_playtime_cap_hours", 60.0);
    let weight = |pt: &Value| -> f64 { let hours = if truthy(pt) { num(pt).unwrap_or(0.0) / 60.0 } else { 0.0 }; if hours <= 0.0 { 0.0 } else { (hours.ln_1p() / cap_h.ln_1p()).min(1.0) } };
    let mut liked_w: Option<Vec<f64>> = Some(profile.liked.iter().map(|(_, pt)| weight(pt)).collect());
    if !liked_w.as_ref().unwrap().iter().any(|w| *w > 0.0) { liked_w = None; }
    let liked_tags: Vec<Vec<String>> = profile.liked.iter().map(|(t, _)| split_tags(t)).collect();
    let lib_tags: Vec<Vec<String>> = profile.library_tags.iter().map(split_tags).collect();
    let liked_rate: HashMap<String, f64> = tag_pool_rate(&liked_tags, liked_w.as_deref()).into_iter().collect();
    let min_rate = state_f(state, "tag_similarity_min_library_rate", 2.0) / 100.0;
    let library_rate: Vec<(String, f64)> = tag_pool_rate(&lib_tags, None).into_iter().filter(|(_, r)| *r >= min_rate).collect();
    let affinity: HashMap<String, f64> = library_rate.iter().map(|(t, r)| (t.clone(), liked_rate.get(t).copied().unwrap_or(0.0) - r)).collect();
    let affinity = apply_tag_overrides(affinity, state.get("tag_similarity_overrides"));

    let mut sc = Scorer { state, p, affinity, k: state_f(state, "tag_similarity_smoothing_k", 4.0), raw: HashMap::new(), raw_min: 0.0, raw_span: 0.0, now, now_year, b_review: p.b_review };
    for g in games { let r = sc.raw_tag_score(g); sc.raw.insert(appid(g), r); }
    let known: Vec<f64> = sc.raw.values().filter_map(|v| *v).collect();
    if !known.is_empty() {
        let (mn, mx) = (known.iter().cloned().fold(f64::MAX, f64::min), known.iter().cloned().fold(f64::MIN, f64::max));
        sc.raw_min = mn; sc.raw_span = mx - mn;
    }

    let has_bounds = p.b_review.is_some() || p.b_staleness.is_some() || p.b_recency.is_some() || p.b_hltb.is_some();
    let bounded = if has_bounds { games.iter().filter(|g| sc.score(g, 0.0).is_some()).count() } else { games.len() };
    let mut remaining: Vec<&Value> = games.iter().collect();
    let (mut picks, mut any_relaxed) = (Vec::new(), false);
    for _ in 0..NUM_PICKS.min(remaining.len()) {
        let mut scored: Vec<((f64, f64, Vec<String>), &Value)> = remaining.iter().filter_map(|g| sc.score(g, 0.0).map(|r| (r, *g))).collect();
        let mut relaxed = false;
        if scored.is_empty() && has_bounds {
            for step in 1..=20 {
                let f = step as f64 * 0.05;
                scored = remaining.iter().filter_map(|g| sc.score(g, f).map(|r| (r, *g))).collect();
                if !scored.is_empty() { relaxed = true; any_relaxed = true; break; }
            }
        }
        if scored.is_empty() { break; }
        let total = py_sum(scored.iter().map(|(s, _)| s.0));
        let (game, matched): (&Value, Vec<String>);
        if total == 0.0 {
            let i = (rng() * scored.len() as f64) as usize;
            game = scored[i.min(scored.len() - 1)].1; matched = vec![];
        } else {
            let r = rng() * total;
            let mut cumulative = 0.0;
            let last = scored.last().unwrap();
            let (mut g, mut m) = (last.1, last.0 .2.clone());
            for ((f, _s, mm), gg) in &scored { cumulative += f; if r <= cumulative { g = *gg; m = mm.clone(); break; } }
            game = g; matched = m;
        }
        let rev = Scorer::review_score(game);
        let profile_desc = if profile.using_fallback { "your most-played games" } else { "games you've beaten" };
        let order: Vec<(&str, f64)> = if p.mode == "weighted" {
            let mut v = vec![("tags", p.w_tags), ("review", p.w_review), ("staleness", p.w_staleness), ("recency", p.w_recency), ("hltb", p.w_hltb)];
            v.sort_by(|a, b| b.1.abs().partial_cmp(&a.1.abs()).unwrap_or(std::cmp::Ordering::Equal));
            v
        } else { vec![("tags", 65.0), ("review", 35.0)] };
        let mut phrases: Vec<String> = Vec::new();
        for (key, w) in order {
            if w == 0.0 || phrases.len() >= 3 { continue; }
            let ph: Option<String> = match key {
                "tags" => if w > 0.0 && !matched.is_empty() { Some(format!("matches {profile_desc} on {}", matched.iter().take(3).cloned().collect::<Vec<_>>().join(", "))) } else { None },
                "review" => rev.map(|r| { let pct = (r * 100.0) as i64; if w > 0.0 { format!("well reviewed ({pct}%)") } else { format!("low-reviewed ({pct}%)") } }),
                "staleness" => {
                    let lp = field(game, "last_played");
                    if truthy(lp) {
                        let days = (now - num(lp).unwrap_or(0.0)) / 86400.0;
                        if w > 0.0 { Some(if days >= 365.0 { format!("last played {:.0}yr ago", days / 365.0) } else if days >= 30.0 { format!("last played {}mo ago", (days / 30.0) as i64) } else { format!("last played {}d ago", days as i64) }) } else { None }
                    } else if w > 0.0 { Some("never played".to_string()) } else { None }
                }
                "recency" => { let rd = field(game, "release_date"); if truthy(rd) { num(rd).and_then(year_of).map(|y| format!("{y} release")) } else { None } }
                "hltb" => { let t = Scorer::hltb_times(game); if t.is_empty() { None } else {
                    let v = if w > 0.0 { t.iter().cloned().fold(f64::MIN, f64::max) } else { t.iter().cloned().fold(f64::MAX, f64::min) };
                    Some(format!("~{}h to beat", py_round(v / 60.0))) } }
                _ => None,
            };
            if let Some(ph) = ph { phrases.push(ph); }
        }
        let mut reason = if !phrases.is_empty() { capitalize(&phrases.join("; ")) + "." }
            else if let Some(r) = rev { format!("Solid reviews ({}%).", (r * 100.0) as i64) } else { "Picked based on your library.".to_string() };
        if field(game, "completion_status").as_str() == Some("Unfinished") { reason += " You've started this one before."; }
        picks.push(Pick { appid: appid(game), reason: Some(reason), bounds_relaxed: relaxed });
        let id = appid(game);
        remaining.retain(|g| appid(g) != id);
    }
    PickResult { picks, pool_size: games.len(), bounded_pool_size: bounded, bounds_relaxed: any_relaxed }
}

// ---------------------------------------------------------------- reading the data

fn row_json(row: &rusqlite::Row, names: &[String]) -> Value {
    use rusqlite::types::ValueRef;
    let mut m = serde_json::Map::new();
    for (i, n) in names.iter().enumerate() {
        m.insert(n.clone(), match row.get_ref(i).unwrap() {
            ValueRef::Null | ValueRef::Blob(_) => Value::Null,
            ValueRef::Integer(v) => Value::from(v),
            ValueRef::Real(v) => Value::from(v),
            ValueRef::Text(t) => Value::String(String::from_utf8_lossy(t).into_owned()),
        });
    }
    Value::Object(m)
}
fn sql_param(v: &Value) -> rusqlite::types::Value {
    use rusqlite::types::Value as V;
    match v { Value::Null => V::Null, Value::Bool(b) => V::Integer(*b as i64), Value::Number(n) => n.as_i64().map(V::Integer).unwrap_or_else(|| V::Real(n.as_f64().unwrap_or(0.0))), Value::String(s) => V::Text(s.clone()), o => V::Text(o.to_string()) }
}

/// The columns the engine (and the page's card actions) read; loading all 60-odd per game is slow and heavy on a big library.
/// `field()` yields null for a missing key, so a column left out here would silently change picks: `pick6_matches_python` guards this.
pub const PICK_COLUMNS: &str = "appid, name, installed, platform, completion_status, last_played, release_date, tags, review_percentage, weighted_percentage, hltb_main, hltb_extras, hltb_completionist";

pub struct PickData { pub games: Vec<Value>, pub liked: Vec<(Option<String>, Value)>, pub library_tags: Vec<Option<String>>, pub using_fallback: bool }

/// The three reads `pick_game` makes. Every query ends in a fixed order (`ORDER BY appid`) so results do not depend on the SQLite version.
pub fn fetch_pick_data(conn: &rusqlite::Connection, state: &Value, use_filtered: bool, statuses: Option<&[String]>,
                       supplements: &HashMap<String, Vec<i64>>, priority: Option<&[String]>) -> Result<PickData, rusqlite::Error> {
    let (mut where_, mut params) = build_pick_where(state, use_filtered, supplements, priority);
    if let Some(st) = statuses {
        where_ = format!("({where_}) AND completion_status IN ({})", st.iter().map(|_| "?").collect::<Vec<_>>().join(","));
        params.extend(st.iter().map(|s| Value::String(s.clone())));
    }
    let sql = format!("SELECT {PICK_COLUMNS} FROM games WHERE {where_} ORDER BY appid");
    let mut stmt = conn.prepare(&sql)?;
    let names: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();
    let games: Vec<Value> = stmt.query_map(rusqlite::params_from_iter(params.iter().map(sql_param)), |r| Ok(row_json(r, &names)))?.collect::<Result<_, _>>()?;

    let liked_of = |sql: &str| -> Result<Vec<(Option<String>, Value)>, rusqlite::Error> {
        conn.prepare(sql)?.query_map([], |r| Ok((r.get::<_, Option<String>>(0)?, row_json_value(r, 1)))).and_then(|it| it.collect())
    };
    let mut liked = liked_of("SELECT tags, playtime_forever FROM games WHERE completion_status IN ('Beaten', 'Completed') AND platform = 'steam' AND tags IS NOT NULL AND tags != '' ORDER BY appid")?;
    let using_fallback = liked.is_empty();
    if using_fallback {
        liked = liked_of("SELECT tags, playtime_forever FROM games WHERE platform = 'steam' AND tags IS NOT NULL AND tags != '' ORDER BY playtime_forever DESC, appid LIMIT 50")?;
    }
    let library_tags = conn.prepare("SELECT tags FROM games WHERE platform = 'steam' AND tags IS NOT NULL AND tags != '' ORDER BY appid")?
        .query_map([], |r| r.get::<_, Option<String>>(0))?.collect::<Result<_, _>>()?;
    Ok(PickData { games, liked, library_tags, using_fallback })
}

fn row_json_value(row: &rusqlite::Row, i: usize) -> Value {
    use rusqlite::types::ValueRef;
    match row.get_ref(i).unwrap() { ValueRef::Integer(v) => Value::from(v), ValueRef::Real(v) => Value::from(v), ValueRef::Text(t) => Value::String(String::from_utf8_lossy(t).into_owned()), _ => Value::Null }
}

/// `pick_status_counts`: games per completion status under the pick page's WHERE (the whole library, or the active filter).
pub fn status_counts(conn: &rusqlite::Connection, state: &Value, use_filtered: bool, supplements: &HashMap<String, Vec<i64>>, priority: Option<&[String]>)
    -> Result<std::collections::BTreeMap<String, i64>, rusqlite::Error> {
    let (where_, params) = build_pick_where(state, use_filtered, supplements, priority);
    let mut counts: std::collections::BTreeMap<String, i64> = ["Never Played", "Unfinished", "Beaten", "Completed", "Won't Play"].iter().map(|s| (s.to_string(), 0)).collect();
    let mut stmt = conn.prepare(&format!("SELECT completion_status, COUNT(*) FROM games WHERE ({where_}) GROUP BY completion_status"))?;
    let rows = stmt.query_map(rusqlite::params_from_iter(params.iter().map(sql_param)), |r| Ok((r.get::<_, Option<String>>(0)?, r.get::<_, i64>(1)?)))?;
    for r in rows { let (s, n) = r?; if let Some(s) = s { if let Some(c) = counts.get_mut(&s) { *c = n; } } }
    Ok(counts)
}
