//! Port of PlayDate's filter tree -> SQL compiler (`library.build_tree_sql`, `build_condition_sql`,
//! `is_safe_sql`, `_auto_cast_int_division`, saved-filter references). Behaviour is pinned by golden
//! cases generated from the Python implementation (`tests/gen_golden.py`), not by reading the code.
use regex::Regex;
use serde_json::{Map, Value};
use std::collections::{HashMap, HashSet};

pub mod query;
pub mod wine_dpapi;
pub mod startup_sync;
pub mod battle_net_account;
pub mod battle_net_local;
pub use query::strip_search_condition as strip_search;
pub mod shelf;
pub mod home;
pub mod pick;
pub mod dupes;
pub mod steamlocal;
pub mod steamweb;
pub mod hltb;
pub mod net;
pub mod steamclient;
pub mod account;
pub mod jobs;
pub mod status;
pub mod importer;
pub mod sources;
pub mod sds;
pub mod xbox_owned;
pub mod proc;
pub mod winuninstall;
pub mod windpapi;
pub mod macwine;
pub mod companion;
pub mod native_exe;
pub mod installdir;
pub mod archive;
pub mod addgame;
pub mod junk;
pub mod recalc;
pub mod tagaffinity;
pub mod updater;
pub mod weblogin;
pub mod steamsignin;
pub mod artfocus;
pub mod steamsession;
pub mod steamowned;
pub mod secrets;
pub mod steamlicenses;
pub mod whats_new;
pub mod hltb_ops;
pub mod schema;
pub mod populate;
pub mod setup;
pub mod backup;
pub mod zipwrite;
pub mod launcher_installer;
pub mod import_playdate;
pub mod sandbox;
pub mod wine;
pub mod pagywosg;
pub mod pagywosg_event;
pub mod pagywosg_filter;
pub mod miam;
pub mod instance;
pub mod blaeo;
pub mod steamgifts;
pub mod steamgifts_session;
pub mod steamgifts_runner;
pub mod steamstats;
pub mod playtime_sync;
pub mod pop_sync;
pub mod rollback_marker;
pub mod plugin;
pub mod artclient;
pub mod metadata;
pub mod hltbclient;
pub mod emulators;
pub mod dbimport;
pub mod sendlog;
pub mod steam_dates;
pub mod playnite;
pub mod selfupdate;
pub mod installed_apps;
pub mod home_edit;
pub mod gamepad_db;
use std::sync::OnceLock;

pub const SAFE_COLUMNS: &[&str] = &[
    "appid", "name", "completion_status", "installed", "release_date", "date_added",
    "last_played", "playtime_forever", "review_percentage", "weighted_percentage",
    "review_score", "vertical_art_source", "horizontal_art_source", "icon_source",
    "groups", "tags", "developers", "publishers",
    "total_reviews", "positive_reviews", "unlocked_achievements", "total_achievements",
    "genres", "categories", "is_free",
    "protondb_tier", "protondb_confidence",
    "hltb_main", "hltb_extras", "hltb_completionist",
    "platform", "platform_id", "duplicate_of",
    "tag_similarity", "metacritic_score",
];
const SQL_ALLOWED_KEYWORDS: &[&str] = &[
    "and", "or", "not", "in", "like", "ilike", "between", "is", "null",
    "true", "false", "exists", "case", "when", "then", "else", "end",
    "order", "by", "asc", "desc", "limit", "where",
    "as", "text", "integer", "real", "blob", "numeric",
];
const SQL_ALLOWED_FUNCTIONS: &[&str] = &[
    "lower", "upper", "trim", "length", "substr", "replace",
    "coalesce", "ifnull", "nullif", "abs", "round", "date", "strftime", "cast",
];
const INTEGER_COLUMNS: &[&str] = &[
    "appid", "installed", "unlocked_achievements", "total_achievements",
    "review_percentage", "weighted_percentage", "total_reviews",
    "positive_reviews", "is_free", "metacritic_score",
];
const DATE_COLUMNS: &[&str] = &["release_date", "date_added", "last_played"];
const COMMA_COLUMNS: &[&str] = &["tags", "groups", "genres", "categories"];

/// Everything the Python code reads from `config`/state while compiling a tree.
#[derive(Default, Clone)]
pub struct Ctx {
    /// `state.json` "saved_filters": name -> filter (either `{id, tree}` or a bare tree). Insertion order matters.
    pub saved_filters: Map<String, Value>,
    /// supplement appid lists by source key (`icaio_ga`, `icaio_wl`), any order (sorted when used).
    pub supplements: HashMap<String, Vec<i64>>,
    /// state.json `artwork_orientation == "horizontal"` (decides which edge column the Cover Shape filter uses).
    pub horizontal: bool,
}

fn re(cell: &'static OnceLock<Regex>, pat: &str) -> &'static Regex { cell.get_or_init(|| Regex::new(pat).unwrap()) }

// ---------------------------------------------------------------- safety validator

pub fn is_safe_sql(sql: &str) -> bool {
    static STR: OnceLock<Regex> = OnceLock::new();
    static NUM: OnceLock<Regex> = OnceLock::new();
    static TOK: OnceLock<Regex> = OnceLock::new();
    static PUNCT: OnceLock<Regex> = OnceLock::new();
    if sql.trim().is_empty() { return false; }
    let scrubbed = re(&STR, r"'[^']*(?:''[^']*)*'").replace_all(sql, "STRING");
    let scrubbed = re(&NUM, r"\b\d+(\.\d+)?\b").replace_all(&scrubbed, "NUMBER");
    for m in re(&TOK, r"[A-Za-z_][A-Za-z0-9_]*|[^\s\w]").find_iter(&scrubbed) {
        let tok = m.as_str();
        let t = tok.to_lowercase();
        if SAFE_COLUMNS.contains(&t.as_str()) || SQL_ALLOWED_KEYWORDS.contains(&t.as_str())
            || SQL_ALLOWED_FUNCTIONS.contains(&t.as_str()) || t == "string" || t == "number"
            || re(&PUNCT, r"^[=<>!%()\[\],.*?+\-/|]$").is_match(tok) { continue; }
        return false;
    }
    true
}

pub fn auto_cast_int_division(sql: &str) -> String {
    static R: OnceLock<Regex> = OnceLock::new();
    re(&R, r"\b([A-Za-z_][A-Za-z0-9_]*)\s*/").replace_all(sql, |c: &regex::Captures| {
        let col = &c[1];
        if INTEGER_COLUMNS.contains(&col.to_lowercase().as_str()) { format!("CAST({col} AS REAL) /") } else { c[0].to_string() }
    }).into_owned()
}

pub(crate) fn strip_sql_wrapper(sql: &str) -> String {
    let sql: String = sql.chars().take(10000).collect();
    let upper = sql.to_ascii_uppercase();
    let mut s = sql.as_str();
    if let Some(i) = upper.find(" WHERE ") { s = &sql[i + 7..]; }
    let up2 = s.to_ascii_uppercase();
    if let Some(i) = up2.rfind(" ORDER BY ") { s = &s[..i]; }
    s.trim().to_string()
}

// ---------------------------------------------------------------- helpers mirroring Python semantics

/// Python's `str(v)` / f-string rendering of a JSON value.
fn py_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => "None".into(),
        Value::Bool(b) => if *b { "True".into() } else { "False".into() },
        Value::Number(n) => if let Some(f) = n.as_f64().filter(|_| !n.is_i64() && !n.is_u64()) {
            if f.fract() == 0.0 && f.abs() < 1e16 { format!("{:.1}", f) } else { format!("{}", f) }
        } else { n.to_string() },
        other => other.to_string(),
    }
}

/// Python truthiness of a JSON value.
pub(crate) fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().map_or(true, |f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// `database.date_to_ts`: 'YYYY-MM-DD' (first 10 chars, as `datetime.strptime` accepts it) -> UTC midnight timestamp.
pub fn date_to_ts(v: &Value) -> Option<i64> {
    static R: OnceLock<Regex> = OnceLock::new();
    if !truthy(v) { return None; }
    let s: String = py_str(v).chars().take(10).collect();
    let c = re(&R, r"^(\d{4})-(1[0-2]|0[1-9]|[1-9])-(3[01]|[12]\d|0[1-9]|[1-9]| [1-9])$").captures(&s)?;
    let (y, m, d): (i64, i64, i64) = (c[1].parse().ok()?, c[2].parse().ok()?, c[3].trim().parse().ok()?);
    let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
    let dim = [31, if leap { 29 } else { 28 }, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31][(m - 1) as usize];
    if y < 1 || d > dim { return None; }
    Some(days_from_civil(y, m, d) * 86400)
}

fn zfill2(s: &str) -> String { if s.chars().count() >= 2 { s.to_string() } else { format!("{:0>2}", s) } }

fn cover_fit_sql(op: &str, val: &Value, ctx: &Ctx) -> String {
    let edge = if ctx.horizontal { "edge_horizontal" } else { "edge_vertical" };
    let expr = match val.as_str() {
        Some("fits") => format!("(COALESCE({edge}, 'x') = '-')"),
        Some("wrong") => format!("(COALESCE({edge}, '-') != '-')"),
        Some("none") => format!("({edge} IS NULL)"),
        _ => return "1=1".into(),
    };
    match op { "=" => expr, "!=" => format!("NOT {expr}"), _ => "1=1".into() }
}

/// Narrowest cover (pixels) that still looks sharp on a card (90% of Steam's 600 and 460, chosen by Rob 2026-11-11): under this a vertical cover (600x900 is Steam's) or a horizontal one (460 wide is Steam's header)
/// is flagged "low resolution". A size that was never measured (NULL) is never flagged.
pub const LOW_RES_VERTICAL_W: i64 = 540;
pub const LOW_RES_HORIZONTAL_W: i64 = 414;

/// "Cover Quality (this view)": `ok` (fits the card and is sharp enough), `missing`, `wrong` (shape), `low` (resolution) and `q95` (saved at JPEG quality 95), for the view's cover.
fn cover_quality_sql(op: &str, val: &Value, ctx: &Ctx) -> String {
    let (edge, w, min, q) = if ctx.horizontal { ("edge_horizontal", "horizontal_art_w", LOW_RES_HORIZONTAL_W, "horizontal_art_q") } else { ("edge_vertical", "vertical_art_w", LOW_RES_VERTICAL_W, "vertical_art_q") };
    let low = format!("({w} IS NOT NULL AND {w} < {min})");
    let expr = match val.as_str() {
        Some("ok") => format!("(COALESCE({edge}, 'x') = '-' AND NOT {low})"),
        Some("missing") => format!("({edge} IS NULL)"),
        Some("wrong") => format!("(COALESCE({edge}, '-') != '-')"),
        Some("low") => low,
        // saved as a JPEG at quality 95: what PlayDate re-encoded every cover to (Zest keeps a JPEG exactly as it arrives, so a rescrape replaces it with the original)
        Some("q95") => format!("(COALESCE({q}, 0) = 95)"),
        _ => return "1=1".into(),
    };
    match op { "=" => expr, "!=" => format!("NOT {expr}"), _ => "1=1".into() }
}

// ---------------------------------------------------------------- the compiler

pub fn build_condition_sql(cond: &Value, params: &mut Vec<Value>, ctx: &Ctx) -> String {
    let col = cond.get("column").and_then(|v| v.as_str()).unwrap_or("");
    let op = cond.get("operator").and_then(|v| v.as_str()).unwrap_or("=");
    let empty = Value::String(String::new());
    let val = cond.get("value").unwrap_or(&empty);
    let one = || "1=1".to_string();

    if col == "cover_fit" { return cover_fit_sql(op, val, ctx); }
    if col == "cover_quality" { return cover_quality_sql(op, val, ctx); }
    if !SAFE_COLUMNS.contains(&col) { return one(); }

    if let Some(vc) = cond.get("value_col").filter(|v| truthy(v)) {
        let Some(vc) = vc.as_str().filter(|s| SAFE_COLUMNS.contains(s)) else { return one() };
        return if ["=", "!=", "<", ">", "<=", ">="].contains(&op) { format!("{col} {op} {vc}") } else { one() };
    }

    if let Value::Array(list) = val {
        if !COMMA_COLUMNS.contains(&col) || !["LIKE", "NOT LIKE", "=", "!="].contains(&op) { return one(); }
        let items: Vec<String> = list.iter().map(|v| py_str(v).trim().to_string()).filter(|s| !s.is_empty()).collect();
        if items.is_empty() { return one(); }
        let positive = op == "LIKE" || op == "=";
        let mut parts = Vec::new();
        for item in &items {
            params.push(Value::String(format!("%,{item},%")));
            parts.push(format!("',' || {col} || ',' {}LIKE ?", if positive { "" } else { "NOT " }));
        }
        return if parts.len() == 1 { parts.remove(0) } else { format!("({})", parts.join(if positive { " OR " } else { " AND " })) };
    }

    if val.as_str() == Some("") && op != "IS NULL" && op != "IS NOT NULL" { return one(); }

    if COMMA_COLUMNS.contains(&col) {
        params.push(Value::String(format!("%,{},%", py_str(val))));
        return format!("',' || {col} || ',' {}LIKE ?", if op == "LIKE" || op == "=" { "" } else { "NOT " });
    }

    let is_date = DATE_COLUMNS.contains(&col);
    let sval = || py_str(val);
    match op {
        "=" => {
            if is_date {
                let Some(ts) = date_to_ts(val) else { return one() };
                params.extend([Value::from(ts), Value::from(ts + 86400)]);
                return format!("({col} >= ? AND {col} < ?)");
            }
            params.push(val.clone());
            format!("{col} = ? COLLATE NOCASE")
        }
        "!=" => {
            if is_date {
                let Some(ts) = date_to_ts(val) else { return one() };
                params.extend([Value::from(ts), Value::from(ts + 86400)]);
                return format!("({col} < ? OR {col} >= ?)");
            }
            params.push(val.clone());
            format!("{col} != ? COLLATE NOCASE")
        }
        "LIKE" | "NOT LIKE" => {
            let not = if op == "NOT LIKE" { "NOT " } else { "" };
            if is_date {
                let s = sval();
                params.push(Value::String(if s.contains('%') { s } else { format!("{s}%") }));
                return format!("({col} IS NOT NULL AND {col} != 0 AND strftime('%Y-%m-%d', {col}, 'unixepoch') {not}LIKE ?)");
            }
            params.push(Value::String(format!("%{}%", sval())));
            format!("{col} {not}LIKE ?")
        }
        "STARTS_WITH" => { params.push(Value::String(format!("{}%", sval()))); format!("{col} LIKE ?") }
        ">" | "<" | ">=" | "<=" => {
            if is_date {
                let Some(ts) = date_to_ts(val) else { return one() };
                return match op {
                    ">" => { params.push(Value::from(ts + 86400)); format!("{col} >= ?") }
                    "<=" => { params.push(Value::from(ts + 86400)); format!("{col} < ?") }
                    _ => { params.push(Value::from(ts)); format!("{col} {op} ?") }
                };
            }
            params.push(val.clone());
            format!("{col} {op} ?")
        }
        "STRFTIME_MONTH" => { params.push(Value::String(zfill2(&sval()))); format!("({col} IS NOT NULL AND {col} != 0 AND strftime('%m', {col}, 'unixepoch') = ?)") }
        "STRFTIME_DAY" => { params.push(Value::String(zfill2(&sval()))); format!("({col} IS NOT NULL AND {col} != 0 AND strftime('%d', {col}, 'unixepoch') = ?)") }
        "STRFTIME_YEAR" => { params.push(val.clone()); format!("({col} IS NOT NULL AND {col} != 0 AND strftime('%Y', {col}, 'unixepoch') = ?)") }
        "STRFTIME_WEEKDAY" => { params.push(Value::String(sval())); format!("({col} IS NOT NULL AND {col} != 0 AND strftime('%w', datetime({col}, 'unixepoch')) = ?)") }
        "IS NULL" => if is_date { format!("({col} IS NULL OR {col} = 0)") } else { format!("({col} IS NULL OR {col} = '')") },
        "IS NOT NULL" => if is_date { format!("({col} IS NOT NULL AND {col} != 0)") } else { format!("({col} IS NOT NULL AND {col} != '')") },
        _ => { params.push(val.clone()); format!("{col} = ?") }
    }
}

/// A saved filter looked up by id (stable across renames), falling back to name: (name, tree, id).
fn find_saved_filter<'a>(saved: &'a Map<String, Value>, fid: Option<&str>, name: Option<&str>) -> Option<(&'a str, &'a Value, Option<&'a str>)> {
    for (sname, sf) in saved {
        let (tree, id) = match sf.as_object() {
            Some(o) if o.contains_key("tree") => (&o["tree"], o.get("id").and_then(|v| v.as_str())),
            _ => (sf, None),
        };
        let by_id = fid.map_or(false, |f| !f.is_empty() && id == Some(f));
        let by_name = fid.map_or(true, |f| f.is_empty()) && name.map_or(false, |n| !n.is_empty() && n == sname);
        if by_id || by_name { return Some((sname.as_str(), tree, id)); }
    }
    None
}

fn saved_filter_ref_sql(node: &Value, params: &mut Vec<Value>, ctx: &Ctx, stack: &[String]) -> String {
    let fid = node.get("id").and_then(|v| v.as_str());
    let name = node.get("name").and_then(|v| v.as_str());
    let Some((sname, tree, id)) = find_saved_filter(&ctx.saved_filters, fid, name) else { return "1=0".into() };
    let key = id.filter(|s| !s.is_empty()).unwrap_or(sname).to_string();
    if stack.contains(&key) || stack.len() >= 8 || !tree.is_object() { return "1=0".into(); }
    let custom = strip_sql_wrapper(tree.get("custom_sql").filter(|v| truthy(v)).and_then(|v| v.as_str()).unwrap_or(""));
    let sql = if !custom.is_empty() {
        if is_safe_sql(&custom) { format!("({})", auto_cast_int_division(&custom)) } else { "1=0".into() }
    } else {
        let mut st = stack.to_vec(); st.push(key);
        build_tree_sql_inner(tree, params, ctx, &st)
    };
    if node.get("negate").map_or(false, truthy) { format!("NOT ({sql})") } else { sql }
}

pub fn build_tree_sql(node: &Value, params: &mut Vec<Value>, ctx: &Ctx) -> String { build_tree_sql_inner(node, params, ctx, &[]) }

fn build_tree_sql_inner(node: &Value, params: &mut Vec<Value>, ctx: &Ctx, stack: &[String]) -> String {
    let ntype = node.get("type").and_then(|v| v.as_str()).unwrap_or("condition");
    match ntype {
        "saved_filter_ref" => saved_filter_ref_sql(node, params, ctx, stack),
        "condition" => build_condition_sql(node, params, ctx),
        "appid_list" => {
            let ids: Vec<String> = node.get("appids").and_then(|v| v.as_array()).map(|a| a.iter()
                .filter_map(|x| x.as_i64().filter(|_| x.is_i64() || x.is_u64())).map(|i| i.to_string()).collect()).unwrap_or_default();
            if ids.is_empty() { "1=1".into() } else { format!("appid IN ({})", ids.join(",")) }
        }
        "appid_list_ref" => {
            let mut ids = ctx.supplements.get(node.get("source").and_then(|v| v.as_str()).unwrap_or("")).cloned().unwrap_or_default();
            ids.sort_unstable(); ids.dedup();
            if ids.is_empty() { "1=1".into() } else { format!("appid IN ({})", ids.iter().map(|i| i.to_string()).collect::<Vec<_>>().join(",")) }
        }
        "pag_cond" => pagywosg_filter::cond_node_sql(node, params),
        "pag_filter" => build_tree_sql_inner(&pagywosg_filter::expand_tree(&pagywosg_filter::Spec::from_json(node.get("spec").unwrap_or(&Value::Null))), params, ctx, stack),
        "custom_expr" => {
            let sql = node.get("sql").and_then(|v| v.as_str()).unwrap_or("").trim();
            if !sql.is_empty() && is_safe_sql(sql) { format!("({})", auto_cast_int_division(sql)) } else { "1=1".into() }
        }
        "group" => {
            let logic = node.get("logic").and_then(|v| v.as_str()).unwrap_or("AND").to_uppercase();
            let logic = if logic == "AND" || logic == "OR" { logic } else { "AND".into() };
            let mut parts = Vec::new();
            for item in node.get("items").and_then(|v| v.as_array()).map(|a| a.as_slice()).unwrap_or(&[]) {
                let sql = build_tree_sql_inner(item, params, ctx, stack);
                if !sql.is_empty() && sql != "1=1" { parts.push(sql); }
            }
            if parts.is_empty() { "1=1".into() } else if parts.len() > 1 { format!("({})", parts.join(&format!(" {logic} "))) } else { parts.remove(0) }
        }
        _ => "1=1".into(),
    }
}

#[allow(dead_code)]
fn _unused(_: HashSet<i64>) {}
