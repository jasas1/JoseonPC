//! Times the spectrum analyzer on the demo signal, the way the engine drives
//! it: 800 frames, then a read. Run it in release:
//!
//! ```text
//! cargo run --release -p joseon-core --example spectrum_bench -- 20
//! ```
//!
//! The argument is the number of passes over 12 seconds of audio.

use joseon_core::contracts::SpectrumAnalyzing;
use joseon_core::spectrum::SpectrumAnalyzer;
use joseon_core::test_signals::demo_block;
use std::time::{Duration, Instant};

fn main() {
    let passes: usize = std::env::args().nth(1).and_then(|text| text.parse().ok()).unwrap_or(5);
    let (rate, block) = (48_000.0, 800);
    let (left, right) = demo_block(0, 48_000 * 12, rate);
    let mut analyzer = SpectrumAnalyzer::default();
    let (mut process_time, mut read_time, mut calls) = (Duration::ZERO, Duration::ZERO, 0u32);
    for pass in 0..=passes {
        for (l, r) in left.chunks_exact(block).zip(right.chunks_exact(block)) {
            let t0 = Instant::now();
            analyzer.process(l, r, rate);
            let t1 = Instant::now();
            std::hint::black_box(analyzer.read());
            let t2 = Instant::now();
            // The first pass fills the history and takes every first-use path.
            if pass > 0 {
                process_time += t1 - t0;
                read_time += t2 - t1;
                calls += 1;
            }
        }
    }
    let micros = |time: Duration| time.as_secs_f64() * 1e6 / calls.max(1) as f64;
    println!(
        "process {:.1} us, read {:.1} us, average over {calls} blocks of 800 frames",
        micros(process_time),
        micros(read_time)
    );
}
