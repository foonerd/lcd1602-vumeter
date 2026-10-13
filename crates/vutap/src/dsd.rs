//! One-bit audio, measured by density. A DSD modulator turns amplitude into
//! the share of ones in the bit stream: silence is one half, and 0 dBFS is
//! a swing between a quarter and three quarters. The level of a run of DSD
//! bytes is the strongest swing of that share seen over short groups, on
//! the 16-bit scale the meters use. The order of the bits within a byte or
//! a word does not matter to a count of ones, so the same measure serves
//! native DSD in every ALSA word size and the DSD bytes inside DoP frames.

/// Groups per second: the bandwidth of the measure, the same from DSD64 to
/// DSD512 once the group is sized from the bit rate.
pub const GROUP_HZ: u32 = 8000;
/// Groups whose squared swings are averaged.
const WINDOW: usize = 2;
/// 0 dBFS in DSD is a 50 percent modulation index, so the share of a
/// full-scale signal swings by a quarter either way.
const FULL_SCALE: f64 = 2.0;

/// Bytes per density group for a stream carrying this many DSD bits per
/// second on one channel: DSD64 gives 44, DSD128 88.
pub fn group_bytes(bits_per_second: u64) -> usize {
    ((bits_per_second / GROUP_HZ as u64) / 8).max(2) as usize
}

/// The level of a run of DSD bytes, 0 through 32767. Fewer than `group`
/// bytes read as nothing.
pub fn level<I: Iterator<Item = u8>>(bytes: I, group: usize) -> i32 {
    let group = group.max(1);
    let bits = (group * 8) as f64;
    let mut ring = [0.0f64; WINDOW];
    let mut running = 0.0;
    let mut best = 0.0;
    let mut groups = 0usize;
    let mut ones = 0u32;
    let mut filled = 0usize;
    for b in bytes {
        ones += b.count_ones();
        filled += 1;
        if filled == group {
            let d = (ones as f64 - bits / 2.0) / (bits / 2.0);
            running += d * d - ring[groups % WINDOW];
            ring[groups % WINDOW] = d * d;
            groups += 1;
            if groups >= WINDOW && running > best {
                best = running;
            }
            ones = 0;
            filled = 0;
        }
    }
    if groups == 0 {
        return 0;
    }
    let lev = if groups < WINDOW {
        (running / groups as f64).sqrt()
    } else {
        (best / WINDOW as f64).sqrt()
    };
    let lev = lev * FULL_SCALE * 32767.0;
    if lev > 32767.0 {
        32767
    } else {
        lev as i32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn half_density_is_silence_and_all_ones_is_full_scale() {
        let group = group_bytes(2_822_400);
        assert_eq!(group, 44, "DSD64 groups are 44 bytes");
        assert_eq!(group_bytes(5_644_800), 88);
        let quiet = std::iter::repeat_n(0x55u8, group * 8);
        assert_eq!(level(quiet, group), 0);
        let loud = std::iter::repeat_n(0xFFu8, group * 8);
        assert_eq!(level(loud, group), 32767);
        assert_eq!(
            level(std::iter::repeat_n(0xFFu8, group - 1), group),
            0,
            "less than a group is nothing"
        );
    }

    #[test]
    fn a_quarter_swing_reads_full_scale_and_an_eighth_half() {
        // Groups alternating three quarters and a quarter ones: the 0 dBFS swing.
        let group = 16;
        let swing = |hi: u8, lo: u8| {
            (0..8usize)
                .flat_map(move |g| std::iter::repeat_n(if g % 2 == 0 { hi } else { lo }, group))
        };
        // Six ones in eight then two: three quarters and a quarter.
        let full = swing(0b0111_0111, 0b0001_0001);
        let lev = level(full, group);
        assert!((lev - 32767).abs() < 400, "full scale swing reads {lev}");
        // Five ones then three: an eighth either side of the middle.
        let half = swing(0b0011_0111, 0b0011_0001);
        let lev = level(half, group);
        assert!((lev - 16384).abs() < 400, "half scale swing reads {lev}");
    }
}
