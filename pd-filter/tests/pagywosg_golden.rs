//! Replays the PAGYWOSG golden cases generated from the real Python (`gen_pagywosg_golden.py`): the classifier on every category of every real event,
//! the condition-to-SQL translation, the operator table, the event id formula.
use pd_filter::pagywosg::*;
use serde_json::{json, Value};

fn load() -> Value { serde_json::from_str(&std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/pagywosg_golden.json")).expect("run gen_pagywosg_golden.py first")).unwrap() }

fn to_json(c: &Class) -> Value {
    match c {
        Class::Tag { tag, reason } => json!({"type": "tag", "reason": reason, "tag": tag}),
        Class::Cond { col, op, val, reason } => json!({"type": "cond", "reason": reason, "col": col, "op": op, "val": match val { Val::One(s) => json!(s), Val::Many(l) => json!(l) }}),
        Class::Appids { source, auto, force_wins, reason } => json!({"type": "appids", "reason": reason, "auto": auto, "force_wins": force_wins,
            "source": match source { AppidSource::IcaioGiveaways => "icaio_giveaways", AppidSource::IcaioWishlist => "icaio_wishlist", AppidSource::Santa => "santa", AppidSource::Verified => "verified" }}),
        Class::Skip { reason } => json!({"type": "skip", "reason": reason}),
    }
}
fn params(p: &[Param]) -> Value { Value::Array(p.iter().map(|x| match x { Param::Text(s) => json!(s), Param::Int(i) => json!(i) }).collect()) }

#[test]
fn classifier_matches_python_on_every_real_category() {
    let g = load();
    let (mut n, mut bad) = (0, Vec::new());
    for c in g["classify"].as_array().unwrap() {
        let f: Vec<bool> = c["flags"].as_array().unwrap().iter().map(|v| v.as_i64() == Some(1)).collect();
        let base = c["base"].as_str().unwrap();
        let got: Vec<Value> = classify(base, f[0], f[1], f[2], f[3]).iter().map(to_json).collect();
        n += 1;
        if Value::Array(got.clone()) != c["py"] { bad.push(format!("{base:?} {:?}\n  py   {}\n  rust {}", f, c["py"], Value::Array(got))); }
    }
    assert!(bad.is_empty(), "{} of {n} differ, first ones:\n{}", bad.len(), bad.iter().take(8).cloned().collect::<Vec<_>>().join("\n"));
}

#[test]
fn condition_sql_matches_python() {
    let g = load();
    for c in g["sql"].as_array().unwrap() {
        let (col, o, val) = (c["col"].as_str().unwrap(), c["op"].as_str().unwrap(), c["val"].as_str().unwrap());
        let got = redundant_where(&[(col.into(), o.into(), val.into())], &[]);
        match (&c["sql"], got) {
            (Value::Null, None) => {}
            (Value::String(s), Some((sql, p))) => { assert_eq!(&sql, s, "{col} {o} {val}"); assert_eq!(params(&p), c["params"], "params of {col} {o} {val}"); }
            (py, got) => panic!("{col} {o} {val}: python {py}, rust {got:?}"),
        }
    }
}

#[test]
fn tag_and_condition_combinations_match_python() {
    for c in load()["combos"].as_array().unwrap() {
        let conds: Vec<(String, String, String)> = c["conds"].as_array().unwrap().iter().map(|x| (x["col"].as_str().unwrap().into(), x["op"].as_str().unwrap().into(), x["val"].as_str().unwrap().into())).collect();
        let tags: Vec<String> = c["tags"].as_array().unwrap().iter().map(|t| t.as_str().unwrap().into()).collect();
        match (&c["sql"], redundant_where(&conds, &tags)) {
            (Value::Null, None) => {}
            (Value::String(s), Some((sql, p))) => { assert_eq!(&sql, s); assert_eq!(params(&p), c["params"]); }
            (py, got) => panic!("python {py}, rust {got:?}"),
        }
    }
}

#[test]
fn operator_table_matches_python() {
    let g = load();
    let ops = g["ops"].as_object().unwrap();
    assert_eq!(ops.len(), OPS.len());
    for (name, m) in ops {
        let o = op(name).unwrap_or_else(|| panic!("missing op {name}"));
        assert_eq!(m["tree_op"].as_str().unwrap(), o.tree_op, "{name}");
        assert_eq!(m["kind"].as_str().unwrap(), o.kind, "{name}");
        assert_eq!(m["fmt"].as_str(), o.fmt, "{name}");
    }
}

#[test]
fn event_ids_and_year_starts_match_python() {
    let g = load();
    for c in g["ids"].as_array().unwrap() { assert_eq!(event_id(c["y"].as_i64().unwrap() as i32, c["m"].as_i64().unwrap() as i32) as i64, c["py"].as_i64().unwrap()); }
    for c in g["years"].as_array().unwrap() { assert_eq!(year_start_ts(c["y"].as_i64().unwrap()), c["py"].as_i64().unwrap(), "year {}", c["y"]); }
}
