//! Monthly in a Month: the community sheet of games that can be done in a month, and the filter that lists the ones you own. A port of `miam_sheet_data`
//! in `pagywosg.py` (the sheet is Google Sheets' CSV export; rows whose "Can do?" says N or N? are excluded).
use crate::net::Http;
use serde_json::{json, Value};
use std::time::Duration;

pub const SHEET_URL: &str = "https://docs.google.com/spreadsheets/d/1xBd5rltp8WxQSnG2BVKPzr7quTLJCvCt2MaGCxzs-d8/export?format=csv";

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Sheet { /// Rows with an `app/<id>` value.
    pub total: usize, /// Rows not marked N / N?, in sheet order.
    pub eligible: Vec<i64> }

/// A CSV file as rows of fields (quoted fields, doubled quotes and line breaks inside quotes).
pub fn parse_csv(text: &str) -> Vec<Vec<String>> {
    let (mut rows, mut row, mut field, mut quoted, mut chars) = (Vec::new(), Vec::new(), String::new(), false, text.chars().peekable());
    while let Some(c) = chars.next() {
        match (c, quoted) {
            ('"', true) => if chars.peek() == Some(&'"') { chars.next(); field.push('"'); } else { quoted = false; },
            ('"', false) if field.is_empty() => quoted = true,
            (',', false) => row.push(std::mem::take(&mut field)),
            ('\r', false) => {}
            ('\n', false) => { row.push(std::mem::take(&mut field)); rows.push(std::mem::take(&mut row)); }
            (c, _) => field.push(c),
        }
    }
    if !field.is_empty() || !row.is_empty() { row.push(field); rows.push(row); }
    rows
}

pub fn parse_sheet(csv: &str) -> Sheet {
    let rows = parse_csv(csv);
    let Some(head) = rows.first() else { return Sheet::default() };
    let col = |name: &str| head.iter().position(|h| h.trim() == name);
    let (Some(app), Some(can)) = (col("App ID"), col("Can do? (Y/N)")) else { return Sheet::default() };
    let mut s = Sheet::default();
    for r in &rows[1..] {
        let id = r.get(app).map(|v| v.trim()).unwrap_or("");
        let Some(n) = id.strip_prefix("app/").and_then(|n| n.parse::<i64>().ok()) else { continue };
        s.total += 1;
        if matches!(r.get(can).map(|v| v.trim()), Some("N") | Some("N?")) { continue; }
        s.eligible.push(n);
    }
    s
}

pub fn fetch_sheet(http: &dyn Http) -> Result<Sheet, String> {
    let r = http.get(SHEET_URL, &[], Duration::from_secs(15)).map_err(|e| format!("the sheet: {e}"))?;
    if r.status != 200 { return Err(format!("the sheet answered {}", r.status)); }
    Ok(parse_sheet(&r.body))
}

/// The saved filter: Steam games on the list (`appids`: the ones you own) in the chosen completion statuses.
pub fn tree(appids: &[i64], statuses: &[String]) -> Value {
    let comp: Vec<Value> = statuses.iter().map(|v| json!({"type": "condition", "column": "completion_status", "operator": "=", "value": v})).collect();
    let comp = if comp.len() == 1 { comp.into_iter().next().unwrap() } else { json!({"type": "group", "logic": "OR", "items": comp}) };
    json!({"type": "group", "logic": "AND", "miam": true, "items": [
        {"type": "condition", "column": "platform", "operator": "=", "value": "steam"}, comp,
        {"type": "appid_list", "appids": appids}]})
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn the_sheet_is_read_like_python() {
        let csv = "Name,App ID,\"Can do? (Y/N)\",Notes\r\n\"Game, One\",app/10,Y,\"a \"\"quote\"\"\"\nTwo,app/20,N,\nThree,app/30,N?,x\nFour,app/40,,\nHeader row,not an app,Y,\nFive,app/abc,Y,\nSix,app/60,Y";
        let s = parse_sheet(csv);
        assert_eq!(s, Sheet { total: 5, eligible: vec![10, 40, 60] });
        assert_eq!(parse_csv("a,\"b\nc\",d\n")[0], vec!["a", "b\nc", "d"]);
        assert_eq!(parse_sheet("nothing useful"), Sheet::default());
    }
    #[test]
    fn the_filter_tree_has_the_pieces() {
        let t = tree(&[1, 2], &["Never Played".into(), "Unfinished".into()]);
        assert_eq!(t["miam"], true);
        let mut p = vec![];
        let sql = crate::build_tree_sql(&t, &mut p, &crate::Ctx::default());
        assert!(sql.contains("appid IN (1,2)") && sql.contains("completion_status"), "{sql}");
    }
}
