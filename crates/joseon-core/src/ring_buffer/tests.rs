//! Port source: `Tests/JoseonCoreTests/RingBufferTests.swift` and
//! `RingBufferRaceTests.swift`. The Swift tests run each case on two backends.
//! Rust has one backend, so each case runs once.

use super::*;
use std::sync::atomic::AtomicBool;
use std::sync::Mutex;
use std::time::{Duration, Instant};

fn counter(from: usize, count: usize) -> (Vec<f32>, Vec<f32>) {
    let left: Vec<f32> = (0..count).map(|i| (from + i) as f32).collect();
    let right = left.iter().map(|x| -x).collect();
    (left, right)
}

/// Counter signal: left is the frame index, right is its negation. A mix of
/// frames from two writes then shows as a jump in `left` and as a channel
/// disagreement.
fn write_counter(writer: &mut RingWriter, from: usize, count: usize) {
    let (left, right) = counter(from, count);
    writer.write(&left, &right, 48_000.0);
}

fn assert_intact(left: &[f32], right: &[f32], count: usize) {
    for i in 0..count {
        assert_eq!(right[i], -left[i], "channels disagree at {i}");
        if i > 0 {
            assert_eq!(left[i], left[i - 1] + 1.0, "torn at {i}");
        }
    }
}

#[test]
fn write_then_read() {
    let (mut writer, mut reader) = stereo_ring_buffer(1024);
    let (left, right) = counter(0, 300);
    writer.write(&left, &right, 44_100.0);
    assert_eq!(reader.available_frames(), 300);
    let (mut out_left, mut out_right) = (vec![0.0; 512], vec![0.0; 512]);
    assert_eq!(reader.read(&mut out_left, &mut out_right), 300);
    assert_eq!(out_left[299], 299.0);
    assert_eq!(out_right[299], -299.0);
    assert_eq!(reader.sample_rate(), 44_100.0);
}

#[test]
fn overrun_keeps_newest() {
    let (mut writer, mut reader) = stereo_ring_buffer(256);
    let (left, _) = counter(0, 1000);
    writer.write(&left, &left, 48_000.0);
    let (mut out_left, mut out_right) = (vec![0.0; 256], vec![0.0; 256]);
    assert_eq!(reader.read(&mut out_left, &mut out_right), 256);
    assert_eq!(out_left[255], 999.0);
    assert_eq!(out_left[0], 744.0);
}

/// Many small writes that together overrun the buffer must leave the newest
/// `capacity` frames, with no gap and no stale frame.
#[test]
fn overrun_keeps_newest_across_many_writes() {
    let (mut writer, mut reader) = stereo_ring_buffer(512);
    let mut written = 0;
    for _ in 0..40 {
        let (block, _) = counter(written, 100);
        writer.write(&block, &block, 48_000.0);
        written += 100;
    }
    assert_eq!(reader.available_frames(), 512);
    assert_eq!(writer.available_frames(), 512);
    let (mut out_left, mut out_right) = (vec![-1.0; 4000], vec![-1.0; 4000]);
    let n = reader.read(&mut out_left, &mut out_right);
    assert_eq!(n, 512);
    assert_eq!(out_left[0], (written - 512) as f32);
    assert_eq!(out_left[511], (written - 1) as f32);
    for i in 1..n {
        assert_eq!(out_left[i], out_left[i - 1] + 1.0, "gap at {i}");
    }
    assert_eq!(reader.available_frames(), 0);
}

/// One write larger than the buffer keeps the newest `capacity` frames of that write.
#[test]
fn single_write_larger_than_capacity() {
    let (mut writer, mut reader) = stereo_ring_buffer(128);
    write_counter(&mut writer, 0, 5000);
    let (mut out_left, mut out_right) = (vec![0.0; 128], vec![0.0; 128]);
    assert_eq!(reader.read(&mut out_left, &mut out_right), 128);
    assert_eq!(out_left[0], 4872.0);
    assert_eq!(out_left[127], 4999.0);
    assert_eq!(out_right[127], -4999.0);
}

/// Odd-sized writes and reads that wrap the storage many times must deliver an
/// unbroken counter signal on both channels.
#[test]
fn wrap_around_odd_sizes() {
    let (mut writer, mut reader) = stereo_ring_buffer(1024);
    let write_sizes = [37, 101, 7, 255, 3, 129, 61];
    let read_sizes = [13, 200, 1, 77, 333, 5, 91];
    let (mut written, mut read_back) = (0usize, 0usize);
    let (mut out_left, mut out_right) = (vec![0.0f32; 1024], vec![0.0f32; 1024]);

    for step in 0..600 {
        let count = write_sizes[step % write_sizes.len()];
        // Stay under capacity so nothing is dropped. This test checks wrap math, not overrun.
        if written - read_back + count <= 1024 {
            write_counter(&mut writer, written, count);
            written += count;
        }
        let want = read_sizes[step % read_sizes.len()];
        let n = reader.read(&mut out_left[..want], &mut out_right[..want]);
        assert!(n <= want);
        for i in 0..n {
            assert_eq!(out_left[i], (read_back + i) as f32, "step {step} index {i}");
            assert_eq!(out_right[i], -((read_back + i) as f32), "step {step} index {i} right");
        }
        read_back += n;
    }

    // Drain the tail and confirm the whole stream came through in order.
    loop {
        let n = reader.read(&mut out_left, &mut out_right);
        if n == 0 {
            break;
        }
        assert_eq!(&out_left[..n], counter(read_back, n).0.as_slice());
        read_back += n;
    }
    assert_eq!(read_back, written);
    assert!(written > 10_000);
}

#[test]
fn zero_capacity_becomes_one_frame() {
    let (mut writer, mut reader) = stereo_ring_buffer(0);
    assert_eq!(writer.capacity(), 1);
    writer.write(&[1.0, 2.0, 3.0], &[-1.0, -2.0, -3.0], 48_000.0);
    let (mut out_left, mut out_right) = ([0.0; 4], [0.0; 4]);
    assert_eq!(reader.read(&mut out_left, &mut out_right), 1);
    assert_eq!((out_left[0], out_right[0]), (3.0, -3.0));
}

#[test]
fn empty_write_and_empty_read_do_nothing() {
    let (mut writer, mut reader) = stereo_ring_buffer(64);
    writer.write(&[], &[], 96_000.0);
    assert_eq!(reader.available_frames(), 0);
    assert_eq!(
        reader.sample_rate(),
        48_000.0,
        "an empty write does not change the rate"
    );
    write_counter(&mut writer, 0, 10);
    assert_eq!(reader.read(&mut [], &mut []), 0);
    assert_eq!(reader.available_frames(), 10);
}

// MARK: Sample rate

#[test]
fn sample_rate_follows_mid_stream_change() {
    let (mut writer, mut reader) = stereo_ring_buffer(1024);
    let monitor = writer.monitor();
    assert_eq!(reader.sample_rate(), 48_000.0, "default before any write");

    let (a, _) = counter(0, 100);
    writer.write(&a, &a, 44_100.0);
    assert_eq!(
        reader.sample_rate(),
        44_100.0,
        "newest write rate before the first read"
    );
    assert_eq!(monitor.sample_rate(), 44_100.0);

    let (mut out_left, mut out_right) = (vec![0.0; 1024], vec![0.0; 1024]);
    assert_eq!(reader.read(&mut out_left, &mut out_right), 100);
    assert_eq!(reader.sample_rate(), 44_100.0);

    // The rate changes mid-stream: the frames of the new epoch come back under the new rate.
    let (b, _) = counter(100, 150);
    writer.write(&b, &b, 96_000.0);
    assert_eq!(monitor.available_frames(), 150);
    assert_eq!(reader.sample_rate(), 44_100.0, "still the rate of the last read");
    assert_eq!(reader.read(&mut out_left, &mut out_right), 150);
    assert_eq!(out_left[0], 100.0);
    assert_eq!(reader.sample_rate(), 96_000.0);
    assert_eq!(monitor.sample_rate(), 96_000.0);
}

/// Unread frames from before a rate change are dropped, not relabelled. One
/// read never mixes two rates.
#[test]
fn rate_change_drops_unread_old_epoch_frames() {
    let (mut writer, mut reader) = stereo_ring_buffer(1024);
    let (a, _) = counter(0, 100);
    writer.write(&a, &a, 44_100.0);
    let (b, _) = counter(1000, 100);
    writer.write(&b, &b, 48_000.0);

    assert_eq!(reader.available_frames(), 100, "only the new epoch is readable");
    let (mut out_left, mut out_right) = (vec![0.0; 1024], vec![0.0; 1024]);
    assert_eq!(reader.read(&mut out_left, &mut out_right), 100);
    assert_eq!(out_left[0], 1000.0);
    assert_eq!(out_left[99], 1099.0);
    assert_eq!(reader.sample_rate(), 48_000.0);
}

// MARK: The reader's overrun check
//
// These tests drive the exact interleaving that must never reach the caller:
// the writer laps the buffer *between the two channel copies of one `read`*.
// They use the reader-side hook, so the "writer" runs inline on the test
// thread and the interleaving happens on every run.
//
// Each of them fails on a `read` that loads `claim_end` before its copy.

const RACE_CAPACITY: usize = 1024;

fn race_ring() -> (Arc<Mutex<RingWriter>>, RingReader) {
    let (writer, reader) = stereo_ring_buffer(RACE_CAPACITY);
    (Arc::new(Mutex::new(writer)), reader)
}

/// Arms a one-shot lap that runs after the left channel of the first copy segment.
fn lap_once(reader: &mut RingReader, writer: &Arc<Mutex<RingWriter>>, from: usize, count: usize) {
    let writer = Arc::clone(writer);
    let mut fired = false;
    reader.hook_after_left_channel = Some(Box::new(move || {
        if !fired {
            fired = true;
            write_counter(&mut writer.lock().unwrap(), from, count);
        }
    }));
}

fn read_100(reader: &mut RingReader) -> (Vec<f32>, Vec<f32>, usize) {
    let (mut left, mut right) = (vec![f32::NAN; 1024], vec![f32::NAN; 1024]);
    let n = reader.read(&mut left[..100], &mut right[..100]);
    reader.hook_after_left_channel = None;
    (left, right, n)
}

/// `C` = 1024. The reader takes frames 0..100 out of slots 0..100. Between the
/// left copy and the right copy the writer appends 925 frames and reaches frame
/// 1024, the first frame that reuses slot 0. `read` must throw that copy away
/// and come back with a clean one.
///
/// `claim_end` is 1025 afterwards and `1025 - 0 > 1024`, so the copy is rejected.
#[test]
fn lap_between_channel_copies_is_rejected() {
    let (writer, mut reader) = race_ring();
    write_counter(&mut writer.lock().unwrap(), 0, 100);
    lap_once(&mut reader, &writer, 100, 925);

    let (left, right, n) = read_100(&mut reader);
    assert_eq!(n, 100);
    assert_intact(&left, &right, n);
    // The second attempt moves to the newest 1024 frames, which now start at frame 1.
    assert_eq!(left[0], 1.0, "did not resync after rejecting the lapped copy");
}

/// The same lap, but with a copy that wraps the storage. 1000 frames are
/// consumed first, so the copy starts at slot 1000 and splits into slots
/// 1000..1024 and 0..76. The lap reaches frame 2024, which reuses slot 1000.
#[test]
fn lap_between_wrapped_copy_segments_is_rejected() {
    let (writer, mut reader) = race_ring();
    write_counter(&mut writer.lock().unwrap(), 0, 1000);
    let (mut sink_left, mut sink_right) = (vec![0.0; 1000], vec![0.0; 1000]);
    assert_eq!(reader.read(&mut sink_left, &mut sink_right), 1000);
    write_counter(&mut writer.lock().unwrap(), 1000, 100);

    lap_once(&mut reader, &writer, 1100, 925);
    let (left, right, n) = read_100(&mut reader);
    assert_eq!(n, 100);
    assert_intact(&left, &right, n);
    assert_eq!(left[0], 1001.0, "did not resync after rejecting the lapped copy");
}

/// One write longer than the whole buffer claims `base + count` but stores only
/// the last `capacity` frames. The claim still bounds every slot it touches.
#[test]
fn lap_by_one_oversized_write_is_rejected() {
    let (writer, mut reader) = race_ring();
    write_counter(&mut writer.lock().unwrap(), 0, 100);
    lap_once(&mut reader, &writer, 100, 3000);

    let (left, right, n) = read_100(&mut reader);
    assert_eq!(n, 100);
    assert_intact(&left, &right, n);
    assert_eq!(left[0], 2076.0, "should hold the newest 1024 frames");
}

/// The boundary case on the safe side. The lap writes frames 100..1024, so
/// `claim_end` is exactly `start + capacity`. Nothing the copy took was
/// touched, and the copy must be accepted. A strict `<` would reject it.
#[test]
fn write_ending_exactly_at_the_capacity_boundary_is_accepted() {
    let (writer, mut reader) = race_ring();
    write_counter(&mut writer.lock().unwrap(), 0, 100);
    lap_once(&mut reader, &writer, 100, 924);

    let (left, right, n) = read_100(&mut reader);
    assert_eq!(n, 100);
    assert_intact(&left, &right, n);
    assert_eq!(left[0], 0.0, "an intact copy was rejected: the bound is too strict");
    assert_eq!(left[99], 99.0);
}

/// One frame further and slot 0 is gone. This is the first index the bound must reject.
#[test]
fn write_one_frame_past_the_capacity_boundary_is_rejected() {
    let (writer, mut reader) = race_ring();
    write_counter(&mut writer.lock().unwrap(), 0, 100);
    lap_once(&mut reader, &writer, 100, 925);

    let (left, right, n) = read_100(&mut reader);
    assert_eq!(n, 100);
    assert_intact(&left, &right, n);
    assert_ne!(left[0], 0.0, "a lapped copy was accepted");
}

/// Every attempt is lapped, so `read` gives up. It must report nothing, not
/// what the last rejected attempt left in the caller's buffers.
#[test]
fn every_attempt_lapped_returns_nothing() {
    let (writer, mut reader) = race_ring();
    write_counter(&mut writer.lock().unwrap(), 0, 100);
    let hook_writer = Arc::clone(&writer);
    let mut next = 100;
    reader.hook_after_left_channel = Some(Box::new(move || {
        write_counter(&mut hook_writer.lock().unwrap(), next, 2048);
        next += 2048;
    }));
    let (_, _, n) = read_100(&mut reader);
    assert_eq!(n, 0, "torn frames reported after every attempt was lapped");

    // The reader has moved to the newest frames, so the next read comes back clean.
    let (mut left, mut right) = (vec![f32::NAN; 512], vec![f32::NAN; 512]);
    let m = reader.read(&mut left, &mut right);
    assert_eq!(m, 0, "every published frame was skipped");
    write_counter(&mut writer.lock().unwrap(), 5000, 300);
    let m = reader.read(&mut left, &mut right);
    assert_eq!(m, 300);
    assert_intact(&left, &right, m);
    assert_eq!(left[0], 5000.0);
}

// MARK: Concurrency

#[derive(Default, Debug)]
struct StressResult {
    tears: usize,
    channel_mismatch: usize,
    frames: usize,
    reads: usize,
    overruns: usize,
}

/// One writer thread at 512-frame blocks and one reader that drains at random
/// sizes. `spinners` extra threads keep every core busy, so the reader and the
/// writer get descheduled in the middle of an operation.
///
/// Inside one `read` the counter signal must be contiguous (no tear). Across
/// reads it can jump (a reported overrun).
fn stress(capacity: usize, duration: Duration, spinners: usize) -> StressResult {
    // 1 << 20 is exact in f32, and 512 divides it, so a block never straddles
    // the wrap of the counter signal.
    const MODULUS: usize = 1 << 20;
    let (mut writer, mut reader) = stereo_ring_buffer(capacity);
    let stop = Arc::new(AtomicBool::new(false));
    let mut threads = Vec::new();

    for _ in 0..spinners {
        let stop = Arc::clone(&stop);
        threads.push(std::thread::spawn(move || {
            let mut x = 1.0f64;
            while !stop.load(Ordering::Relaxed) {
                for _ in 0..5000 {
                    x = std::hint::black_box(x * 1.000_000_1 + 1.0);
                }
            }
        }));
    }
    {
        let stop = Arc::clone(&stop);
        threads.push(std::thread::spawn(move || {
            let mut position = 0usize;
            let (mut left, mut right) = ([0.0f32; 512], [0.0f32; 512]);
            while !stop.load(Ordering::Relaxed) {
                for i in 0..512 {
                    left[i] = ((position + i) % MODULUS) as f32;
                    right[i] = -left[i];
                }
                writer.write(&left, &right, 48_000.0);
                position += 512;
            }
        }));
    }

    let mut result = StressResult::default();
    let (mut left, mut right) = (vec![0.0f32; 4096], vec![0.0f32; 4096]);
    let mut previous_last: Option<f32> = None;
    let mut random = 0x4A6F_7365_6F6E_u64;
    let deadline = Instant::now() + duration;
    while Instant::now() < deadline {
        random = random
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let want = 1 + (random >> 33) as usize % 4096;
        let n = reader.read(&mut left[..want], &mut right[..want]);
        result.reads += 1;
        if n == 0 {
            continue;
        }
        result.frames += n;
        let next = |x: f32| ((x as usize + 1) % MODULUS) as f32;
        for i in 0..n {
            if right[i] != -left[i] {
                result.channel_mismatch += 1;
            }
            if i > 0 && left[i] != next(left[i - 1]) {
                result.tears += 1;
            }
        }
        if previous_last.is_some_and(|previous| left[0] != next(previous)) {
            result.overruns += 1;
        }
        previous_last = Some(left[n - 1]);
    }
    stop.store(true, Ordering::Relaxed);
    for thread in threads {
        thread.join().expect("a stress thread panicked");
    }
    result
}

#[test]
fn concurrent_writer_reader_no_tears() {
    let result = stress(1 << 13, Duration::from_secs(2), 0);
    println!("[ring stress] {result:?}");
    assert_eq!(result.tears, 0, "torn data inside a read call");
    assert_eq!(result.channel_mismatch, 0, "left and right channels disagree");
    assert!(result.frames > 100_000, "the test did not exercise the buffer");
}

/// The stress test of record: a small buffer and every core busy.
#[test]
fn concurrent_read_write_under_cpu_load() {
    let cores = std::thread::available_parallelism().map_or(4, |n| n.get());
    let result = stress(1 << 12, Duration::from_millis(1500), cores);
    println!("[ring race] {result:?}");
    assert_eq!(result.tears, 0, "torn data inside a read call");
    assert_eq!(result.channel_mismatch, 0, "left and right channels disagree");
    // A shared runner gives the reader less time: `JOSEON_TIMING_SLACK` is a factor on the count (1 = none).
    let slack: f64 = std::env::var("JOSEON_TIMING_SLACK")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1.0);
    let least = (50_000.0 / slack.max(1.0)) as usize;
    assert!(
        result.frames > least,
        "the test did not exercise the buffer: {} frames",
        result.frames
    );
}

#[test]
fn both_ends_move_to_other_threads() {
    fn assert_send<T: Send>() {}
    assert_send::<RingWriter>();
    assert_send::<RingReader>();
    assert_send::<RingMonitor>();
}
