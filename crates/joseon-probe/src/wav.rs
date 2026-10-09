//! A small WAV reader for `joseon-probe analyze`. Audio is read into memory
//! and analyzed there. Nothing is written.
//!
//! Reads RIFF/WAVE with integer PCM (8, 16, 24 or 32 bit) or float (32 or 64
//! bit), plain or `WAVE_FORMAT_EXTENSIBLE`. Mono goes to both channels. With
//! more than two channels the first two are used.

use std::fmt;

/// A file as two channels of float samples.
#[derive(Debug, Clone, PartialEq)]
pub struct WavAudio {
    pub sample_rate: f64,
    /// Channels in the file.
    pub channel_count: usize,
    pub bit_depth: u32,
    /// True for a float file.
    pub is_float: bool,
    pub left: Vec<f32>,
    pub right: Vec<f32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WavError {
    /// Not a RIFF/WAVE file.
    NotWave,
    /// The file ends in the middle of a chunk it needs.
    Truncated,
    /// A `fmt ` or `data` chunk is missing.
    MissingChunk(&'static str),
    /// A sample format this reader does not know.
    Unsupported(String),
}

impl fmt::Display for WavError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WavError::NotWave => write!(f, "not a RIFF/WAVE file"),
            WavError::Truncated => write!(f, "the file ends too early"),
            WavError::MissingChunk(name) => write!(f, "no '{name}' chunk"),
            WavError::Unsupported(what) => write!(f, "unsupported format: {what}"),
        }
    }
}

impl std::error::Error for WavError {}

const FORMAT_PCM: u16 = 1;
const FORMAT_FLOAT: u16 = 3;
const FORMAT_EXTENSIBLE: u16 = 0xFFFE;

fn u16_at(bytes: &[u8], at: usize) -> Result<u16, WavError> {
    bytes
        .get(at..at + 2)
        .map(|b| u16::from_le_bytes([b[0], b[1]]))
        .ok_or(WavError::Truncated)
}

fn u32_at(bytes: &[u8], at: usize) -> Result<u32, WavError> {
    bytes
        .get(at..at + 4)
        .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .ok_or(WavError::Truncated)
}

/// Parses the bytes of a WAV file.
pub fn parse(bytes: &[u8]) -> Result<WavAudio, WavError> {
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err(WavError::NotWave);
    }
    let mut format: Option<(u16, usize, f64, u32)> = None; // tag, channels, rate, bits
    let mut data: Option<&[u8]> = None;
    let mut at = 12;
    while at + 8 <= bytes.len() {
        let id = &bytes[at..at + 4];
        let size = u32_at(bytes, at + 4)? as usize;
        let body_start = at + 8;
        // A recorder that was cut off leaves a data size past the end: take what is there.
        let body_end = body_start.saturating_add(size).min(bytes.len());
        let body = &bytes[body_start..body_end];
        match id {
            b"fmt " => {
                let mut tag = u16_at(body, 0)?;
                let channels = u16_at(body, 2)? as usize;
                let rate = u32_at(body, 4)? as f64;
                let bits = u16_at(body, 14)? as u32;
                if tag == FORMAT_EXTENSIBLE {
                    // The real tag is the first two bytes of the sub-format GUID.
                    tag = u16_at(body, 24)?;
                }
                format = Some((tag, channels, rate, bits));
            }
            b"data" => data = Some(body),
            _ => {}
        }
        // Chunks are padded to an even size.
        at = body_start.saturating_add(size).saturating_add(size & 1);
    }
    let (tag, channels, rate, bits) = format.ok_or(WavError::MissingChunk("fmt "))?;
    let data = data.ok_or(WavError::MissingChunk("data"))?;
    if channels == 0 || !(rate > 0.0) {
        return Err(WavError::Unsupported(format!("{channels} channels at {rate} Hz")));
    }
    let decode: fn(&[u8]) -> f32 = match (tag, bits) {
        (FORMAT_PCM, 8) => |b| (b[0] as f32 - 128.0) / 128.0,
        (FORMAT_PCM, 16) => |b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32_768.0,
        (FORMAT_PCM, 24) => |b| (i32::from_le_bytes([0, b[0], b[1], b[2]]) >> 8) as f32 / 8_388_608.0,
        (FORMAT_PCM, 32) => |b| (i32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f64 / 2_147_483_648.0) as f32,
        (FORMAT_FLOAT, 32) => |b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]),
        (FORMAT_FLOAT, 64) => |b| f64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]) as f32,
        _ => return Err(WavError::Unsupported(format!("format tag {tag}, {bits} bits"))),
    };
    let bytes_per_sample = (bits / 8) as usize;
    let frame_bytes = bytes_per_sample * channels;
    let frames = data.len() / frame_bytes;
    let (mut left, mut right) = (Vec::with_capacity(frames), Vec::with_capacity(frames));
    for frame in data.chunks_exact(frame_bytes) {
        let l = decode(&frame[..bytes_per_sample]);
        let r = if channels > 1 {
            decode(&frame[bytes_per_sample..2 * bytes_per_sample])
        } else {
            l
        };
        left.push(l);
        right.push(r);
    }
    Ok(WavAudio {
        sample_rate: rate,
        channel_count: channels,
        bit_depth: bits,
        is_float: tag == FORMAT_FLOAT,
        left,
        right,
    })
}

/// The bytes of a WAV file. For tests and for people who want a file to try the probe on.
/// `bits` is 16 or 24 for integer PCM, or 32 for float.
pub fn encode(channels: &[&[f32]], sample_rate: u32, bits: u32) -> Vec<u8> {
    let channel_count = channels.len();
    let frames = channels.iter().map(|channel| channel.len()).min().unwrap_or(0);
    let bytes_per_sample = (bits / 8) as usize;
    let data_size = frames * channel_count * bytes_per_sample;
    let mut out = Vec::with_capacity(44 + data_size);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_size as u32).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&(if bits == 32 { FORMAT_FLOAT } else { FORMAT_PCM }).to_le_bytes());
    out.extend_from_slice(&(channel_count as u16).to_le_bytes());
    out.extend_from_slice(&sample_rate.to_le_bytes());
    out.extend_from_slice(&(sample_rate * (channel_count * bytes_per_sample) as u32).to_le_bytes());
    out.extend_from_slice(&((channel_count * bytes_per_sample) as u16).to_le_bytes());
    out.extend_from_slice(&(bits as u16).to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&(data_size as u32).to_le_bytes());
    for frame in 0..frames {
        for channel in channels {
            let sample = channel[frame];
            match bits {
                16 => out.extend_from_slice(&((sample.clamp(-1.0, 1.0) * 32_767.0).round() as i16).to_le_bytes()),
                24 => {
                    out.extend_from_slice(&((sample.clamp(-1.0, 1.0) * 8_388_607.0).round() as i32).to_le_bytes()[..3])
                }
                _ => out.extend_from_slice(&sample.to_le_bytes()),
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ramp(count: usize, scale: f32) -> Vec<f32> {
        (0..count)
            .map(|i| (i as f32 / count as f32 * 2.0 - 1.0) * scale)
            .collect()
    }

    #[test]
    fn float_stereo_round_trips_exactly() {
        let (left, right) = (ramp(1000, 0.9), ramp(1000, -0.5));
        let audio = parse(&encode(&[&left, &right], 96_000, 32)).unwrap();
        assert_eq!(
            (audio.sample_rate, audio.channel_count, audio.bit_depth, audio.is_float),
            (96_000.0, 2, 32, true)
        );
        assert_eq!(audio.left, left);
        assert_eq!(audio.right, right);
    }

    #[test]
    fn sixteen_and_twenty_four_bit_round_trip_within_two_steps() {
        let (left, right) = (ramp(2000, 1.0), ramp(2000, 0.25));
        for (bits, step) in [(16, 1.0 / 32_768.0), (24, 1.0 / 8_388_608.0)] {
            let audio = parse(&encode(&[&left, &right], 44_100, bits)).unwrap();
            assert_eq!((audio.bit_depth, audio.is_float, audio.left.len()), (bits, false, 2000));
            for (a, b) in audio.left.iter().zip(&left).chain(audio.right.iter().zip(&right)) {
                // The writer scales by 2^n − 1 and the reader by 2^n: just over one step at full scale.
                assert!((a - b).abs() <= 2.0 * step, "{bits} bit: {a} vs {b}");
            }
        }
    }

    #[test]
    fn mono_goes_to_both_channels_and_extra_channels_are_dropped() {
        let mono = ramp(100, 0.5);
        let audio = parse(&encode(&[&mono], 48_000, 32)).unwrap();
        assert_eq!((audio.channel_count, &audio.left, &audio.right), (1, &mono, &mono));

        let (a, b, c) = (ramp(100, 0.1), ramp(100, 0.2), ramp(100, 0.3));
        let audio = parse(&encode(&[&a, &b, &c], 48_000, 32)).unwrap();
        assert_eq!((audio.channel_count, &audio.left, &audio.right), (3, &a, &b));
    }

    #[test]
    fn eight_bit_thirty_two_bit_integer_and_double_decode() {
        let header = |tag: u16, bits: u16, data: &[u8]| {
            let mut out = Vec::new();
            out.extend_from_slice(b"RIFF\0\0\0\0WAVEfmt ");
            out.extend_from_slice(&16u32.to_le_bytes());
            out.extend_from_slice(&tag.to_le_bytes());
            out.extend_from_slice(&1u16.to_le_bytes());
            out.extend_from_slice(&48_000u32.to_le_bytes());
            out.extend_from_slice(&[0; 6]);
            out.extend_from_slice(&bits.to_le_bytes());
            out.extend_from_slice(b"data");
            out.extend_from_slice(&(data.len() as u32).to_le_bytes());
            out.extend_from_slice(data);
            out
        };
        assert_eq!(
            parse(&header(1, 8, &[0, 128, 255])).unwrap().left,
            [-1.0, 0.0, 127.0 / 128.0]
        );
        let ints: Vec<u8> = [i32::MIN, 0, 1 << 30].iter().flat_map(|v| v.to_le_bytes()).collect();
        assert_eq!(parse(&header(1, 32, &ints)).unwrap().left, [-1.0, 0.0, 0.5]);
        let doubles: Vec<u8> = [0.25f64, -0.75].iter().flat_map(|v| v.to_le_bytes()).collect();
        assert_eq!(parse(&header(3, 64, &doubles)).unwrap().left, [0.25, -0.75]);
    }

    #[test]
    fn extensible_format_and_unknown_chunks_are_read() {
        let samples: Vec<u8> = [1000i16, -1000].iter().flat_map(|v| v.to_le_bytes()).collect();
        let mut out = Vec::new();
        out.extend_from_slice(b"RIFF\0\0\0\0WAVE");
        // An odd-sized chunk before the format: it is padded to an even size.
        out.extend_from_slice(b"LIST");
        out.extend_from_slice(&3u32.to_le_bytes());
        out.extend_from_slice(&[1, 2, 3, 0]);
        out.extend_from_slice(b"fmt ");
        out.extend_from_slice(&40u32.to_le_bytes());
        out.extend_from_slice(&0xFFFEu16.to_le_bytes());
        out.extend_from_slice(&2u16.to_le_bytes());
        out.extend_from_slice(&44_100u32.to_le_bytes());
        out.extend_from_slice(&[0; 6]);
        out.extend_from_slice(&16u16.to_le_bytes());
        out.extend_from_slice(&[22, 0, 16, 0, 3, 0, 0, 0]); // extension size, valid bits, channel mask
        out.extend_from_slice(&1u16.to_le_bytes()); // sub-format: PCM
        out.extend_from_slice(&[0; 14]);
        out.extend_from_slice(b"data");
        out.extend_from_slice(&(samples.len() as u32).to_le_bytes());
        out.extend_from_slice(&samples);
        let audio = parse(&out).unwrap();
        assert_eq!(
            (audio.channel_count, audio.sample_rate, audio.left.len()),
            (2, 44_100.0, 1)
        );
        assert!((audio.left[0] - 1000.0 / 32_768.0).abs() < 1e-7);
        assert!((audio.right[0] + 1000.0 / 32_768.0).abs() < 1e-7);
    }

    #[test]
    fn bad_files_give_errors_not_panics() {
        assert_eq!(parse(b""), Err(WavError::NotWave));
        assert_eq!(parse(b"RIFF\0\0\0\0AIFF"), Err(WavError::NotWave));
        assert_eq!(parse(b"RIFF\0\0\0\0WAVE"), Err(WavError::MissingChunk("fmt ")));
        let good = encode(&[&ramp(10, 0.5)], 48_000, 16);
        assert_eq!(parse(&good[..36]), Err(WavError::MissingChunk("data")));
        assert_eq!(parse(&good[..30]), Err(WavError::Truncated));
        // A data chunk that says it is longer than the file: what is there is read.
        assert_eq!(parse(&good[..good.len() - 4]).unwrap().left.len(), 8);
        let mut odd = good.clone();
        odd[20] = 2; // ADPCM
        assert!(matches!(parse(&odd), Err(WavError::Unsupported(_))));
        let mut no_channels = good;
        no_channels[22] = 0;
        assert!(matches!(parse(&no_channels), Err(WavError::Unsupported(_))));
        // Every prefix of a valid file parses or fails cleanly.
        let file = encode(&[&ramp(20, 0.5), &ramp(20, 0.5)], 48_000, 24);
        for end in 0..file.len() {
            let _ = parse(&file[..end]);
        }
    }
}
