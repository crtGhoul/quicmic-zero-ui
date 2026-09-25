//! PC-to-phone speaker streaming: WASAPI loopback capture (Windows) or a
//! synthetic test tone, resampled to 48 kHz stereo and fanned out as 20 ms
//! PCM frames over the `/speaker-ws` WebSocket.
//!
//! Wire format: every binary WS message is exactly one frame =
//! 960 samples/channel * 2 channels * 4 bytes = 7680 bytes, little-endian f32,
//! interleaved stereo. No headers, no negotiation.

pub mod format;
pub mod resample;
pub mod synth;
#[cfg(windows)]
pub mod wasapi;

/// Samples per channel in one frame (20 ms at 48 kHz).
pub const FRAME_SAMPLES: usize = 960;
pub const CHANNELS: usize = 2;
pub const SAMPLE_RATE: u32 = 48_000;
/// Exact byte length of one binary WS message.
pub const FRAME_BYTES: usize = FRAME_SAMPLES * CHANNELS * 4;

/// Resolve a user-supplied device selector against a device-name list.
/// A bare number picks the `[n]` index from the list; anything else is a
/// case-insensitive substring match on the friendly name.
/// Pure string logic — shared by the `--speaker-device` flag and the
/// `speaker-device` console command, and unit-tested here.
pub fn resolve_index(names: &[String], sel: &str) -> Option<usize> {
    if let Ok(i) = sel.parse::<usize>() {
        return names.get(i).map(|_| i);
    }
    let needle = sel.to_ascii_lowercase();
    names
        .iter()
        .position(|n| n.to_lowercase().contains(&needle))
}

/// Validate a `speaker-device` argument against the enumerated render
/// endpoints. Returns the canonical friendly name to store in the selection
/// (`None` = follow the system default render endpoint).
pub fn resolve_name(names: &[String], arg: &str) -> Result<Option<String>, String> {
    if arg.eq_ignore_ascii_case("default") {
        return Ok(None);
    }
    match resolve_index(names, arg) {
        Some(i) => Ok(Some(names[i].clone())),
        None => Err(format!(
            "No speaker device matching '{arg}'. Type 'speaker-devices' for the list."
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names() -> Vec<String> {
        vec![
            "Speakers (Realtek Audio)".into(),
            "Headphones (OnePlus Buds)".into(),
            "CABLE Input (VB-Audio Virtual Cable)".into(),
        ]
    }

    #[test]
    fn index_selects_by_position() {
        assert_eq!(resolve_index(&names(), "0"), Some(0));
        assert_eq!(resolve_index(&names(), "2"), Some(2));
        assert_eq!(resolve_index(&names(), "7"), None);
    }

    #[test]
    fn substring_match_is_case_insensitive() {
        assert_eq!(resolve_index(&names(), "headphones"), Some(1));
        assert_eq!(resolve_index(&names(), "ONEPLUS"), Some(1));
        assert_eq!(resolve_index(&names(), "cable"), Some(2));
        assert_eq!(resolve_index(&names(), "nope"), None);
    }

    #[test]
    fn first_match_wins() {
        let n = vec!["Speakers A".into(), "Speakers B".into()];
        assert_eq!(resolve_index(&n, "speakers"), Some(0));
    }

    #[test]
    fn resolve_name_default_and_errors() {
        assert_eq!(resolve_name(&names(), "default"), Ok(None));
        assert_eq!(resolve_name(&names(), "DEFAULT"), Ok(None));
        assert_eq!(
            resolve_name(&names(), "buds"),
            Ok(Some("Headphones (OnePlus Buds)".into()))
        );
        assert!(resolve_name(&names(), "nope").is_err());
        assert!(resolve_name(&names(), "9").is_err());
    }
}
