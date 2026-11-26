//! Which games' art is wanted soonest. The pages say what is on screen (the cards drawn now), what is just off screen (a couple of rows either way, the next cards of a scrolling
//! shelf) and which games the current view lists at all (the Library's filtered, de-duplicated list). The art job asks `rank` before every download, so what the player is looking at
//! is fetched first, then what they are about to scroll to, then the rest of the filtered set, then everything else, and games hidden under a duplicate last of all.
use std::collections::HashSet;
use std::sync::{Arc, RwLock};

/// Ranks: lower is sooner.
pub const VISIBLE: u32 = 0;
pub const NEAR: u32 = 1;
pub const FILTERED: u32 = 2;
pub const OTHER: u32 = 3;
pub const HIDDEN_DUPLICATE: u32 = 4;

#[derive(Default)]
struct Sets { visible: HashSet<i64>, near: HashSet<i64>, filtered: HashSet<i64> }

#[derive(Default)]
pub struct ArtFocus { sets: RwLock<Sets> }

impl ArtFocus {
    pub fn new() -> Arc<ArtFocus> { Arc::new(ArtFocus::default()) }
    /// Replaces what the pages are showing. `near` and `filtered` may overlap `visible`; the lowest rank wins.
    pub fn set(&self, visible: HashSet<i64>, near: HashSet<i64>, filtered: HashSet<i64>) { if let Ok(mut s) = self.sets.write() { *s = Sets { visible, near, filtered }; } }
    /// Replaces only the on-screen and near sets (the page scrolled), keeping the filtered set.
    pub fn set_view(&self, visible: HashSet<i64>, near: HashSet<i64>) { if let Ok(mut s) = self.sets.write() { s.visible = visible; s.near = near; } }
    /// Replaces only the filtered set (the list changed).
    pub fn set_filtered(&self, filtered: HashSet<i64>) { if let Ok(mut s) = self.sets.write() { s.filtered = filtered; } }
    pub fn rank(&self, appid: i64, hidden: &HashSet<i64>) -> u32 {
        let Ok(s) = self.sets.read() else { return OTHER };
        if s.visible.contains(&appid) { VISIBLE } else if s.near.contains(&appid) { NEAR } else if s.filtered.contains(&appid) { FILTERED } else if hidden.contains(&appid) { HIDDEN_DUPLICATE } else { OTHER }
    }
    /// The ranking function the art job's queue takes. `hidden` = the games that sit under a duplicate (`duplicate_of` set): they are fetched last.
    pub fn ranker(self: &Arc<Self>, hidden: HashSet<i64>) -> Arc<dyn Fn(&i64) -> u32 + Send + Sync> { let me = self.clone(); Arc::new(move |a| me.rank(*a, &hidden)) }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn set(v: &[i64]) -> HashSet<i64> { v.iter().copied().collect() }
    #[test] fn visible_then_near_then_the_filtered_set_then_the_rest_then_hidden_duplicates() {
        let f = ArtFocus::new();
        f.set(set(&[1, 2]), set(&[3, 4]), set(&[1, 2, 3, 4, 5, 6]));
        let hidden = set(&[8, 9]);
        let rank = |a| f.rank(a, &hidden);
        assert_eq!([rank(1), rank(3), rank(5), rank(7), rank(8)], [VISIBLE, NEAR, FILTERED, OTHER, HIDDEN_DUPLICATE]);
        // an overlapping game takes its lowest rank, and a hidden copy that is somehow on screen still counts as on screen
        f.set(set(&[8]), set(&[]), set(&[]));
        assert_eq!(rank(8), VISIBLE);
    }
    #[test] fn the_view_and_the_filtered_set_can_change_on_their_own() {
        let f = ArtFocus::new(); let none = HashSet::new();
        f.set(set(&[1]), set(&[2]), set(&[1, 2, 3]));
        f.set_view(set(&[3]), set(&[]));
        assert_eq!((f.rank(1, &none), f.rank(3, &none)), (FILTERED, VISIBLE), "scrolled: the filtered set stays");
        f.set_filtered(set(&[]));
        assert_eq!(f.rank(1, &none), OTHER);
    }
    #[test] fn the_ranker_drives_a_queue_so_a_scroll_re_orders_the_work() {
        let f = ArtFocus::new();
        let q = crate::net::WorkQueue::with_ranker(1..=6i64, f.ranker(set(&[2])));
        f.set(set(&[5]), set(&[]), set(&[4, 5]));
        assert_eq!(q.next(), Some(5)); assert_eq!(q.next(), Some(4), "the rest of the filtered set before the others");
        f.set_view(set(&[6]), set(&[]));
        assert_eq!(q.next(), Some(6), "scrolled: 6 jumps the line");
        assert_eq!(q.next(), Some(1)); assert_eq!(q.next(), Some(3)); assert_eq!(q.next(), Some(2), "the hidden duplicate is last");
        assert_eq!(q.next(), None);
    }
}
