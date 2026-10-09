//! The sample format of a capture stream, read from the bytes of a Windows
//! `WAVEFORMATEX` or `WAVEFORMATEXTENSIBLE`. No Windows type is used here, so
//! the tests run on every system.

/// How one sample is stored. Little-endian.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SampleKind {
    U8,
    I16,
    /// Three bytes.
    I24,
    /// Four bytes. A 24-bit sample in this container is left-justified, so the scale is the same.
    I32,
    F32,
    F64,
}

impl SampleKind {
    pub fn bytes(self) -> usize {
        match self {
            Self::U8 => 1,
            Self::I16 => 2,
            Self::I24 => 3,
            Self::I32 | Self::F32 => 4,
            Self::F64 => 8,
        }
    }
}

/// What a capture packet holds.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WireFormat {
    pub kind: SampleKind,
    pub channels: usize,
    pub sample_rate: f64,
    /// Bits of a sample that carry signal (24 in a 32-bit container, for example).
    pub valid_bits: u32,
    /// Speaker positions, one bit per channel, in the order of the channels. 0 = not given.
    pub channel_mask: u32,
}

pub const WAVE_FORMAT_PCM: u16 = 1;
pub const WAVE_FORMAT_IEEE_FLOAT: u16 = 3;
pub const WAVE_FORMAT_EXTENSIBLE: u16 = 0xFFFE;
/// The bytes of a `WAVEFORMATEX`, and of the part a `WAVEFORMATEXTENSIBLE` adds.
pub const BASE_BYTES: usize = 18;
pub const EXTENSION_BYTES: usize = 22;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FormatError {
    TooShort,
    /// The format tag (or the first word of the sub-format) is not PCM and not float.
    UnknownEncoding(u32),
    /// The container size does not fit the encoding.
    UnknownSampleSize {
        float: bool,
        bits: u16,
    },
    NoChannels,
    NoSampleRate,
}

impl std::fmt::Display for FormatError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooShort => write!(f, "the stream format is cut short"),
            Self::UnknownEncoding(tag) => write!(f, "the stream encoding {tag:#x} is not PCM and not float"),
            Self::UnknownSampleSize { float, bits } => {
                write!(
                    f,
                    "{bits}-bit {} samples are not supported",
                    if *float { "float" } else { "integer" }
                )
            }
            Self::NoChannels => write!(f, "the stream has no channels"),
            Self::NoSampleRate => write!(f, "the stream has no sample rate"),
        }
    }
}

impl WireFormat {
    /// The number of bytes of the format structure that starts at `head` (at least `BASE_BYTES` long).
    pub fn structure_bytes(head: &[u8]) -> usize {
        if head.len() < BASE_BYTES {
            return BASE_BYTES;
        }
        BASE_BYTES + u16::from_le_bytes([head[16], head[17]]) as usize
    }

    /// Reads a `WAVEFORMATEX` (18 bytes) or a `WAVEFORMATEXTENSIBLE` (40 bytes).
    pub fn parse(bytes: &[u8]) -> Result<Self, FormatError> {
        if bytes.len() < 16 {
            return Err(FormatError::TooShort);
        }
        let u16_at = |i: usize| u16::from_le_bytes([bytes[i], bytes[i + 1]]);
        let u32_at = |i: usize| u32::from_le_bytes([bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]]);
        let tag = u16_at(0);
        let channels = u16_at(2) as usize;
        let sample_rate = u32_at(4);
        let container_bits = u16_at(14);
        let (mut encoding, mut valid_bits, mut channel_mask) = (tag as u32, container_bits as u32, 0);
        if tag == WAVE_FORMAT_EXTENSIBLE {
            if bytes.len() < BASE_BYTES + EXTENSION_BYTES {
                return Err(FormatError::TooShort);
            }
            // wValidBitsPerSample, dwChannelMask, then the sub-format GUID. Its first word is the old format tag.
            let stated = u16_at(18) as u32;
            if stated > 0 && stated <= container_bits as u32 {
                valid_bits = stated;
            }
            channel_mask = u32_at(20);
            encoding = u32_at(24);
        }
        if channels == 0 {
            return Err(FormatError::NoChannels);
        }
        if sample_rate == 0 {
            return Err(FormatError::NoSampleRate);
        }
        let float = match encoding {
            e if e == WAVE_FORMAT_PCM as u32 => false,
            e if e == WAVE_FORMAT_IEEE_FLOAT as u32 => true,
            other => return Err(FormatError::UnknownEncoding(other)),
        };
        let kind = match (float, container_bits) {
            (true, 32) => SampleKind::F32,
            (true, 64) => SampleKind::F64,
            (false, 8) => SampleKind::U8,
            (false, 16) => SampleKind::I16,
            (false, 24) => SampleKind::I24,
            (false, 32) => SampleKind::I32,
            (float, bits) => return Err(FormatError::UnknownSampleSize { float, bits }),
        };
        Ok(Self {
            kind,
            channels,
            sample_rate: sample_rate as f64,
            valid_bits,
            channel_mask,
        })
    }

    pub fn frame_bytes(&self) -> usize {
        self.kind.bytes() * self.channels
    }

    /// The bit depth to show: `None` for float (the mixer format says nothing about the device).
    pub fn integer_bits(&self) -> Option<u32> {
        match self.kind {
            SampleKind::F32 | SampleKind::F64 => None,
            _ => Some(self.valid_bits),
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// The bytes of a `WAVEFORMATEX`, or of a `WAVEFORMATEXTENSIBLE` when `extension` is given
    /// (valid bits, channel mask, encoding).
    pub(crate) fn wave_format(
        tag: u16,
        channels: u16,
        rate: u32,
        bits: u16,
        extension: Option<(u16, u32, u32)>,
    ) -> Vec<u8> {
        let block = channels * (bits / 8);
        let mut b = Vec::new();
        b.extend(tag.to_le_bytes());
        b.extend(channels.to_le_bytes());
        b.extend(rate.to_le_bytes());
        b.extend((rate * block as u32).to_le_bytes());
        b.extend(block.to_le_bytes());
        b.extend(bits.to_le_bytes());
        b.extend(if extension.is_some() { 22u16 } else { 0 }.to_le_bytes());
        if let Some((valid, mask, encoding)) = extension {
            b.extend(valid.to_le_bytes());
            b.extend(mask.to_le_bytes());
            // {encoding-0000-0010-8000-00AA00389B71}
            b.extend(encoding.to_le_bytes());
            b.extend([0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0xAA, 0x00, 0x38, 0x9B, 0x71]);
        }
        b
    }

    #[test]
    fn the_usual_mix_format_is_float_in_an_extensible_header() {
        let bytes = wave_format(WAVE_FORMAT_EXTENSIBLE, 2, 48_000, 32, Some((32, 0x3, 3)));
        assert_eq!(bytes.len(), 40);
        assert_eq!(WireFormat::structure_bytes(&bytes), 40);
        let f = WireFormat::parse(&bytes).unwrap();
        assert_eq!(
            f,
            WireFormat {
                kind: SampleKind::F32,
                channels: 2,
                sample_rate: 48_000.0,
                valid_bits: 32,
                channel_mask: 0x3
            }
        );
        assert_eq!(f.frame_bytes(), 8);
        assert_eq!(f.integer_bits(), None);
    }

    #[test]
    fn plain_headers_and_integer_formats_are_read() {
        let f = WireFormat::parse(&wave_format(WAVE_FORMAT_PCM, 2, 44_100, 16, None)).unwrap();
        assert_eq!(
            (f.kind, f.channels, f.sample_rate, f.integer_bits()),
            (SampleKind::I16, 2, 44_100.0, Some(16))
        );
        // A header of 16 bytes (no cbSize) is a valid old PCM format.
        assert!(WireFormat::parse(&wave_format(WAVE_FORMAT_PCM, 1, 8_000, 8, None)[..16]).is_ok());
        let f = WireFormat::parse(&wave_format(
            WAVE_FORMAT_EXTENSIBLE,
            8,
            96_000,
            32,
            Some((24, 0x63F, 1)),
        ))
        .unwrap();
        assert_eq!(
            (f.kind, f.channels, f.valid_bits, f.channel_mask),
            (SampleKind::I32, 8, 24, 0x63F)
        );
        assert_eq!(f.integer_bits(), Some(24));
        let f = WireFormat::parse(&wave_format(WAVE_FORMAT_PCM, 2, 48_000, 24, None)).unwrap();
        assert_eq!((f.kind, f.frame_bytes()), (SampleKind::I24, 6));
        let f = WireFormat::parse(&wave_format(WAVE_FORMAT_IEEE_FLOAT, 2, 48_000, 64, None)).unwrap();
        assert_eq!(f.kind, SampleKind::F64);
    }

    #[test]
    fn a_format_that_cannot_be_read_says_why() {
        assert_eq!(WireFormat::parse(&[0; 10]), Err(FormatError::TooShort));
        let cut = &wave_format(WAVE_FORMAT_EXTENSIBLE, 2, 48_000, 32, Some((32, 3, 3)))[..30];
        assert_eq!(WireFormat::parse(cut), Err(FormatError::TooShort));
        assert_eq!(
            WireFormat::parse(&wave_format(0x55, 2, 44_100, 16, None)),
            Err(FormatError::UnknownEncoding(0x55))
        );
        assert_eq!(
            WireFormat::parse(&wave_format(WAVE_FORMAT_EXTENSIBLE, 2, 48_000, 32, Some((32, 3, 0x92)))),
            Err(FormatError::UnknownEncoding(0x92))
        );
        assert_eq!(
            WireFormat::parse(&wave_format(WAVE_FORMAT_IEEE_FLOAT, 2, 48_000, 16, None)),
            Err(FormatError::UnknownSampleSize { float: true, bits: 16 })
        );
        assert_eq!(
            WireFormat::parse(&wave_format(WAVE_FORMAT_PCM, 0, 48_000, 16, None)),
            Err(FormatError::NoChannels)
        );
        assert_eq!(
            WireFormat::parse(&wave_format(WAVE_FORMAT_PCM, 2, 0, 16, None)),
            Err(FormatError::NoSampleRate)
        );
        assert!(FormatError::UnknownEncoding(0x92).to_string().contains("0x92"));
    }
}
