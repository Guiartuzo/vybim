//! A browser-style jump history: a bounded trail of `(path, line, col)`
//! locations with back/forward navigation.
//!
//! Points are keyed by **path**, not buffer index, so the history survives
//! buffers being reordered or replaced (indices into `App.buffers` are not
//! stable identities) and can re-open a file whose buffer was closed. Only
//! deliberate, cross-distance navigations record a point — this is the
//! VSCode/Vim jumplist instinct, not an undo-of-every-motion.

use std::path::PathBuf;

/// A single location in the jump history.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct JumpPoint {
    pub path: PathBuf,
    pub line: usize,
    pub col: usize,
}

impl JumpPoint {
    pub fn new(path: PathBuf, line: usize, col: usize) -> Self {
        Self { path, line, col }
    }
}

/// A bounded back/forward history, modelled on browser navigation.
///
/// `index` is the current position within `points`. When `index ==
/// points.len()` we are at the **live tip** — the cursor sits at a location
/// not yet recorded (the destination of the last jump). Going `back` from the
/// tip first captures that live location so `forward` can return to it.
#[derive(Default, Debug)]
pub struct JumpList {
    points: Vec<JumpPoint>,
    index: usize,
}

impl JumpList {
    /// Deepest history retained; older entries are evicted. Far more than
    /// anyone navigates back through, but bounded so the trail can't leak.
    const CAP: usize = 64;

    pub fn new() -> Self {
        Self::default()
    }

    /// Push `point`, evicting the oldest entry if over [`CAP`]. Does not touch
    /// `index` — callers set it explicitly.
    fn push_capped(&mut self, point: JumpPoint) {
        self.points.push(point);
        if self.points.len() > Self::CAP {
            self.points.remove(0);
        }
    }

    /// Record `point` as the newest history entry. A repeat of the most recent
    /// point is ignored (dedup adjacent); otherwise any forward branch is
    /// discarded (a divergent jump abandons where "forward" would have gone).
    pub fn record(&mut self, point: JumpPoint) {
        // Drop the forward branch: everything from the current position on.
        self.points.truncate(self.index);
        if self.points.last() == Some(&point) {
            self.index = self.points.len();
            return;
        }
        self.push_capped(point);
        self.index = self.points.len();
    }

    /// Step back one entry, returning the location to move to, or `None` when
    /// already at the start. `current` is the live cursor location; on the
    /// first step back from the tip it is captured so [`forward`](Self::forward)
    /// can return to it.
    pub fn back(&mut self, current: JumpPoint) -> Option<JumpPoint> {
        if self.index == self.points.len() {
            // At the live tip: nothing recorded means nowhere to go back to.
            if self.points.is_empty() {
                return None;
            }
            if self.points.last() != Some(&current) {
                self.push_capped(current);
            }
            // `index` now points at the live location (the last entry).
            self.index = self.points.len() - 1;
        }
        if self.index == 0 {
            return None;
        }
        self.index -= 1;
        Some(self.points[self.index].clone())
    }

    /// Step forward one entry, returning the location to move to, or `None`
    /// when already at the tip.
    pub fn forward(&mut self) -> Option<JumpPoint> {
        if self.index + 1 >= self.points.len() {
            return None;
        }
        self.index += 1;
        Some(self.points[self.index].clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(name: &str, line: usize) -> JumpPoint {
        JumpPoint::new(PathBuf::from(name), line, 0)
    }

    #[test]
    fn record_dedups_adjacent_identical_points() {
        let mut j = JumpList::new();
        j.record(p("a", 1));
        j.record(p("a", 1));
        // Only one entry; stepping back from the tip captures the (same) live
        // location and finds the single prior point.
        assert_eq!(j.points.len(), 1);
    }

    #[test]
    fn back_then_forward_walks_the_trail() {
        // A(1) -> A(50) recorded as origins; live location is B(0).
        let mut j = JumpList::new();
        j.record(p("a", 1));
        j.record(p("a", 50));

        // Back from B returns the most recent origin, then the earlier one.
        assert_eq!(j.back(p("b", 0)), Some(p("a", 50)));
        assert_eq!(j.back(p("a", 50)), Some(p("a", 1)));
        // Already at the start.
        assert_eq!(j.back(p("a", 1)), None);

        // Forward re-advances toward B: A(50), then B, then nothing.
        assert_eq!(j.forward(), Some(p("a", 50)));
        assert_eq!(j.forward(), Some(p("b", 0)));
        assert_eq!(j.forward(), None);
    }

    #[test]
    fn new_jump_after_back_discards_forward_branch() {
        let mut j = JumpList::new();
        j.record(p("a", 1));
        j.record(p("a", 50));
        j.back(p("b", 0)); // now sitting on A(50), forward branch is [B]
        j.back(p("a", 50)); // now sitting on A(1)

        // A divergent jump truncates the forward branch.
        j.record(p("c", 7));
        assert_eq!(j.forward(), None);
    }

    #[test]
    fn back_and_forward_are_noops_on_empty_history() {
        let mut j = JumpList::new();
        assert_eq!(j.back(p("a", 1)), None);
        assert_eq!(j.forward(), None);
        // Empty history must not have been mutated into a phantom entry.
        assert!(j.points.is_empty());
    }

    #[test]
    fn cap_evicts_the_oldest_entries() {
        let mut j = JumpList::new();
        for i in 0..(JumpList::CAP + 10) {
            j.record(p("a", i));
        }
        assert_eq!(j.points.len(), JumpList::CAP);
        // The oldest surviving entry is the (10)th recorded, not the first.
        assert_eq!(j.points.first(), Some(&p("a", 10)));
    }

    #[test]
    fn forward_at_tip_is_a_noop() {
        let mut j = JumpList::new();
        j.record(p("a", 1));
        assert_eq!(j.forward(), None);
    }
}
