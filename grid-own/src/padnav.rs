//! Gamepad cursor movement over cards. Pure functions: the callers supply card positions and act on the answer.

use crate::gamepad::Nav;

/// Library grid: rows of `(first index, count)`; the cursor is a game index. Up/Down keep the column (clamped to a short last row),
/// Left/Right step one card and wrap to the neighbouring row, page moves `page_rows` rows.
pub fn grid_move(rows: &[(usize, usize)], cur: usize, dir: Nav, page_rows: usize) -> usize {
    let Some(ri) = rows.iter().position(|&(f, c)| cur >= f && cur < f + c) else { return rows.first().map_or(0, |r| r.0) };
    let (first, count) = rows[ri];
    let col = cur - first;
    let to_row = |r: usize| { let (f, c) = rows[r]; f + col.min(c.saturating_sub(1)) };
    match dir {
        Nav::Left => if col > 0 { cur - 1 } else if ri > 0 { rows[ri - 1].0 + rows[ri - 1].1 - 1 } else { cur },
        Nav::Right => if col + 1 < count { cur + 1 } else if ri + 1 < rows.len() { rows[ri + 1].0 } else { cur },
        Nav::Up => if ri > 0 { to_row(ri - 1) } else { cur },
        Nav::Down => if ri + 1 < rows.len() { to_row(ri + 1) } else { cur },
        Nav::PagePrev => to_row(ri.saturating_sub(page_rows.max(1))),
        Nav::PageNext => to_row((ri + page_rows.max(1)).min(rows.len() - 1)),
        _ => cur,
    }
}

/// The scroll offset that brings a row (top, height) into a viewport of `view_h` starting at `scroll`, moving as little as possible.
pub fn scroll_into_view(scroll: f32, view_h: f32, top: f32, h: f32, margin: f32) -> f32 {
    if top - margin < scroll { (top - margin).max(0.0) } else if top + h + margin > scroll + view_h { top + h + margin - view_h } else { scroll }
}

/// Free-form cards (Home shelves, Pick 6): the nearest card in the direction pressed. A candidate must lie beyond the current card's
/// edge on that side; the score is the distance along the direction plus twice the sideways offset, so a card straight ahead beats a
/// nearer one off to the side. `None` when nothing lies that way. Rects are (x, y, w, h).
pub fn nearest(cards: &[(i64, [f32; 4])], cur: i64, dir: Nav) -> Option<i64> {
    let c = cards.iter().find(|c| c.0 == cur)?.1;
    let (cx, cy) = (c[0] + c[2] / 2.0, c[1] + c[3] / 2.0);
    // First choice: a card in the same row (for Left/Right) or column (Up/Down), so a wide neighbour on the row above or below can never
    // beat the card straight across; the nearest of those wins.
    let horizontal = matches!(dir, Nav::Left | Nav::Right);
    let overlaps = |r: &[f32; 4]| if horizontal { r[1] < c[1] + c[3] && c[1] < r[1] + r[3] } else { r[0] < c[0] + c[2] && c[0] < r[0] + r[2] };
    let ahead = |r: &[f32; 4]| { let (ox, oy) = (r[0] + r[2] / 2.0, r[1] + r[3] / 2.0); match dir { Nav::Right => ox > cx, Nav::Left => ox < cx, Nav::Down => oy > cy, Nav::Up => oy < cy, _ => false } };
    let dist = |r: &[f32; 4]| if horizontal { ((r[0] + r[2] / 2.0) - cx).abs() } else { ((r[1] + r[3] / 2.0) - cy).abs() };
    let same_line = cards.iter().filter(|o| o.0 != cur && overlaps(&o.1) && ahead(&o.1))
        .min_by(|a, b| dist(&a.1).partial_cmp(&dist(&b.1)).unwrap()).map(|x| x.0);
    if same_line.is_some() { return same_line; }
    cards.iter().filter(|o| o.0 != cur).filter_map(|&(id, r)| {
        let (ox, oy) = (r[0] + r[2] / 2.0, r[1] + r[3] / 2.0);
        let (along, side) = match dir {
            Nav::Right => (ox - cx, (oy - cy).abs()), Nav::Left => (cx - ox, (oy - cy).abs()),
            Nav::Down => (oy - cy, (ox - cx).abs()), Nav::Up => (cy - oy, (ox - cx).abs()),
            _ => return None,
        };
        // beyond the edge on that side (centres compared against half sizes keeps cards of different shapes honest)
        let half = if matches!(dir, Nav::Left | Nav::Right) { c[2] / 2.0 } else { c[3] / 2.0 };
        if along < half * 0.5 { return None; }
        Some((id, along + 2.0 * side))
    }).min_by(|a, b| a.1.partial_cmp(&b.1).unwrap()).map(|x| x.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn grid_moves() {
        let rows = [(0, 4), (4, 4), (8, 2)];
        assert_eq!(grid_move(&rows, 5, Nav::Right, 3), 6);
        assert_eq!(grid_move(&rows, 7, Nav::Right, 3), 8);   // wraps to the next row
        assert_eq!(grid_move(&rows, 4, Nav::Left, 3), 3);
        assert_eq!(grid_move(&rows, 0, Nav::Left, 3), 0);
        assert_eq!(grid_move(&rows, 7, Nav::Down, 3), 9);    // column 3 clamped to the short row
        assert_eq!(grid_move(&rows, 9, Nav::Up, 3), 5);
        assert_eq!(grid_move(&rows, 9, Nav::Down, 3), 9);
        assert_eq!(grid_move(&rows, 9, Nav::PagePrev, 2), 1);
        assert_eq!(grid_move(&rows, 1, Nav::PageNext, 5), 9);
    }
    #[test]
    fn scrolling_is_minimal() {
        assert_eq!(scroll_into_view(100.0, 500.0, 150.0, 200.0, 10.0), 100.0);
        assert_eq!(scroll_into_view(100.0, 500.0, 50.0, 200.0, 10.0), 40.0);
        assert_eq!(scroll_into_view(100.0, 500.0, 500.0, 200.0, 10.0), 210.0);
        assert_eq!(scroll_into_view(100.0, 500.0, 0.0, 200.0, 10.0), 0.0);
    }
    #[test]
    fn free_form_neighbours() {
        let cards = [(1, [0.0, 0.0, 100.0, 150.0]), (2, [110.0, 0.0, 100.0, 150.0]), (3, [220.0, 0.0, 100.0, 150.0]), (4, [0.0, 200.0, 100.0, 150.0])];
        assert_eq!(nearest(&cards, 1, Nav::Right), Some(2));
        assert_eq!(nearest(&cards, 2, Nav::Left), Some(1));
        assert_eq!(nearest(&cards, 1, Nav::Down), Some(4));
        assert_eq!(nearest(&cards, 4, Nav::Up), Some(1));
        assert_eq!(nearest(&cards, 3, Nav::Right), None);
        assert_eq!(nearest(&cards, 3, Nav::Down), Some(4));   // nothing below: the only card lower down still counts
    }
    #[test]
    fn a_wide_control_above_does_not_beat_the_neighbour_in_the_row() {
        // Pick 6: a wide toggle (centre between the two pills) sits above the status pills
        let cards = [(1, [0.0, 0.0, 200.0, 20.0]), (2, [0.0, 30.0, 80.0, 20.0]), (3, [90.0, 30.0, 70.0, 20.0])];
        assert_eq!(nearest(&cards, 3, Nav::Left), Some(2));
        assert_eq!(nearest(&cards, 2, Nav::Right), Some(3));
        assert_eq!(nearest(&cards, 3, Nav::Up), Some(1));
        assert_eq!(nearest(&cards, 1, Nav::Down), Some(2));
    }
}
