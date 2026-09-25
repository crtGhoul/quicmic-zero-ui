//! Mix-format classification for WASAPI loopback capture.
//!
//! Pure logic, no Windows API — so it can be unit-tested anywhere.
//! The Windows side reads the `WAVEFORMATEXTENSIBLE` SubFormat GUID,
//! converts it with `GUID::to_u128()`, and calls [`classify_mix_format`].

/// wFormatTag values (mmreg.h).
pub const WAVE_FORMAT_PCM: u32 = 1;
pub const WAVE_FORMAT_IEEE_FLOAT: u32 = 3;
pub const WAVE_FORMAT_EXTENSIBLE: u32 = 0xFFFE;

/// KSDATAFORMAT_SUBTYPE_PCM as a u128 (matches `GUID::to_u128()`).
pub const SUBTYPE_PCM: u128 = 0x00000001_0000_0010_8000_00aa00389b71;
/// KSDATAFORMAT_SUBTYPE_IEEE_FLOAT as a u128 (matches `GUID::to_u128()`).
pub const SUBTYPE_IEEE_FLOAT: u128 = 0x00000003_0000_0010_8000_00aa00389b71;

/// How captured samples are stored in the WASAPI buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SampleKind {
    /// 32-bit float, use as-is.
    F32,
    /// 16-bit int, divide by 32768.
    I16,
    /// 32-bit int, divide by 2^31.
    I32,
}

/// Decide how to interpret a capture buffer from the mix format.
///
/// `tag`/`bits` come straight from `WAVEFORMATEX`; `subformat` is
/// `Some(GUID::to_u128())` only when `tag == WAVE_FORMAT_EXTENSIBLE`.
/// Returns `None` for anything we can't convert to f32.
pub fn classify_mix_format(tag: u32, bits: u16, subformat: Option<u128>) -> Option<SampleKind> {
    match tag {
        WAVE_FORMAT_IEEE_FLOAT if bits == 32 => Some(SampleKind::F32),
        WAVE_FORMAT_PCM if bits == 16 => Some(SampleKind::I16),
        WAVE_FORMAT_PCM if bits == 32 => Some(SampleKind::I32),
        WAVE_FORMAT_EXTENSIBLE => match subformat {
            Some(s) if s == SUBTYPE_IEEE_FLOAT && bits == 32 => Some(SampleKind::F32),
            Some(s) if s == SUBTYPE_PCM && bits == 16 => Some(SampleKind::I16),
            Some(s) if s == SUBTYPE_PCM && bits == 32 => Some(SampleKind::I32),
            _ => None,
        },
        _ => None,
    }
}

/// Scale one 16-bit int sample to f32 in [-1, 1].
pub fn i16_to_f32(v: i16) -> f32 {
    v as f32 / 32768.0
}

/// Scale one 32-bit int sample to f32 in [-1, 1].
pub fn i32_to_f32(v: i32) -> f32 {
    v as f32 / 2147483648.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_float32() {
        assert_eq!(
            classify_mix_format(WAVE_FORMAT_IEEE_FLOAT, 32, None),
            Some(SampleKind::F32)
        );
    }

    #[test]
    fn plain_pcm16() {
        assert_eq!(
            classify_mix_format(WAVE_FORMAT_PCM, 16, None),
            Some(SampleKind::I16)
        );
    }

    #[test]
    fn plain_pcm32() {
        assert_eq!(
            classify_mix_format(WAVE_FORMAT_PCM, 32, None),
            Some(SampleKind::I32)
        );
    }

    #[test]
    fn extensible_float32_is_the_reported_case() {
        // Exactly what the user's PC reported: tag=65534 bits=32.
        assert_eq!(
            classify_mix_format(WAVE_FORMAT_EXTENSIBLE, 32, Some(SUBTYPE_IEEE_FLOAT)),
            Some(SampleKind::F32)
        );
    }

    #[test]
    fn extensible_pcm16() {
        assert_eq!(
            classify_mix_format(WAVE_FORMAT_EXTENSIBLE, 16, Some(SUBTYPE_PCM)),
            Some(SampleKind::I16)
        );
    }

    #[test]
    fn extensible_pcm32() {
        assert_eq!(
            classify_mix_format(WAVE_FORMAT_EXTENSIBLE, 32, Some(SUBTYPE_PCM)),
            Some(SampleKind::I32)
        );
    }

    #[test]
    fn extensible_unknown_subformat_rejected() {
        assert_eq!(
            classify_mix_format(WAVE_FORMAT_EXTENSIBLE, 32, Some(0xdeadbeef)),
            None
        );
        assert_eq!(classify_mix_format(WAVE_FORMAT_EXTENSIBLE, 32, None), None);
    }

    #[test]
    fn unknown_tag_rejected() {
        assert_eq!(classify_mix_format(0x1234, 16, None), None);
    }

    #[test]
    fn sample_scaling() {
        assert_eq!(i16_to_f32(32767), 32767.0 / 32768.0);
        assert_eq!(i16_to_f32(-32768), -1.0);
        assert_eq!(i32_to_f32(i32::MAX), i32::MAX as f32 / 2147483648.0);
        assert_eq!(i32_to_f32(i32::MIN), -1.0);
    }
}
