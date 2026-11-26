//! Editing the Home layout on the page: the changes drag, resize, split and delete make to the `shelves` array of `state.json` (`index.html`'s edit mode).
//! A row is a shelf alone, or the shelves sharing a `split_group` (side by side, the row height of the first, widths by `col_width`). All of these work on the
//! array as saved, so what the page shows afterwards is simply the array read again.
use serde_json::{json, Value};

pub const MIN_ROW_VH: i64 = 5;   // the page's Height box: 5 to 90
pub const MAX_ROW_VH: i64 = 90;

fn id_of(s: &Value) -> &str { s.get("id").and_then(|v| v.as_str()).unwrap_or("") }
fn group_of(s: &Value) -> Option<String> { s.get("split_group").filter(|g| crate::truthy(g)).map(|g| g.as_str().map(String::from).unwrap_or_else(|| g.to_string())) }
fn pos(shelves: &[Value], id: &str) -> Option<usize> { shelves.iter().position(|s| id_of(s) == id) }

/// Indices of the shelves in `id`'s row: its split group in list order, or just itself.
pub fn row_of(shelves: &[Value], id: &str) -> Vec<usize> {
    let Some(i) = pos(shelves, id) else { return vec![] };
    match group_of(&shelves[i]) { Some(g) => (0..shelves.len()).filter(|j| group_of(&shelves[*j]).as_deref() == Some(g.as_str())).collect(), None => vec![i] }
}

/// The rows the page shows (visible shelves only, as `home_rows` makes them): indices into `shelves`, a split row at the place of its first member.
pub fn rows(shelves: &[Value]) -> Vec<Vec<usize>> {
    let (mut out, mut seen): (Vec<Vec<usize>>, Vec<String>) = (vec![], vec![]);
    for (i, s) in shelves.iter().enumerate() {
        if !s.get("visible").map_or(true, crate::truthy) { continue; }
        match group_of(s) {
            Some(g) => { if seen.contains(&g) { continue; } seen.push(g.clone()); out.push((0..shelves.len()).filter(|j| shelves[*j].get("visible").map_or(true, crate::truthy) && group_of(&shelves[*j]).as_deref() == Some(g.as_str())).collect()); }
            None => out.push(vec![i]),
        }
    }
    out
}

/// Moves `id`'s whole row to just before `before`'s row (or to the end for None). False when nothing changed.
pub fn move_row(shelves: &mut Vec<Value>, id: &str, before: Option<&str>) -> bool {
    let members = row_of(shelves, id);
    if members.is_empty() { return false; }
    let target_first = before.and_then(|b| { let r = row_of(shelves, b); if r.iter().any(|j| members.contains(j)) { None } else { r.first().copied() } });
    if before.is_some() && target_first.is_none() { return false; }   // dropped on itself
    let old = shelves.clone();
    let block: Vec<Value> = members.iter().map(|i| shelves[*i].clone()).collect();
    for i in members.iter().rev() { shelves.remove(*i); }
    let at = match target_first { Some(t) => t - members.iter().filter(|m| **m < t).count(), None => shelves.len() };
    for (k, s) in block.into_iter().enumerate() { shelves.insert(at + k, s); }
    *shelves != old
}

/// The row height in vh for every shelf of the row (the page reads the first member's).
pub fn set_row_height(shelves: &mut [Value], id: &str, vh: i64) {
    let vh = vh.clamp(MIN_ROW_VH, MAX_ROW_VH);
    for i in row_of(shelves, id) { shelves[i]["row_height"] = json!(vh); }
}

/// The widths of two neighbours of a split row, keeping their sum (so the others stay as they are): `a` gets `share_a` of the two's combined weight.
pub fn set_widths(shelves: &mut [Value], a: &str, b: &str, share_a: f64) {
    let (Some(ia), Some(ib)) = (pos(shelves, a), pos(shelves, b)) else { return };
    let w = |s: &Value| s.get("col_width").and_then(|v| v.as_f64()).unwrap_or(10.0).max(0.01);
    let total = w(&shelves[ia]) + w(&shelves[ib]);
    let wa = (total * share_a.clamp(0.1, 0.9) * 100.0).round() / 100.0;
    shelves[ia]["col_width"] = json!(wa);
    shelves[ib]["col_width"] = json!(((total - wa) * 100.0).round() / 100.0);
}

fn put_or_clear(shelf: &mut Value, key: &str, v: Option<Value>) {
    match v.filter(|v| !v.is_null()) { Some(v) => shelf[key] = v, None => { if let Some(o) = shelf.as_object_mut() { o.remove(key); } } }
}

/// The page reads a row's "fill the rest" setting, its height and the height to go back to when fill is turned off from the row's FIRST shelf, but each shelf stores its own.
/// Brings every shelf of `id`'s row to what `from` has, so the row cannot disagree with itself (a shelf put before a fill row used to switch the fill off, one put after kept a
/// setting nobody read, and turning fill off then restored a different height to each shelf).
pub fn align_row(shelves: &mut [Value], id: &str, from: &str) {
    let Some(src) = pos(shelves, from) else { return };
    let (fill, h, before) = (shelves[src].get("fill").cloned(), shelves[src].get("row_height").cloned(), shelves[src].get("height_before_fill").cloned());
    for i in row_of(shelves, id) {
        put_or_clear(&mut shelves[i], "fill", fill.clone());
        if let Some(h) = &h { shelves[i]["row_height"] = h.clone(); }
        put_or_clear(&mut shelves[i], "height_before_fill", before.clone());
    }
}

/// Every side-by-side row made consistent with its first shelf (the one the page reads). A no-op for the page, which only ever looked at the first shelf; it repairs a layout
/// an earlier join left with shelves that disagree.
pub fn normalize_all(shelves: &mut [Value]) {
    let mut seen: Vec<String> = vec![];
    for i in 0..shelves.len() {
        let Some(g) = group_of(&shelves[i]) else { continue };
        if seen.contains(&g) { continue; }
        seen.push(g);
        let id = id_of(&shelves[i]).to_string();
        align_row(shelves, &id, &id);
    }
}

/// `splitShelves(idA, idB, insertBeforeId)`: `b` joins `a`'s row. `b` first leaves any row it was in (that row dissolves if it is left with one shelf); the row is `a`'s group, or a new
/// one; `b` takes `a`'s height; both get a width weight (10 when they had none). `insert_before` is the member of the row `b` goes before; None puts it after the row's last member.
pub fn split_with(shelves: &mut Vec<Value>, a: &str, b: &str, insert_before: Option<&str>) -> bool {
    let (Some(ia), Some(ib)) = (pos(shelves, a), pos(shelves, b)) else { return false };
    if ia == ib || row_of(shelves, a).contains(&ib) { return false; }
    // b leaves its old row
    if let Some(g) = group_of(&shelves[ib]) {
        let others: Vec<usize> = (0..shelves.len()).filter(|j| *j != ib && group_of(&shelves[*j]).as_deref() == Some(g.as_str())).collect();
        if others.len() == 1 { shelves[others[0]]["split_group"] = Value::Null; shelves[others[0]]["col_width"] = Value::Null; }
        shelves[ib]["split_group"] = Value::Null;
    }
    let group = group_of(&shelves[ia]).unwrap_or_else(|| format!("split_{a}_{b}"));
    let height = shelves[ia].get("row_height").cloned().unwrap_or(json!(30));
    let wa = shelves[ia].get("col_width").filter(|v| crate::truthy(v)).cloned().unwrap_or(json!(10));
    // the newcomer takes the average share of the shelves already in the row, not the weight it had in its old row (a 25 beside a 10 took most of the row)
    let others: Vec<f64> = row_of(shelves, a).into_iter().filter(|j| *j != ib).map(|j| shelves[j].get("col_width").filter(|v| crate::truthy(v)).and_then(|v| v.as_f64()).unwrap_or(10.0)).collect();
    let avg = (others.iter().sum::<f64>() / others.len().max(1) as f64 * 100.0).round() / 100.0;
    let wb = if avg.fract() == 0.0 { json!(avg as i64) } else { json!(avg) };
    shelves[ia]["split_group"] = json!(group); shelves[ia]["col_width"] = wa;
    shelves[ib]["split_group"] = json!(group); shelves[ib]["col_width"] = wb; shelves[ib]["row_height"] = height;
    let moved = shelves.remove(pos(shelves, b).unwrap());
    match insert_before.and_then(|id| pos(shelves, id)) {
        Some(at) => shelves.insert(at, moved),
        None => { let last = (0..shelves.len()).rev().find(|j| group_of(&shelves[*j]).as_deref() == Some(group.as_str())).unwrap_or(shelves.len().saturating_sub(1)); shelves.insert(last + 1, moved); }
    }
    align_row(shelves, a, a);   // the row is the row it was joined to: its fill, height and remembered height, for every shelf in it
    true
}

/// `unsplitShelf`: takes `id` out of its row and clears its width weight; a row left with one shelf stops being a split.
pub fn unsplit(shelves: &mut [Value], id: &str) {
    let Some(i) = pos(shelves, id) else { return };
    let Some(g) = group_of(&shelves[i]) else { return };
    shelves[i]["split_group"] = Value::Null; shelves[i]["col_width"] = Value::Null;
    let rest: Vec<usize> = (0..shelves.len()).filter(|j| group_of(&shelves[*j]).as_deref() == Some(g.as_str())).collect();
    if rest.len() == 1 { shelves[rest[0]]["split_group"] = Value::Null; shelves[rest[0]]["col_width"] = Value::Null; }
}

/// Dragging a shelf onto a sibling in its own row: it takes the sibling's place within the row (the other shelves of the list keep their positions).
pub fn reorder_in_row(shelves: &mut Vec<Value>, id: &str, target: &str) -> bool {
    let row = row_of(shelves, id);
    let (Some(i), Some(t)) = (pos(shelves, id), pos(shelves, target)) else { return false };
    if i == t || !row.contains(&t) { return false; }
    let mut order: Vec<usize> = row.clone();
    let from = order.iter().position(|x| *x == i).unwrap(); let to = order.iter().position(|x| *x == t).unwrap();
    let item = order.remove(from); order.insert(to, item);
    let new_vals: Vec<Value> = order.iter().map(|k| shelves[*k].clone()).collect();
    for (slot, v) in row.iter().zip(new_vals) { shelves[*slot] = v; }
    true
}

/// Where a row dropped on another row lands, as PlayDate does it: after the target when the dragged row was above it, before it when it was below. The result is what
/// `move_row` takes (the id to go before; None = the end).
pub fn drop_before(shelves: &[Value], id: &str, target: &str) -> Option<Option<String>> {
    let (mine, theirs) = (row_of(shelves, id), row_of(shelves, target));
    let (m0, t0) = (*mine.first()?, *theirs.first()?);
    if mine == theirs { return None; }
    if m0 < t0 { let after = *theirs.last()? + 1; Some(shelves.get(after).map(|s| id_of(s).to_string())) } else { Some(Some(id_of(&shelves[t0]).to_string())) }
}

/// A shelf gets a row of its own just before `before`'s row (None = at the end of the page): it leaves the row it was in first. False when nothing changed.
pub fn move_to_new_row(shelves: &mut Vec<Value>, id: &str, before: Option<&str>) -> bool {
    let old = shelves.clone();
    unsplit(shelves, id);
    move_row(shelves, id, before);
    *shelves != old
}

/// Removes a shelf (and dissolves a split row it leaves with one member).
pub fn delete(shelves: &mut Vec<Value>, id: &str) {
    let Some(i) = pos(shelves, id) else { return };
    unsplit(shelves, id);
    let i2 = pos(shelves, id).unwrap_or(i); shelves.remove(i2);
}

/// A new shelf (the page's "+ Add Shelf"): returns its id.
pub fn add_shelf(shelves: &mut Vec<Value>) -> String {
    let mut n = shelves.len() + 1; let mut id = format!("shelf_{n}");
    while pos(shelves, &id).is_some() { n += 1; id = format!("shelf_{n}"); }
    let prio = shelves.iter().filter_map(|s| s.get("dedup_priority").and_then(|v| v.as_i64())).max().unwrap_or(0) + 1;
    shelves.push(json!({ "id": id, "label": "NEW SHELF", "preset": "not_beaten", "filter_key": "not_beaten", "custom_sql": null, "limit": 4, "row_height": 30, "col_width": 10,
        "split_group": null, "sort_col": "date_added", "sort_dir": null, "dedup": true, "dedup_priority": prio, "visible": true }));
    id
}

/// Row heights in vh with "fill the rest" applied: rows marked `fill` share what the other rows leave of the window (never less than their own saved height, so a short window scrolls
/// instead of squeezing them). `base` is each row's saved height, `fill` whether it fills; the top bar takes 5.5 vh and rows are 0.5 vh apart.
pub fn row_vhs(base: &[f32], fill: &[bool]) -> Vec<f32> {
    let nfill = fill.iter().filter(|f| **f).count();
    if nfill == 0 { return base.to_vec(); }
    let fixed: f32 = base.iter().zip(fill).filter(|(_, f)| !**f).map(|(b, _)| b + 0.5).sum();
    // what the filling rows share between them; a row whose own height is more than an equal share keeps its height and the rest split what is left
    let mut avail = 100.0 - 5.5 - fixed - 0.5 * nfill as f32 - 0.5;
    let mut open: Vec<usize> = (0..base.len()).filter(|i| fill[*i]).collect();
    let mut kept = vec![false; base.len()];
    loop {
        let share = avail / open.len().max(1) as f32;
        let big: Vec<usize> = open.iter().copied().filter(|i| base[*i] > share).collect();
        if big.is_empty() || big.len() == open.len() && open.len() == 1 { break; }
        for i in &big { kept[*i] = true; avail -= base[*i]; }
        open.retain(|i| !kept[*i]);
        if open.is_empty() { break; }
    }
    let share = avail / open.len().max(1) as f32;
    base.iter().zip(fill).enumerate().map(|(i, (b, f))| if !*f { *b } else if kept[i] { *b } else { share.max(*b) }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sh(id: &str, g: Option<&str>) -> Value { json!({ "id": id, "split_group": g, "row_height": 30, "col_width": 10, "visible": true }) }
    fn ids(v: &[Value]) -> Vec<String> { v.iter().map(|s| id_of(s).to_string()).collect() }
    fn base() -> Vec<Value> { vec![sh("a", Some("top")), sh("b", Some("top")), sh("c", None), sh("d", None), sh("e", Some("low")), sh("f", Some("low"))] }

    #[test] fn joining_a_row_keeps_the_rows_fill_and_height_for_every_shelf() {
        let fill = |id: &str| json!({ "id": id, "split_group": null, "row_height": 10, "visible": true, "fill": true, "height_before_fill": 30 });
        let rows_vh = |v: &[Value]| { let r = rows(v); row_vhs(&r.iter().map(|x| v[x[0]]["row_height"].as_f64().unwrap() as f32).collect::<Vec<_>>(), &r.iter().map(|x| v[x[0]].get("fill").and_then(|f| f.as_bool()).unwrap_or(false)).collect::<Vec<_>>()) };
        // a lone shelf put BEFORE a fill row used to switch the fill off (the row read its first shelf)
        let mut v = vec![fill("a"), sh("b", None), sh("c", None)];
        assert!(split_with(&mut v, "a", "b", Some("a")));
        assert_eq!(ids(&v), ["b", "a", "c"]);
        assert!(v.iter().take(2).all(|s| s["fill"] == json!(true) && s["row_height"] == json!(10) && s["height_before_fill"] == json!(30)), "{v:?}");
        assert!(rows_vh(&v)[0] > 60.0, "the row still fills the page: {:?}", rows_vh(&v));
        // a new shelf too
        let mut v = vec![fill("a"), sh("c", None)]; let id = add_shelf(&mut v);
        assert!(split_with(&mut v, "a", &id, Some("a")));
        assert!(rows_vh(&v)[0] > 60.0, "{:?}", rows_vh(&v));
        // a fill shelf joining a row that does not fill takes that row's setting, in either place
        for before in [None, Some("b")] {
            let mut v = vec![fill("a"), sh("b", None), sh("c", None)];
            assert!(split_with(&mut v, "b", "a", before));
            assert!(v.iter().take(2).all(|s| s.get("fill").map_or(true, |f| f == &json!(false) || f.is_null()) && s["row_height"] == json!(30) && s.get("height_before_fill").is_none()), "{v:?}");
            assert_eq!(rows_vh(&v)[0], 30.0);
        }
    }
    #[test] fn a_shelf_joining_a_row_takes_the_average_width_of_the_shelves_in_it() {
        let w = |id: &str, w: f64| json!({ "id": id, "split_group": null, "row_height": 30, "col_width": w, "visible": true });
        let mut v = vec![w("a", 10.0), w("b", 25.0), w("c", 10.0)];
        assert!(split_with(&mut v, "a", "b", None));
        assert_eq!(v[1]["col_width"], json!(10), "{v:?}");   // not its old 25
        // a third shelf: the average of the two already there (the second widened by hand)
        let mut v = vec![w("a", 10.0), w("b", 25.0), w("c", 25.0)];
        assert!(split_with(&mut v, "a", "b", None)); v[1]["col_width"] = json!(30.0);
        assert!(split_with(&mut v, "a", "c", None));
        assert_eq!(v[2]["col_width"], json!(20), "{v:?}");
    }
    #[test] fn a_layout_whose_rows_disagree_is_made_consistent_with_the_first_shelf() {
        let mut v = vec![json!({ "id": "a", "split_group": "g", "row_height": 10, "fill": true, "height_before_fill": 30, "visible": true }),
                         json!({ "id": "b", "split_group": "g", "row_height": 40, "fill": false, "visible": true }), sh("c", None)];
        normalize_all(&mut v);
        assert!(v[..2].iter().all(|s| s["row_height"] == json!(10) && s["fill"] == json!(true) && s["height_before_fill"] == json!(30)), "{v:?}");
        assert_eq!(v[2]["row_height"], json!(30));   // a lone shelf is left alone
    }
    #[test] fn a_shelf_can_move_to_a_row_of_its_own() {
        // out of a pair, to the end
        let mut v = base(); assert!(move_to_new_row(&mut v, "a", None)); assert_eq!(ids(&v), ["b", "c", "d", "e", "f", "a"]); assert_eq!(row_of(&v, "a"), [5]); assert_eq!(row_of(&v, "b"), [0]);
        // out of a pair, above the rest of it: it stays first, alone
        let mut v = base(); assert!(move_to_new_row(&mut v, "a", Some("a"))); assert_eq!(ids(&v), ["a", "b", "c", "d", "e", "f"]); assert_eq!(row_of(&v, "a"), [0]); assert_eq!(row_of(&v, "b"), [1]);
        // a lone shelf put where it already is changes nothing
        let mut v = base(); assert!(!move_to_new_row(&mut v, "c", Some("d")));
        // a lone shelf above another row
        let mut v = base(); assert!(move_to_new_row(&mut v, "d", Some("a"))); assert_eq!(ids(&v), ["d", "a", "b", "c", "e", "f"]);
    }
    #[test] fn a_row_is_the_split_group_or_the_shelf_alone() {
        let v = base(); assert_eq!(row_of(&v, "b"), [0, 1]); assert_eq!(row_of(&v, "c"), [2]); assert_eq!(row_of(&v, "zz"), Vec::<usize>::new());
    }
    #[test] fn moving_a_row_keeps_its_members_together_and_in_order() {
        let mut v = base(); assert!(move_row(&mut v, "a", Some("e"))); assert_eq!(ids(&v), ["c", "d", "a", "b", "e", "f"]);
        let mut v = base(); assert!(move_row(&mut v, "d", Some("a"))); assert_eq!(ids(&v), ["d", "a", "b", "c", "e", "f"]);
        let mut v = base(); assert!(move_row(&mut v, "a", None)); assert_eq!(ids(&v), ["c", "d", "e", "f", "a", "b"]);
        let mut v = base(); assert!(!move_row(&mut v, "a", Some("b")), "onto its own row"); assert!(!move_row(&mut v, "c", Some("d")), "already just before d? c is before d");
    }
    #[test] fn heights_and_widths() {
        let mut v = base(); set_row_height(&mut v, "a", 45); assert_eq!((v[0]["row_height"].clone(), v[1]["row_height"].clone(), v[2]["row_height"].clone()), (json!(45), json!(45), json!(30)));
        set_row_height(&mut v, "c", 500); assert_eq!(v[2]["row_height"], json!(MAX_ROW_VH)); set_row_height(&mut v, "c", 0); assert_eq!(v[2]["row_height"], json!(MIN_ROW_VH)); assert_eq!(MIN_ROW_VH, 5);
        set_widths(&mut v, "a", "b", 0.75); assert_eq!((v[0]["col_width"].clone(), v[1]["col_width"].clone()), (json!(15.0), json!(5.0)));
        set_widths(&mut v, "a", "b", 5.0); assert_eq!(v[0]["col_width"], json!(18.0), "a share is held between 10% and 90%");
    }
    #[test] fn splitting_follows_playdates_rules() {
        // b leaves its old row (which dissolves with one left), joins a's group, takes a's height, widths default to 10
        let mut v = base(); v[2]["row_height"] = json!(44);
        assert!(split_with(&mut v, "c", "a", None));   // a is in "top" with b: top dissolves? no, it has b left: b alone -> dissolved
        assert_eq!(group_of(&v[v.iter().position(|s| id_of(s) == "b").unwrap()]), None, "the row a left had two shelves, so it dissolved");
        let ia = v.iter().position(|s| id_of(s) == "a").unwrap(); let ic = v.iter().position(|s| id_of(s) == "c").unwrap();
        assert_eq!(group_of(&v[ia]), group_of(&v[ic])); assert!(group_of(&v[ic]).is_some()); assert_eq!(v[ia]["row_height"], json!(44)); assert_eq!(v[ia]["col_width"], json!(10));
        assert_eq!(ids(&v)[ic + 1], "a", "after the last member when no position is given");
        // a position: before the named member
        let mut v = base(); assert!(split_with(&mut v, "c", "d", Some("c"))); assert_eq!(ids(&v), ["a", "b", "d", "c", "e", "f"]);
        let mut v = base(); assert!(split_with(&mut v, "a", "d", Some("b"))); assert_eq!(ids(&v), ["a", "d", "b", "c", "e", "f"]); assert_eq!(row_of(&v, "a"), [0, 1, 2], "a row of three: 'add to row'");
        assert!(!split_with(&mut v, "a", "b", None), "already in the same row");
        // unsplit: one shelf out, its width cleared, a row left with one dissolves
        let mut v = base(); v[0]["col_width"] = json!(7); unsplit(&mut v, "a");
        assert_eq!((group_of(&v[0]), v[0]["col_width"].clone(), group_of(&v[1]), v[1]["col_width"].clone()), (None, Value::Null, None, Value::Null));
        let mut v = base(); assert!(split_with(&mut v, "a", "c", None)); unsplit(&mut v, "c"); assert_eq!(row_of(&v, "a").len(), 2, "a row of three keeps two");
    }
    #[test] fn dragging_within_a_row_and_dropping_on_rows() {
        let mut v = base(); assert!(reorder_in_row(&mut v, "b", "a")); assert_eq!(ids(&v), ["b", "a", "c", "d", "e", "f"]); assert!(!reorder_in_row(&mut v, "a", "c"), "c is not in a's row");
        // a row from above lands after the target, one from below lands before it
        let v = base(); assert_eq!(drop_before(&v, "a", "d"), Some(Some("e".to_string()))); assert_eq!(drop_before(&v, "d", "a"), Some(Some("a".to_string())));
        assert_eq!(drop_before(&v, "e", "e"), None); assert_eq!(drop_before(&v, "c", "f"), Some(None), "after the last row: the end");
    }
    #[test] fn the_rows_are_what_the_page_shows() {
        let mut v = base(); v[3]["visible"] = json!(false);
        assert_eq!(rows(&v), vec![vec![0, 1], vec![2], vec![4, 5]], "a hidden shelf is in no row");
        v[0]["visible"] = json!(false); assert_eq!(rows(&v), vec![vec![1], vec![2], vec![4, 5]]);
    }
    #[test] fn rows_that_fill_share_what_is_left() {
        assert_eq!(row_vhs(&[30.0, 40.0, 23.0], &[false, false, false]), vec![30.0, 40.0, 23.0]);
        // the top row 30, Installed fills: 100 - 5.5 - (30 + 0.5) - 0.5 (its gap) - 0.5 (bottom) = 63
        let v = row_vhs(&[30.0, 40.0], &[false, true]); assert_eq!(v[0], 30.0); assert!((v[1] - 63.0).abs() < 0.001, "{v:?}");
        // two fillers split it; a window too short keeps their saved heights
        let v = row_vhs(&[20.0, 20.0, 20.0], &[false, true, true]); assert!((v[1] - v[2]).abs() < 0.001 && v[1] > 20.0);
        let v = row_vhs(&[60.0, 40.0], &[false, true]); assert_eq!(v[1], 40.0, "never below its own height");
        // two fillers where one keeps a bigger saved height: the other gets what is left, so the page does not run past the window
        let v = row_vhs(&[50.0, 10.0], &[true, true]); assert_eq!(v[0], 50.0); assert!((v[1] - 43.0).abs() < 0.001, "{v:?}");
        assert!((v.iter().sum::<f32>() + 5.5 + 1.0 + 0.5 - 100.0).abs() < 0.001, "{v:?}");
    }
    #[test] fn deleting_and_adding() {
        let mut v = base(); delete(&mut v, "a"); assert_eq!(ids(&v), ["b", "c", "d", "e", "f"]); assert_eq!(group_of(&v[0]), None);
        let mut v = base(); delete(&mut v, "c"); assert_eq!(v.len(), 5);
        let mut v = base(); let id = add_shelf(&mut v); assert_eq!(id, "shelf_7"); assert_eq!(id_of(v.last().unwrap()), "shelf_7"); assert_eq!(v.last().unwrap()["label"], "NEW SHELF");
        let id2 = add_shelf(&mut v); assert_ne!(id, id2);
    }
}
