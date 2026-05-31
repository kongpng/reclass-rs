//! `struct ValueHistory` (`core.h:867-923`) — the per-field heatmap ring buffer.
//!
//! Faithful port. `QDateTime::currentMSecsSinceEpoch()` becomes a `now_ms()`
//! free function (system clock, ms since epoch) so the data model stays pure
//! and testable.

pub const K_CAPACITY: usize = 10;

fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[derive(Clone, Debug)]
pub struct ValueHistory {
    pub values: [String; K_CAPACITY],
    pub timestamps: [i64; K_CAPACITY],
    /// total values recorded (NOT capped — heat keys on this).
    pub count: i32,
    /// next write position in the ring.
    pub head: i32,
}

impl Default for ValueHistory {
    fn default() -> Self {
        ValueHistory {
            values: Default::default(),
            timestamps: [0; K_CAPACITY],
            count: 0,
            head: 0,
        }
    }
}

impl ValueHistory {
    pub fn new() -> Self {
        Self::default()
    }

    /// `record()` (`core.h:874-883`) — dedups *consecutive* identical values.
    pub fn record(&mut self, v: &str) {
        let cap = K_CAPACITY as i32;
        if self.count > 0 {
            let last = ((self.head + cap - 1) % cap) as usize;
            if self.values[last] == v {
                return; // no change
            }
        }
        let h = self.head as usize;
        self.values[h] = v.to_string();
        self.timestamps[h] = now_ms();
        self.head = (self.head + 1) % cap;
        if self.count < i32::MAX {
            self.count += 1;
        }
    }

    /// `clear()` (`core.h:885-888`) — resets counters; does not wipe contents.
    pub fn clear(&mut self) {
        self.count = 0;
        self.head = 0;
    }

    pub fn unique_count(&self) -> i32 {
        self.count.min(K_CAPACITY as i32)
    }

    /// `heatLevel()` (`core.h:893-898`) — keyed on total `count`, not unique.
    pub fn heat_level(&self) -> i32 {
        if self.count <= 1 {
            0
        } else if self.count == 2 {
            1
        } else if self.count <= 4 {
            2
        } else {
            3
        }
    }

    pub fn last(&self) -> &str {
        if self.count == 0 {
            ""
        } else {
            let cap = K_CAPACITY as i32;
            &self.values[((self.head + cap - 1) % cap) as usize]
        }
    }

    /// `forEach()` (`core.h:906-912`) — oldest → newest.
    pub fn for_each<F: FnMut(&str)>(&self, mut f: F) {
        let cap = K_CAPACITY as i32;
        let n = self.unique_count();
        let start = (self.head + cap - n) % cap;
        for i in 0..n {
            f(&self.values[((start + i) % cap) as usize]);
        }
    }

    /// `forEachWithTime()` (`core.h:915-922`) — newest → oldest.
    pub fn for_each_with_time<F: FnMut(&str, i64)>(&self, mut f: F) {
        let cap = K_CAPACITY as i32;
        let n = self.unique_count();
        for i in 0..n {
            let idx = ((self.head + cap - 1 - i) % cap) as usize;
            f(&self.values[idx], self.timestamps[idx]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_and_single() {
        let mut h = ValueHistory::new();
        assert_eq!(h.heat_level(), 0);
        assert_eq!(h.last(), "");
        h.record("a");
        assert_eq!(h.heat_level(), 0);
        assert_eq!(h.last(), "a");
    }

    #[test]
    fn consecutive_dedup_and_heat() {
        let mut h = ValueHistory::new();
        h.record("a");
        h.record("a"); // deduped
        assert_eq!(h.count, 1);
        h.record("b");
        assert_eq!(h.heat_level(), 1); // count 2
        h.record("c");
        assert_eq!(h.heat_level(), 2); // count 3
        h.record("d");
        h.record("e");
        assert_eq!(h.heat_level(), 3); // count 5
    }

    #[test]
    fn ring_wraps() {
        let mut h = ValueHistory::new();
        for i in 0..15 {
            h.record(&i.to_string());
        }
        assert_eq!(h.count, 15);
        assert_eq!(h.unique_count(), 10);
        assert_eq!(h.last(), "14");
        // oldest surviving is "5"
        let mut first = String::new();
        h.for_each(|v| {
            if first.is_empty() {
                first = v.to_string();
            }
        });
        assert_eq!(first, "5");
    }

    #[test]
    fn oscillation_counts_each() {
        let mut h = ValueHistory::new();
        for v in ["a", "b", "a", "b"] {
            h.record(v);
        }
        assert_eq!(h.count, 4);
        assert_eq!(h.heat_level(), 2); // warm
    }
}
