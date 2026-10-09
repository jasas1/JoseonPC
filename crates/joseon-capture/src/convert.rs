//! Capture packets (interleaved bytes in the format of the device mix) to the
//! two float channels the analysis takes.
//!
//! Two channels pass through. One channel goes to both sides. A surround mix
//! (a 5.1 or 7.1 endpoint, as many gaming headsets show to Windows) is folded
//! down to stereo, so the analyzer sees the center and the surround channels
//! too: see `Downmix`.

use crate::format::{SampleKind, WireFormat};

// Speaker positions of a channel mask, in channel order.
const FRONT_LEFT: u32 = 0x1;
const FRONT_RIGHT: u32 = 0x2;
const FRONT_CENTER: u32 = 0x4;
const LOW_FREQUENCY: u32 = 0x8;
const BACK_LEFT: u32 = 0x10;
const BACK_RIGHT: u32 = 0x20;
const FRONT_LEFT_OF_CENTER: u32 = 0x40;
const FRONT_RIGHT_OF_CENTER: u32 = 0x80;
const BACK_CENTER: u32 = 0x100;
const SIDE_LEFT: u32 = 0x200;
const SIDE_RIGHT: u32 = 0x400;

/// -3 dB.
const HALF_POWER: f32 = std::f32::consts::FRAC_1_SQRT_2;

/// The gain of every channel into the left and into the right output.
///
/// The fold-down follows ITU-R BS.775: front left and right at full level, the
/// center and the surround channels 3 dB down. The bass channel (LFE) is added
/// 3 dB down to both sides: an analyzer that left it out would hide bass that
/// the listener hears. Nothing is scaled down after the sum, so a loud surround
/// mix can read over full scale. The readouts then show it, as they must.
#[derive(Debug, Clone, PartialEq)]
pub struct Downmix {
    pub left: Vec<f32>,
    pub right: Vec<f32>,
}

impl Downmix {
    /// `mask` = the speaker positions (0 = not given: the usual layout of that channel count).
    pub fn for_layout(channels: usize, mask: u32) -> Self {
        let mut mask = mask;
        if mask.count_ones() as usize != channels {
            mask = match channels {
                1 => FRONT_CENTER,
                2 => 0x3,
                3 => 0x7,
                4 => 0x33,
                5 => 0x37,
                6 => 0x3F,
                7 => 0x13F,
                8 => 0x63F,
                _ => 0,
            };
        }
        let (mut left, mut right) = (vec![0.0; channels], vec![0.0; channels]);
        if channels == 1 {
            return Self {
                left: vec![1.0],
                right: vec![1.0],
            };
        }
        if mask == 0 {
            // An unknown layout: the first two channels are left and right.
            left[0] = 1.0;
            right[1] = 1.0;
            return Self { left, right };
        }
        let mut channel = 0;
        for bit in 0..32 {
            let position = 1u32 << bit;
            if mask & position == 0 {
                continue;
            }
            if channel >= channels {
                break;
            }
            let (l, r) = match position {
                FRONT_LEFT => (1.0, 0.0),
                FRONT_RIGHT => (0.0, 1.0),
                FRONT_CENTER | LOW_FREQUENCY | BACK_CENTER => (HALF_POWER, HALF_POWER),
                BACK_LEFT | SIDE_LEFT | FRONT_LEFT_OF_CENTER => (HALF_POWER, 0.0),
                BACK_RIGHT | SIDE_RIGHT | FRONT_RIGHT_OF_CENTER => (0.0, HALF_POWER),
                // Height channels and the like: not in the stereo picture.
                _ => (0.0, 0.0),
            };
            left[channel] = l;
            right[channel] = r;
            channel += 1;
        }
        Self { left, right }
    }

    /// True when the two outputs are channels 0 and 1 as they are.
    pub fn is_plain_stereo(&self) -> bool {
        self.left.len() == 2 && self.left == [1.0, 0.0] && self.right == [0.0, 1.0]
    }
}

/// Turns packets into left and right samples. It keeps its buffers, so a packet
/// that is not larger than the largest one before it allocates nothing.
pub struct Converter {
    format: WireFormat,
    downmix: Downmix,
    /// Every sample is multiplied by this (1 = as captured).
    gain: f32,
    left: Vec<f32>,
    right: Vec<f32>,
}

impl Converter {
    pub fn new(format: WireFormat) -> Self {
        Self {
            downmix: Downmix::for_layout(format.channels, format.channel_mask),
            format,
            gain: 1.0,
            left: Vec::new(),
            right: Vec::new(),
        }
    }

    /// The factor on every sample from now on. It undoes a volume control that stands before the capture point.
    pub fn set_gain(&mut self, gain: f32) {
        self.gain = if gain.is_finite() && gain > 0.0 { gain } else { 1.0 };
    }

    pub fn gain(&self) -> f32 {
        self.gain
    }

    pub fn format(&self) -> &WireFormat {
        &self.format
    }

    pub fn downmix(&self) -> &Downmix {
        &self.downmix
    }

    fn sample(kind: SampleKind, b: &[u8]) -> f32 {
        match kind {
            SampleKind::F32 => f32::from_le_bytes([b[0], b[1], b[2], b[3]]),
            SampleKind::F64 => f64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]) as f32,
            SampleKind::I16 => i16::from_le_bytes([b[0], b[1]]) as f32 / 32_768.0,
            SampleKind::I24 => (i32::from_le_bytes([0, b[0], b[1], b[2]]) >> 8) as f32 / 8_388_608.0,
            SampleKind::I32 => (i32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f64 / 2_147_483_648.0) as f32,
            SampleKind::U8 => (b[0] as f32 - 128.0) / 128.0,
        }
    }

    /// The whole frames of `bytes`, as left and right samples.
    pub fn convert(&mut self, bytes: &[u8]) -> (&[f32], &[f32]) {
        let kind = self.format.kind;
        let (size, channels) = (kind.bytes(), self.format.channels);
        let frames = bytes.len() / (size * channels);
        let gain = self.gain;
        self.left.clear();
        self.right.clear();
        self.left.reserve(frames);
        self.right.reserve(frames);
        if self.downmix.is_plain_stereo() {
            for frame in bytes.chunks_exact(size * 2) {
                self.left.push(Self::sample(kind, &frame[..size]) * gain);
                self.right.push(Self::sample(kind, &frame[size..]) * gain);
            }
        } else {
            for frame in bytes.chunks_exact(size * channels) {
                let (mut l, mut r) = (0.0f32, 0.0f32);
                for (c, raw) in frame.chunks_exact(size).enumerate() {
                    let v = Self::sample(kind, raw);
                    l += v * self.downmix.left[c];
                    r += v * self.downmix.right[c];
                }
                self.left.push(l * gain);
                self.right.push(r * gain);
            }
        }
        (&self.left, &self.right)
    }

    /// `frames` of digital silence (a packet the system marks as silent).
    pub fn silence(&mut self, frames: usize) -> (&[f32], &[f32]) {
        self.left.clear();
        self.right.clear();
        self.left.resize(frames, 0.0);
        self.right.resize(frames, 0.0);
        (&self.left, &self.right)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn format(kind: SampleKind, channels: usize, mask: u32) -> WireFormat {
        WireFormat {
            kind,
            channels,
            sample_rate: 48_000.0,
            valid_bits: kind.bytes() as u32 * 8,
            channel_mask: mask,
        }
    }

    fn floats(values: &[f32]) -> Vec<u8> {
        values.iter().flat_map(|v| v.to_le_bytes()).collect()
    }

    #[test]
    fn float_stereo_passes_through_bit_for_bit() {
        let mut c = Converter::new(format(SampleKind::F32, 2, 0x3));
        assert!(c.downmix().is_plain_stereo());
        let input = [0.25f32, -0.5, 1.0, -1.0, 1e-9, 0.0];
        let (l, r) = c.convert(&floats(&input));
        assert_eq!(l, [0.25, 1.0, 1e-9]);
        assert_eq!(r, [-0.5, -1.0, 0.0]);
        // A cut frame at the end is left out.
        let mut bytes = floats(&input);
        bytes.extend([1, 2, 3]);
        assert_eq!(c.convert(&bytes).0.len(), 3);
        assert_eq!(c.silence(5), (&[0.0f32; 5][..], &[0.0f32; 5][..]));
    }

    #[test]
    fn integer_samples_have_full_scale_at_one() {
        let mut c = Converter::new(format(SampleKind::I16, 2, 0));
        let bytes: Vec<u8> = [i16::MIN, i16::MAX, 16_384, 0]
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect();
        let (l, r) = c.convert(&bytes);
        assert_eq!(l, [-1.0, 0.5]);
        assert_eq!(r, [32_767.0 / 32_768.0, 0.0]);

        let mut c = Converter::new(format(SampleKind::I24, 2, 0));
        // -1, +half, the smallest step, -the smallest step.
        let bytes = [0x00, 0x00, 0x80, 0x00, 0x00, 0x40, 0x01, 0x00, 0x00, 0xFF, 0xFF, 0xFF];
        let (l, r) = c.convert(&bytes);
        assert_eq!(l, [-1.0, 1.0 / 8_388_608.0]);
        assert_eq!(r, [0.5, -1.0 / 8_388_608.0]);

        let mut c = Converter::new(format(SampleKind::I32, 2, 0));
        let bytes: Vec<u8> = [i32::MIN, 1 << 30].iter().flat_map(|v| v.to_le_bytes()).collect();
        assert_eq!(c.convert(&bytes), (&[-1.0f32][..], &[0.5f32][..]));

        let mut c = Converter::new(format(SampleKind::U8, 2, 0));
        assert_eq!(c.convert(&[0, 192]), (&[-1.0f32][..], &[0.5f32][..]));

        let mut c = Converter::new(format(SampleKind::F64, 2, 0));
        let bytes: Vec<u8> = [0.125f64, -0.75].iter().flat_map(|v| v.to_le_bytes()).collect();
        assert_eq!(c.convert(&bytes), (&[0.125f32][..], &[-0.75f32][..]));
    }

    #[test]
    fn the_gain_is_on_every_sample() {
        let mut c = Converter::new(format(SampleKind::F32, 2, 0x3));
        c.set_gain(4.0);
        assert_eq!(c.convert(&floats(&[0.125, -0.25])), (&[0.5f32][..], &[-1.0f32][..]));
        let mut c = Converter::new(format(SampleKind::F32, 1, 0));
        c.set_gain(2.0);
        assert_eq!(c.convert(&floats(&[0.25])).1, [0.5]);
        // A gain that makes no sense is no gain.
        for bad in [0.0, -1.0, f32::NAN, f32::INFINITY] {
            c.set_gain(bad);
            assert_eq!(c.gain(), 1.0);
        }
    }

    #[test]
    fn one_channel_goes_to_both_sides() {
        let mut c = Converter::new(format(SampleKind::F32, 1, 0));
        let (l, r) = c.convert(&floats(&[0.3, -0.2]));
        assert_eq!(l, [0.3, -0.2]);
        assert_eq!(l, r);
    }

    #[test]
    fn a_surround_mix_is_folded_down() {
        // 5.1: FL FR FC LFE BL BR.
        let d = Downmix::for_layout(6, 0x3F);
        assert_eq!(d.left, [1.0, 0.0, HALF_POWER, HALF_POWER, HALF_POWER, 0.0]);
        assert_eq!(d.right, [0.0, 1.0, HALF_POWER, HALF_POWER, 0.0, HALF_POWER]);
        // 7.1 with side channels: FL FR FC LFE BL BR SL SR.
        let d = Downmix::for_layout(8, 0x63F);
        assert_eq!(d.left[6], HALF_POWER);
        assert_eq!(d.right[7], HALF_POWER);
        // No mask: the usual layout of the count.
        assert_eq!(Downmix::for_layout(6, 0), Downmix::for_layout(6, 0x3F));
        // A mask that does not fit the count is not trusted.
        assert_eq!(Downmix::for_layout(2, 0x3F), Downmix::for_layout(2, 0x3));
        // More channels than any known layout: the first two.
        let d = Downmix::for_layout(12, 0);
        assert_eq!((d.left[0], d.right[1], d.left[5]), (1.0, 1.0, 0.0));

        // Speech in the center of a 5.1 mix reaches both sides at -3 dB.
        let mut c = Converter::new(format(SampleKind::F32, 6, 0x3F));
        let (l, r) = c.convert(&floats(&[0.0, 0.0, 0.5, 0.0, 0.0, 0.0, 0.2, -0.2, 0.0, 0.0, 0.0, 0.4]));
        assert_eq!(l, [0.5 * HALF_POWER, 0.2]);
        assert_eq!(r, [0.5 * HALF_POWER, -0.2 + 0.4 * HALF_POWER]);
    }
}
