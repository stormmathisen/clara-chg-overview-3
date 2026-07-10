//! Client-side rolling point buffer with snapshot/delta application.
//!
//! Lives in `shared` (rather than the frontend) so the dedup logic — the subtle part
//! of the delta protocol — is unit-tested here; the frontend, being a wasm-only
//! target, can't run these tests itself.

use std::collections::VecDeque;

/// A rolling buffer of `[timestamp, value]` points kept in sync with the server via
/// full snapshots (`set_snapshot`) and incremental appends (`apply_delta`), using a
/// monotonic `cursor` (the server's `total_pushed`) to de-duplicate overlap.
#[derive(Default, Clone, Debug)]
pub struct PointBuffer {
    points: VecDeque<[f64; 2]>,
    cursor: u64,
}

impl PointBuffer {
    pub fn new() -> Self {
        Self::default()
    }

    /// The buffered points, oldest first.
    pub fn points(&self) -> &VecDeque<[f64; 2]> {
        &self.points
    }

    /// The append cursor: total samples applied so far.
    pub fn cursor(&self) -> u64 {
        self.cursor
    }

    /// Replace the buffer wholesale from a full snapshot.
    pub fn set_snapshot(&mut self, points: Vec<[f64; 2]>, cursor: u64, cap: usize) {
        self.points = points.into_iter().collect();
        self.trim(cap);
        self.cursor = cursor;
    }

    /// Append the points from a delta, skipping any already applied. `cursor` is the
    /// server's `total_pushed` after the delta's points; `new_points` are the points
    /// ending at that cursor.
    pub fn apply_delta(&mut self, new_points: Vec<[f64; 2]>, cursor: u64, cap: usize) {
        // Entirely older than what we already have (e.g. the delta that straddles a
        // client's connect snapshot): ignore.
        if cursor <= self.cursor {
            return;
        }
        // Cursor value just before the first point in `new_points`.
        let start = cursor.saturating_sub(new_points.len() as u64);
        // How many leading points we already have (overlap to skip).
        let skip = (self.cursor.saturating_sub(start) as usize).min(new_points.len());
        for p in new_points.into_iter().skip(skip) {
            if self.points.len() >= cap {
                self.points.pop_front();
            }
            self.points.push_back(p);
        }
        self.cursor = cursor;
    }

    /// Shrink to a new capacity, dropping the oldest points.
    pub fn set_capacity(&mut self, cap: usize) {
        self.trim(cap);
    }

    fn trim(&mut self, cap: usize) {
        while self.points.len() > cap {
            self.points.pop_front();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pt(v: f64) -> [f64; 2] {
        [v, v * 10.0]
    }

    #[test]
    fn snapshot_then_contiguous_deltas() {
        let mut b = PointBuffer::new();
        b.set_snapshot(vec![pt(1.0), pt(2.0)], 2, 100);
        assert_eq!(b.cursor(), 2);
        b.apply_delta(vec![pt(3.0)], 3, 100);
        b.apply_delta(vec![pt(4.0)], 4, 100);
        assert_eq!(b.cursor(), 4);
        let got: Vec<_> = b.points().iter().copied().collect();
        assert_eq!(got, vec![pt(1.0), pt(2.0), pt(3.0), pt(4.0)]);
    }

    #[test]
    fn straddling_delta_is_deduplicated() {
        // Client snapshot covers points up to cursor 5. A delta produced during the
        // handshake covers cursors 3..=6 (overlapping 4,5 already applied).
        let mut b = PointBuffer::new();
        b.set_snapshot(vec![pt(3.0), pt(4.0), pt(5.0)], 5, 100);
        // Delta new_points end at cursor 6, containing points for cursors 4,5,6.
        b.apply_delta(vec![pt(4.0), pt(5.0), pt(6.0)], 6, 100);
        let got: Vec<_> = b.points().iter().copied().collect();
        // Only pt(6) is genuinely new; 4 and 5 must not be duplicated.
        assert_eq!(got, vec![pt(3.0), pt(4.0), pt(5.0), pt(6.0)]);
        assert_eq!(b.cursor(), 6);
    }

    #[test]
    fn fully_old_delta_ignored() {
        let mut b = PointBuffer::new();
        b.set_snapshot(vec![pt(1.0), pt(2.0), pt(3.0)], 3, 100);
        b.apply_delta(vec![pt(2.0)], 2, 100); // cursor 2 <= 3
        let got: Vec<_> = b.points().iter().copied().collect();
        assert_eq!(got, vec![pt(1.0), pt(2.0), pt(3.0)]);
        assert_eq!(b.cursor(), 3);
    }

    #[test]
    fn appends_respect_capacity() {
        let mut b = PointBuffer::new();
        b.set_snapshot(vec![pt(1.0), pt(2.0), pt(3.0)], 3, 3);
        b.apply_delta(vec![pt(4.0)], 4, 3);
        let got: Vec<_> = b.points().iter().copied().collect();
        assert_eq!(got, vec![pt(2.0), pt(3.0), pt(4.0)]);
    }

    #[test]
    fn gap_delta_appends_without_panicking() {
        // Client at cursor 2; a delta arrives starting at cursor 5 (missed 3,4).
        let mut b = PointBuffer::new();
        b.set_snapshot(vec![pt(1.0), pt(2.0)], 2, 100);
        b.apply_delta(vec![pt(6.0)], 6, 100); // start = 5 > cursor 2 (gap)
        let got: Vec<_> = b.points().iter().copied().collect();
        assert_eq!(got, vec![pt(1.0), pt(2.0), pt(6.0)]);
        assert_eq!(b.cursor(), 6);
    }

    #[test]
    fn set_capacity_trims_oldest() {
        let mut b = PointBuffer::new();
        b.set_snapshot(vec![pt(1.0), pt(2.0), pt(3.0), pt(4.0)], 4, 100);
        b.set_capacity(2);
        let got: Vec<_> = b.points().iter().copied().collect();
        assert_eq!(got, vec![pt(3.0), pt(4.0)]);
    }
}
