//! Shared by the parity tests: reads a golden file and keeps score.
#![allow(dead_code)]

use std::collections::HashMap;

/// The lines of `tests/golden/<name>`, keyed by their first two words.
pub fn golden(name: &str) -> HashMap<String, String> {
    let path = format!("{}/tests/golden/{name}", env!("CARGO_MANIFEST_DIR"));
    let text = std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("{path}: {error}"));
    text.lines()
        .map(|line| {
            let mut parts = line.splitn(3, ' ');
            let key = format!("{} {}", parts.next().unwrap(), parts.next().unwrap());
            (key, parts.next().unwrap_or("").to_string())
        })
        .collect()
}

pub fn floats(text: &str) -> Vec<f32> {
    text.split_whitespace().map(|v| v.parse().expect("a float")).collect()
}

/// FNV-1a over the bit patterns, as `tools/golden` computes it.
pub fn fnv(values: impl IntoIterator<Item = f32>) -> String {
    let hash = values.into_iter().fold(0xcbf2_9ce4_8422_2325u64, |h, v| {
        (h ^ v.to_bits() as u64).wrapping_mul(0x0100_0000_01b3)
    });
    format!("{hash:x}")
}

/// Collects differences from the macOS app.
#[derive(Default)]
pub struct Score {
    pub compared: usize,
    pub worst: f32,
    pub worst_at: String,
    pub worst_relative: f32,
    pub worst_relative_at: String,
    pub failures: Vec<String>,
}

impl Score {
    /// Compares number lists. `tolerance` is the largest difference allowed in one value.
    pub fn numbers(&mut self, key: &str, ours: &[f32], theirs: &[f32], tolerance: f32) {
        if ours.len() != theirs.len() {
            self.failures.push(format!(
                "{key}: {} values, the macOS app has {}",
                ours.len(),
                theirs.len()
            ));
            return;
        }
        for (i, (a, b)) in ours.iter().zip(theirs).enumerate() {
            let difference = (a - b).abs();
            self.compared += 1;
            if difference > self.worst {
                (self.worst, self.worst_at) = (difference, format!("{key}[{i}]"));
            }
            if !(difference <= tolerance) {
                self.failures.push(format!("{key}[{i}]: {a} vs {b}"));
            }
        }
    }

    /// Compares two texts that must be the same.
    pub fn same(&mut self, key: &str, ours: &str, theirs: &str) {
        self.compared += 1;
        if ours != theirs {
            self.failures.push(format!("{key}: {ours} vs {theirs}"));
        }
    }

    pub fn finish(self) {
        println!(
            "{} values compared. Largest difference from the macOS app: {} at {}",
            self.compared, self.worst, self.worst_at
        );
        if self.worst_relative > 0.0 {
            println!(
                "Largest relative difference in the f64 values: {} at {}",
                self.worst_relative, self.worst_relative_at
            );
        }
        assert!(
            self.failures.is_empty(),
            "not the same as the macOS app:\n{}",
            self.failures.join("\n")
        );
    }
}

/// A loud, clipped, bright signal with a 30 Hz rumble. After `quiet_after` seconds it is nearly silent.
pub fn loud_signal(seconds: f64, quiet_after: f64) -> (Vec<f32>, Vec<f32>) {
    use joseon_core::test_signals::{pink_noise, sine, white_noise};
    let rate = 48_000.0;
    let n = (rate * seconds) as usize;
    let noise = pink_noise(0.9, n, 41);
    let hiss = white_noise(0.3, n, 42);
    let rumble = sine(30.0, 0.6, rate, seconds, 0.0);
    let (mut left, mut right) = (vec![0.0f32; n], vec![0.0f32; n]);
    for i in 0..n {
        let quiet = if i > (rate * quiet_after) as usize { 0.001 } else { 1.0 };
        left[i] = ((noise[i] + hiss[i] + rumble[i]) * 1.6).clamp(-1.0, 1.0) * quiet;
        right[i] = ((noise[i] - hiss[i] + rumble[i]) * 1.6).clamp(-1.0, 1.0) * quiet;
    }
    (left, right)
}

pub fn doubles(text: &str) -> Vec<f64> {
    text.split_whitespace().map(|v| v.parse().expect("a number")).collect()
}

impl Score {
    /// Compares `f64` lists. `relative` is the largest difference allowed, as a
    /// part of the larger value (and as an absolute difference near zero).
    pub fn doubles(&mut self, key: &str, ours: &[f64], theirs: &[f64], relative: f64) {
        if ours.len() != theirs.len() {
            self.failures.push(format!(
                "{key}: {} values, the macOS app has {}",
                ours.len(),
                theirs.len()
            ));
            return;
        }
        for (i, (a, b)) in ours.iter().zip(theirs).enumerate() {
            self.compared += 1;
            if a == b {
                continue; // also covers two infinities
            }
            let difference = (a - b).abs() / a.abs().max(b.abs()).max(1.0);
            if difference as f32 > self.worst_relative {
                (self.worst_relative, self.worst_relative_at) = (difference as f32, format!("{key}[{i}]"));
            }
            if !(difference <= relative) {
                self.failures.push(format!("{key}[{i}]: {a} vs {b}"));
            }
        }
    }
}

/// A golden line by its whole key, for keys with more than two words or with
/// brackets: the text after `key`. Use `Lines::of` to read the file once.
pub struct Lines(String);

impl Lines {
    pub fn of(name: &str) -> Self {
        let path = format!("{}/tests/golden/{name}", env!("CARGO_MANIFEST_DIR"));
        Self(std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("{path}: {error}")))
    }

    pub fn get(&self, key: &str) -> &str {
        let start = format!("{key} ");
        self.0
            .lines()
            .find_map(|line| line.strip_prefix(&start).or_else(|| (line == key).then_some("")))
            .unwrap_or_else(|| panic!("no golden line {key}"))
    }

    /// The lines `key count`, `key 0`, `key 1`, ...
    pub fn texts(&self, key: &str) -> Vec<&str> {
        let count: usize = self.get(&format!("{key} count")).parse().unwrap();
        (0..count).map(|i| self.get(&format!("{key} {i}"))).collect()
    }
}

impl Score {
    /// Compares lists of texts that must be the same.
    pub fn texts(&mut self, key: &str, ours: &[String], theirs: &[&str]) {
        self.same(
            &format!("{key} count"),
            &ours.len().to_string(),
            &theirs.len().to_string(),
        );
        for (i, (a, b)) in ours.iter().zip(theirs).enumerate() {
            self.same(&format!("{key} {i}"), a, b);
        }
    }
}
