//! Single-writer, single-reader stereo float ring buffer.
//!
//! The capture thread writes. The analysis thread reads. Nothing allocates
//! after `stereo_ring_buffer` returns.
//!
//! Port source: `Sources/JoseonCore/StereoRingBuffer.swift`. The algorithm is
//! the same. Three things differ, all because of Rust:
//!
//! - The buffer is two handles, [`RingWriter`] and [`RingReader`]. The type
//!   system then enforces "one writer, one reader". The Swift class documents
//!   that rule and trusts the caller.
//! - Samples are stored as `AtomicU32` bit patterns and moved with relaxed
//!   loads and stores. The reader can read a slot while the writer overwrites
//!   it. With plain `f32` storage that is a data race, which Rust does not
//!   allow. A relaxed atomic load is the same machine instruction as a plain
//!   load, so the cost is only that the copy loops do not become `memcpy`.
//! - There is one backend. The Swift lock fallback exists for macOS 14, which
//!   has no atomics library. Rust has atomics on every target.
//!
//! ## Writer guarantee
//!
//! `write` is wait-free. It does one atomic exchange, the sample copy and
//! three atomic stores. It takes no lock and never waits for the reader.
//!
//! ## Reader
//!
//! The reader copies samples, then checks that the writer did not lap the
//! region it copied. On overrun it moves to the newest `capacity` frames and
//! copies again. Samples from a lapped copy are never returned. Returned frames
//! are always one contiguous run with no tear.
//!
//! ## Sample rate
//!
//! `sample_rate` describes exactly the frames the last successful `read`
//! returned. Before the first read it is the rate of the newest write. A rate
//! change starts a new epoch: unread frames at the old rate are dropped, so one
//! `read` never mixes rates.

use std::sync::atomic::{fence, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;

/// Default capacity in frames. The same value as the macOS app.
pub const DEFAULT_CAPACITY: usize = 1 << 17;

const DEFAULT_SAMPLE_RATE: f64 = 48_000.0;

/// How many times `read` copies again after the writer lapped a copy, before it
/// gives up for this call.
const MAX_READ_ATTEMPTS: usize = 8;

/// One consistent view of the state the writer published.
#[derive(Clone, Copy)]
struct Snapshot {
    /// Total frames fully written and published.
    write_index: u64,
    /// Frame index where the current sample rate began.
    epoch_start: u64,
    /// `f64::to_bits` of the current sample rate.
    rate_bits: u64,
}

/// The storage and the index words that the writer and the reader share.
///
/// Word roles:
/// - `claim_end`: published *before* the writer touches the sample storage. It
///   marks the end of the region the writer may be overwriting now. The reader
///   uses it to check a finished copy.
/// - `write_index`: published *after* the copy. Frames below it are complete.
/// - `epoch_start` / `rate_bits`: the current sample rate and the frame index
///   where it started.
/// - `read_index` / `read_rate_bits`: published by the reader after each read.
struct Shared {
    capacity: u64,
    left: Box<[AtomicU32]>,
    right: Box<[AtomicU32]>,
    write_index: AtomicU64,
    claim_end: AtomicU64,
    epoch_start: AtomicU64,
    rate_bits: AtomicU64,
    read_index: AtomicU64,
    read_rate_bits: AtomicU64,
}

impl Shared {
    /// Writer, before the sample copy.
    fn claim(&self, end: u64) {
        // An acquire+release exchange, not a plain store. The release half keeps
        // earlier work from moving below it. The acquire half keeps the sample
        // stores that follow from moving above it.
        self.claim_end.swap(end, Ordering::AcqRel);
    }

    /// Writer, after the sample copy.
    fn publish(&self, write_index: u64, epoch_start: u64, rate_bits: u64) {
        // The order of the first two stores is part of the proof in `snapshot`:
        // the epoch start goes first and the rate is released, so a reader that
        // sees a new rate also sees the new epoch start.
        self.epoch_start.store(epoch_start, Ordering::Relaxed);
        self.rate_bits.store(rate_bits, Ordering::Release);
        // A reader that acquires this value also sees the samples and the epoch words.
        self.write_index.store(write_index, Ordering::Release);
    }

    fn snapshot(&self) -> Snapshot {
        // Seqlock with `write_index` as the sequence counter. It grows on every
        // publish, so the same value before and after the epoch words means no
        // publish *completed* between them.
        //
        // A publish that is half done is still visible, so the epoch words are
        // read in the reverse of the order `publish` writes them: rate first,
        // epoch start second. `publish` releases the rate after it stores the
        // epoch start, so a new rate here also makes the new epoch start
        // visible. That rules out the one skewed pair that matters: an old
        // epoch start with a new rate, which would label old frames with the
        // new rate. The other skewed pair, a new epoch start with the old rate,
        // is harmless. A new epoch starts at the writer's `base`, which is the
        // `write_index` held here, so it gives no readable frames and the next
        // call sees the finished publish.
        let mut w1 = 0;
        let mut rate_bits = 0;
        for _ in 0..64 {
            w1 = self.write_index.load(Ordering::Acquire);
            rate_bits = self.rate_bits.load(Ordering::Acquire);
            let epoch_start = self.epoch_start.load(Ordering::Acquire);
            if self.write_index.load(Ordering::Acquire) == w1 {
                return Snapshot {
                    write_index: w1,
                    epoch_start,
                    rate_bits,
                };
            }
        }
        // Not reachable in practice. Report an empty epoch, so the caller reads
        // zero frames and never labels frames with the wrong rate.
        Snapshot {
            write_index: w1,
            epoch_start: w1,
            rate_bits,
        }
    }

    fn commit_read(&self, read_index: u64, rate_bits: u64) {
        self.read_rate_bits.store(rate_bits, Ordering::Relaxed);
        self.read_index.store(read_index, Ordering::Release);
    }

    /// First frame index the reader may take: not consumed, not before the
    /// current rate epoch, and not older than the newest `capacity` frames.
    fn read_start(&self, s: Snapshot, consumed: u64) -> u64 {
        let mut start = consumed.max(s.epoch_start);
        if s.write_index.saturating_sub(start) > self.capacity {
            start = s.write_index - self.capacity;
        }
        start.min(s.write_index)
    }

    fn available_frames(&self) -> usize {
        let s = self.snapshot();
        let start = self.read_start(s, self.read_index.load(Ordering::Acquire));
        s.write_index.saturating_sub(start) as usize
    }

    fn sample_rate(&self) -> f64 {
        let bits = self.read_rate_bits.load(Ordering::Acquire);
        if bits != 0 {
            return f64::from_bits(bits);
        }
        f64::from_bits(self.snapshot().rate_bits)
    }
}

/// Orders every load of the reader's sample copy *before* the `claim_end` load
/// that checks that copy.
///
/// Without it the check is not sound. The writer side is ordered: `claim` is an
/// acquire+release exchange, so the samples it stores afterwards cannot become
/// visible before the claim. The reader side copies the samples and then loads
/// `claim_end`. An acquire load orders the accesses that come *after* it, not
/// the ones before it. On a weakly ordered CPU (ARM) the load can complete
/// while the copy's loads are still in flight. The reader could then pair
/// samples from write *k* with a `claim_end` from before write *k*, decide that
/// nothing lapped its region, and return torn frames.
///
/// On AArch64 this is `dmb ishld`. On x86 loads are already ordered and this
/// costs nothing.
#[inline(always)]
fn reader_copy_fence() {
    fence(Ordering::Acquire);
}

/// Makes a ring buffer of `capacity` frames (at least 1) and returns its two ends.
pub fn stereo_ring_buffer(capacity: usize) -> (RingWriter, RingReader) {
    let capacity = capacity.max(1);
    let storage = || (0..capacity).map(|_| AtomicU32::new(0)).collect::<Box<[AtomicU32]>>();
    let rate_bits = DEFAULT_SAMPLE_RATE.to_bits();
    let shared = Arc::new(Shared {
        capacity: capacity as u64,
        left: storage(),
        right: storage(),
        write_index: AtomicU64::new(0),
        claim_end: AtomicU64::new(0),
        epoch_start: AtomicU64::new(0),
        rate_bits: AtomicU64::new(rate_bits),
        read_index: AtomicU64::new(0),
        read_rate_bits: AtomicU64::new(0),
    });
    let writer = RingWriter {
        shared: Arc::clone(&shared),
        write_index: 0,
        epoch_start: 0,
        rate_bits,
    };
    let reader = RingReader {
        shared,
        read_index: 0,
        #[cfg(test)]
        hook_after_left_channel: None,
    };
    (writer, reader)
}

/// The writing end. Move it to the capture thread.
pub struct RingWriter {
    shared: Arc<Shared>,
    write_index: u64,
    epoch_start: u64,
    rate_bits: u64,
}

impl RingWriter {
    pub fn capacity(&self) -> usize {
        self.shared.capacity as usize
    }

    /// Writes `min(left.len(), right.len())` frames. Old unread frames are
    /// overwritten when the reader is slow.
    ///
    /// Real-time safe: no allocation, no lock, no wait.
    pub fn write(&mut self, left: &[f32], right: &[f32], sample_rate: f64) {
        let count = left.len().min(right.len());
        if count == 0 {
            return;
        }
        let capacity = self.shared.capacity as usize;
        let rate_bits = sample_rate.to_bits();
        // A write longer than the buffer stores only its newest `capacity` frames.
        let skipped = count.saturating_sub(capacity);
        let base = self.write_index;
        let end = base + count as u64;

        // Tell the reader which frames can be overwritten before any sample changes.
        self.shared.claim(end);

        let mut position = ((base + skipped as u64) % self.shared.capacity) as usize;
        let mut offset = skipped;
        while offset < count {
            let chunk = (count - offset).min(capacity - position);
            let slots = position..position + chunk;
            let source = offset..offset + chunk;
            for (slot, sample) in self.shared.left[slots.clone()].iter().zip(&left[source.clone()]) {
                slot.store(sample.to_bits(), Ordering::Relaxed);
            }
            for (slot, sample) in self.shared.right[slots].iter().zip(&right[source]) {
                slot.store(sample.to_bits(), Ordering::Relaxed);
            }
            position = (position + chunk) % capacity;
            offset += chunk;
        }

        self.write_index = end;
        if rate_bits != self.rate_bits {
            self.rate_bits = rate_bits;
            self.epoch_start = base;
        }
        self.shared.publish(end, self.epoch_start, rate_bits);
    }

    /// Frames ready to read.
    pub fn available_frames(&self) -> usize {
        self.shared.available_frames()
    }

    /// A handle to watch the buffer from any thread.
    pub fn monitor(&self) -> RingMonitor {
        RingMonitor {
            shared: Arc::clone(&self.shared),
        }
    }
}

/// The reading end. Move it to the analysis thread.
pub struct RingReader {
    shared: Arc<Shared>,
    read_index: u64,
    /// Test seam. Runs after the left channel of each copy segment, which is
    /// one of the instants where a lap by the writer must be caught.
    #[cfg(test)]
    hook_after_left_channel: Option<Box<dyn FnMut() + Send>>,
}

impl RingReader {
    pub fn capacity(&self) -> usize {
        self.shared.capacity as usize
    }

    /// Frames ready to read.
    pub fn available_frames(&self) -> usize {
        self.shared.available_frames()
    }

    /// Sample rate of the frames the last successful `read` returned. Before
    /// the first read, the rate of the newest write.
    pub fn sample_rate(&self) -> f64 {
        self.shared.sample_rate()
    }

    /// A handle to watch the buffer from any thread.
    pub fn monitor(&self) -> RingMonitor {
        RingMonitor {
            shared: Arc::clone(&self.shared),
        }
    }

    /// Reads up to `min(left.len(), right.len())` frames. Returns the frame count.
    pub fn read(&mut self, left: &mut [f32], right: &mut [f32]) -> usize {
        let max_count = left.len().min(right.len());
        if max_count == 0 {
            return 0;
        }
        let capacity = self.shared.capacity;
        for _ in 0..MAX_READ_ATTEMPTS {
            let s = self.shared.snapshot();
            let start = self.shared.read_start(s, self.read_index);
            let available = s.write_index - start;
            if available == 0 {
                // Nothing to take. Still record frames that an overrun or a rate change skipped.
                if start > self.read_index {
                    self.commit(start, s.rate_bits);
                }
                return 0;
            }
            let count = (max_count as u64).min(available) as usize;
            self.copy_out(start, &mut left[..count], &mut right[..count]);

            // Check the finished copy. `start`, the oldest frame index the copy
            // touched, is the only index to check. Write `C` for `capacity`:
            //
            //  1. The copy read the slots `start % C ..= (start + count - 1) % C`.
            //  2. The writer only writes frames at or after the `write_index` it
            //     published, and `start + count <= s.write_index`. So it never
            //     rewrites a frame this copy took. It can only overwrite a slot.
            //  3. Frame `f` lives in slot `f % C`. The lowest frame index that
            //     collides with a slot this copy read is `start + C`.
            //  4. The writer publishes the end of its in-flight region before it
            //     touches a sample, and `claim_end` never decreases. So the
            //     highest frame index it has claimed is `claim_end - 1`.
            //
            //  => the copy is intact  <=>  claim_end - 1 < start + C
            //                         <=>  claim_end - start <= C.
            //
            // The bound is `<=`, not `<`. A writer whose in-flight region ends
            // exactly at `start + C` stops one frame before the oldest slot this
            // copy read, and that copy is good.
            //
            // The fence makes step 4 true of the value loaded here.
            reader_copy_fence();
            let claim_end = self.shared.claim_end.load(Ordering::Acquire);
            if claim_end.saturating_sub(start) <= capacity {
                self.commit(start + count as u64, s.rate_bits);
                return count;
            }
        }
        // The writer lapped every attempt. Report nothing, never torn samples,
        // and move to the newest frames.
        let write_index = self.shared.write_index.load(Ordering::Acquire);
        if write_index > self.read_index {
            self.commit(write_index, self.shared.snapshot().rate_bits);
        }
        0
    }

    fn commit(&mut self, read_index: u64, rate_bits: u64) {
        self.read_index = read_index;
        self.shared.commit_read(read_index, rate_bits);
    }

    fn copy_out(&mut self, start: u64, left: &mut [f32], right: &mut [f32]) {
        let capacity = self.shared.capacity as usize;
        let count = left.len();
        let mut position = (start % self.shared.capacity) as usize;
        let mut offset = 0;
        while offset < count {
            let chunk = (count - offset).min(capacity - position);
            let slots = position..position + chunk;
            let target = offset..offset + chunk;
            for (sample, slot) in left[target.clone()].iter_mut().zip(&self.shared.left[slots.clone()]) {
                *sample = f32::from_bits(slot.load(Ordering::Relaxed));
            }
            #[cfg(test)]
            {
                if let Some(hook) = self.hook_after_left_channel.as_mut() {
                    hook();
                }
            }
            for (sample, slot) in right[target].iter_mut().zip(&self.shared.right[slots]) {
                *sample = f32::from_bits(slot.load(Ordering::Relaxed));
            }
            position = (position + chunk) % capacity;
            offset += chunk;
        }
    }
}

/// Watches a ring buffer from any thread. Cheap to clone.
#[derive(Clone)]
pub struct RingMonitor {
    shared: Arc<Shared>,
}

impl RingMonitor {
    pub fn capacity(&self) -> usize {
        self.shared.capacity as usize
    }

    /// Frames ready to read.
    pub fn available_frames(&self) -> usize {
        self.shared.available_frames()
    }

    /// See [`RingReader::sample_rate`].
    pub fn sample_rate(&self) -> f64 {
        self.shared.sample_rate()
    }
}

#[cfg(test)]
mod tests;
