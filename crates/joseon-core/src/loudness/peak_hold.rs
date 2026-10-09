//! Display ballistics of one peak meter channel: hold the highest level of the
//! last `hold_seconds`, then fall at `fall_db_per_second`.
//!
//! Port source: `Sources/JoseonCore/Loudness/PeakHoldBallistics.swift`.
//!
//! ## Why a sliding maximum and not a hold timer
//!
//! The obvious form latches the display on a new peak, starts a timer, and
//! falls when the timer runs out. It restarts the timer only when the input
//! *exceeds* what is displayed. On repeating programme material that never
//! happens: a kick drum hits the same level every half second, but each hit
//! lands a hair below the loudest hit so far. So the timer runs on, the display
//! falls 20 dB/s towards the material between the hits, and it re-latches only
//! once it has fallen below the next hit. The display then saw-tooths, and two
//! channels fed the same mono kick saw-tooth on different phases.
//!
//! A sliding maximum has no such state: the held value is the largest level in
//! the last `hold_seconds` whatever the order of arrival. A repeating peak
//! holds the meter up, and two channels with the same peak read the same number.
//!
//! ## Cost and accuracy
//!
//! The window is `SLOTS` buckets of `hold_seconds / (SLOTS - 1)`, so it covers
//! between `hold_seconds` and `hold_seconds · SLOTS / (SLOTS - 1)`: at 16
//! slots, 1.5 s to 1.6 s of hold. Every call is a fixed number of comparisons
//! and no allocation.

/// Buckets of the sliding maximum. More slots means a tighter hold window.
pub(crate) const SLOTS: usize = 16;

#[derive(Debug, Clone)]
pub(crate) struct PeakHoldBallistics {
    floor_db: f64,
    fall_db_per_second: f64,
    slot_seconds: f64,
    /// Ring of bucket maxima. `write` is the newest bucket.
    bucket: [f64; SLOTS],
    write: usize,
    slot_age: f64,
    /// What the meter shows, in dB.
    display: f64,
}

impl PeakHoldBallistics {
    pub fn new(hold_seconds: f64, fall_db_per_second: f64, floor_db: f64) -> Self {
        Self {
            floor_db,
            fall_db_per_second,
            slot_seconds: hold_seconds / (SLOTS - 1) as f64,
            bucket: [floor_db; SLOTS],
            write: 0,
            slot_age: 0.0,
            display: floor_db,
        }
    }

    pub fn display(&self) -> f64 {
        self.display
    }

    pub fn clear(&mut self) {
        self.bucket = [self.floor_db; SLOTS];
        self.write = 0;
        self.slot_age = 0.0;
        self.display = self.floor_db;
    }

    /// Feeds the peak level of one chunk, in dB, and how long that chunk lasted.
    pub fn update(&mut self, level: f64, dt: f64) {
        self.slot_age += dt;
        if self.slot_age >= self.slot_seconds {
            let steps = ((self.slot_age / self.slot_seconds) as usize).min(SLOTS);
            for _ in 0..steps {
                self.write += 1;
                if self.write == SLOTS {
                    self.write = 0;
                }
                self.bucket[self.write] = self.floor_db;
            }
            self.slot_age -= steps as f64 * self.slot_seconds;
            // A single chunk longer than the whole window has just cleared every bucket.
            if self.slot_age >= self.slot_seconds {
                self.slot_age = 0.0;
            }
        }
        if level > self.bucket[self.write] {
            self.bucket[self.write] = level;
        }

        let mut held = self.floor_db;
        for &value in &self.bucket {
            if value > held {
                held = value;
            }
        }

        if held >= self.display {
            self.display = held;
        } else {
            self.display = held.max(self.display - self.fall_db_per_second * dt);
        }
    }
}
