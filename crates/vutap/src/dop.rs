//! DSD over PCM. A DoP stream carries DSD bits in the low sixteen bits of
//! 24-bit frames with a marker byte alternating 0x05 and 0xFA on top; as
//! 16-bit samples (the top sixteen bits of each frame) it looks like
//! full-scale noise with the marker in the high byte and eight DSD bits in
//! the low byte. The level of such a stream is the density of those bits,
//! measured as for native DSD.

const MARKER_A: i32 = 0x05;
const MARKER_B: i32 = 0xFA;
const PROBE: usize = 64;

/// Frames per density group at a sample rate: the group's duration, and so
/// the measurement bandwidth, stays the same from DSD64 to DSD512.
pub fn group_frames(rate: u32) -> usize {
    ((rate / crate::dsd::GROUP_HZ) as usize).max(2)
}

/// Whether the first frames carry the alternating DoP markers.
pub fn is_stream(samples: &[i16]) -> bool {
    let n = samples.len().min(PROBE);
    if n < 8 {
        return false;
    }
    let mut prev = -1;
    for &s in &samples[..n] {
        let m = ((s as i32) >> 8) & 0xFF;
        if (m != MARKER_A && m != MARKER_B) || m == prev {
            return false;
        }
        prev = m;
    }
    true
}

/// The level of a DoP stream on the 16-bit scale, 0 through 32767: the
/// density of the DSD byte each sample carries.
pub fn level(samples: &[i16], group: usize) -> i32 {
    crate::dsd::level(samples.iter().map(|s| *s as u8), group)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markers_tell_dop_from_pcm_and_density_gives_the_level() {
        let dop: Vec<i16> = (0..64)
            .map(|i| (((if i % 2 == 0 { 0x05 } else { 0xFA }) << 8) | 0x55) as i16)
            .collect();
        assert!(is_stream(&dop));
        let pcm: Vec<i16> = (0..64).map(|i| (i * 100) as i16).collect();
        assert!(!is_stream(&pcm));
        assert!(!is_stream(&dop[..4]), "too short to tell");
        // Half density is silence; all ones is beyond full scale.
        let quiet: Vec<i16> = vec![0x0555u16 as i16; 176];
        assert_eq!(level(&quiet, group_frames(176_400)), 0);
        let loud: Vec<i16> = vec![0x05FFu16 as i16; 176];
        assert_eq!(level(&loud, group_frames(176_400)), 32767);
    }
}
