//! The meter's ballistics: from the tap's hops to a level per channel that
//! rises quickly, falls at a VU meter's pace, and a peak that holds and
//! then drops. Levels are in dB below full scale, with the gain applied;
//! the bars are drawn from them.

use std::time::Duration;

use vutap::Frame;

use crate::config::Source;

/// How quickly the level follows a rise and a fall.
const ATTACK: Duration = Duration::from_millis(40);
const RELEASE: Duration = Duration::from_millis(300);
/// How quickly a held peak falls once its hold is over.
const PEAK_FALL_DB_PER_S: f32 = 40.0;
/// Below this, the meter is at rest.
const SILENCE_DB: f32 = -100.0;

#[derive(Clone, Copy, Debug)]
struct Channel {
    level_db: f32,
    peak_db: f32,
    /// Time since the peak was last raised.
    peak_age: Duration,
}

impl Default for Channel {
    fn default() -> Self {
        Self {
            level_db: SILENCE_DB,
            peak_db: SILENCE_DB,
            peak_age: Duration::ZERO,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Meter {
    channels: [Channel; 2],
    pub source: Source,
    pub gain_db: f32,
    pub range_db: f32,
    pub hold: Duration,
}

/// A linear level as dB below full scale.
pub fn to_db(level: f32) -> f32 {
    if level <= 0.0 {
        SILENCE_DB
    } else {
        // Through ln, not log10: glibc 2.43 gives log10f a new symbol version,
        // and a binary built on such a host would not load on Volumio's 2.36.
        (20.0 * std::f32::consts::LOG10_E * level.ln()).max(SILENCE_DB)
    }
}

fn approach(current: f32, target: f32, dt: Duration, tau: Duration) -> f32 {
    let k = 1.0 - (-dt.as_secs_f32() / tau.as_secs_f32().max(1e-6)).exp();
    current + (target - current) * k
}

impl Meter {
    pub fn new(source: Source, gain_db: f32, range_db: f32, hold: Duration) -> Meter {
        Meter {
            channels: [Channel::default(); 2],
            source,
            gain_db,
            range_db,
            hold,
        }
    }

    /// A hop (or several merged) arrived `dt` after the last look.
    pub fn feed(&mut self, frame: &Frame, dt: Duration) {
        for (ch, c) in self.channels.iter_mut().enumerate() {
            let value = match self.source {
                Source::Rms => frame.rms[ch],
                Source::Peak => frame.peak[ch],
            };
            let target = to_db(value) + self.gain_db;
            let tau = if target > c.level_db { ATTACK } else { RELEASE };
            c.level_db = approach(c.level_db, target, dt, tau);
            let peak = to_db(frame.peak[ch]) + self.gain_db;
            if peak >= c.peak_db {
                c.peak_db = peak;
                c.peak_age = Duration::ZERO;
            } else {
                Self::age_peak(c, dt, self.hold);
            }
        }
    }

    /// Nothing arrived for `dt`: the level falls, the peak ages.
    pub fn decay(&mut self, dt: Duration) {
        for c in self.channels.iter_mut() {
            c.level_db = approach(c.level_db, SILENCE_DB, dt, RELEASE);
            Self::age_peak(c, dt, self.hold);
        }
    }

    fn age_peak(c: &mut Channel, dt: Duration, hold: Duration) {
        c.peak_age += dt;
        if c.peak_age > hold {
            c.peak_db = (c.peak_db - PEAK_FALL_DB_PER_S * dt.as_secs_f32()).max(SILENCE_DB);
        }
    }

    /// Everything at rest, at once.
    pub fn reset(&mut self) {
        self.channels = [Channel::default(); 2];
    }

    /// The share of the range the level fills, 0 to 1.
    fn fraction(&self, db: f32) -> f32 {
        ((db + self.range_db) / self.range_db).clamp(0.0, 1.0)
    }

    /// The level of channel `ch` in steps of `total`, and the peak's step
    /// when it stands above the level and above rest.
    pub fn steps(&self, ch: usize, total: u16, with_peak: bool) -> (u16, Option<u16>) {
        let c = &self.channels[ch.min(1)];
        let level = (self.fraction(c.level_db) * total as f32).round() as u16;
        let peak_fraction = self.fraction(c.peak_db);
        let peak = if with_peak && peak_fraction > 0.0 {
            let step = (peak_fraction * total as f32).round().max(1.0) as u16 - 1;
            (step >= level).then_some(step)
        } else {
            None
        };
        (level.min(total), peak)
    }

    #[cfg(test)]
    pub fn level_db(&self, ch: usize) -> f32 {
        self.channels[ch.min(1)].level_db
    }

    /// The louder channel's share of the range, for a backlight that follows the level.
    pub fn loudness(&self) -> f32 {
        self.channels
            .iter()
            .map(|c| self.fraction(c.level_db))
            .fold(0.0, f32::max)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(level: f32) -> Frame {
        Frame {
            peak: [level, level],
            rms: [level, level],
            ..Frame::default()
        }
    }

    #[test]
    fn full_scale_fills_the_bar_and_silence_empties_it() {
        let mut m = Meter::new(Source::Peak, 0.0, 40.0, Duration::from_secs(1));
        // Enough time for the attack to settle.
        for _ in 0..50 {
            m.feed(&frame(1.0), Duration::from_millis(20));
        }
        assert_eq!(
            m.steps(0, 75, true),
            (75, None),
            "full, the peak hidden under the bar"
        );
        assert!((m.level_db(0) - 0.0).abs() < 0.1);
        for _ in 0..200 {
            m.decay(Duration::from_millis(20));
        }
        let (level, peak) = m.steps(0, 75, true);
        assert_eq!(level, 0);
        assert_eq!(
            peak, None,
            "after the hold the peak has fallen through the floor"
        );
    }

    #[test]
    fn the_peak_holds_while_the_level_falls() {
        let mut m = Meter::new(Source::Peak, 0.0, 40.0, Duration::from_secs(1));
        for _ in 0..50 {
            m.feed(&frame(0.5), Duration::from_millis(20));
        }
        let (level, _) = m.steps(0, 75, true);
        // -6 dB on a 40 dB range is 85 percent of the bar.
        assert!((63..=65).contains(&level), "level {level}");
        for _ in 0..10 {
            m.feed(&frame(0.05), Duration::from_millis(20));
        }
        let (level, peak) = m.steps(0, 75, true);
        assert!(level < 60, "the level fell to {level}");
        assert_eq!(peak, Some(63), "the peak stands where the level was");
        assert!(m.loudness() > 0.5);
    }

    #[test]
    fn gain_moves_the_level_and_the_source_picks_rms_or_peak() {
        let f = Frame {
            peak: [0.5, 0.5],
            rms: [0.25, 0.25],
            ..Frame::default()
        };
        let mut peak = Meter::new(Source::Peak, 6.0, 40.0, Duration::from_secs(1));
        let mut rms = Meter::new(Source::Rms, 0.0, 40.0, Duration::from_secs(1));
        for _ in 0..50 {
            peak.feed(&f, Duration::from_millis(20));
            rms.feed(&f, Duration::from_millis(20));
        }
        assert!(
            (peak.level_db(0) - 0.0).abs() < 0.1,
            "-6 dB plus 6 dB of gain"
        );
        assert!((rms.level_db(0) + 12.0).abs() < 0.1, "RMS at -12 dB");
        assert_eq!(to_db(0.0), SILENCE_DB);
        assert!((to_db(1.0)).abs() < 1e-6);
    }
}
