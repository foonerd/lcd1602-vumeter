//! What the tap measures from a stream once its shape is known: the peak
//! and RMS of each channel per hop for linear audio, the density level for
//! one-bit audio, every hop published into a ring of the stream's own. Fed
//! with interleaved samples on the 16-bit scale, one per channel per frame,
//! or for DSD one per byte, as the relay carries them.

use std::path::{Path, PathBuf};

use crate::ring::{Frame, Writer, MAX_CHANNELS};
use crate::sample::Layout;
use crate::{dop, dsd};

/// Frames between measurements when the configuration names none: about
/// 23 ms at 44.1 kHz, 5 ms at 192 kHz.
pub const HOP: usize = 1024;

/// The shape of a stream, from the player's hw_params.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Shape {
    pub rate: u32,
    pub channels: u32,
    pub layout: Layout,
}

/// How the measurements are made and where they go.
#[derive(Clone, Debug)]
pub struct Settings {
    /// The tag in the ring's file name.
    pub ring: String,
    /// Frames between measurements; zero means [`HOP`].
    pub hop: usize,
    /// Hops the ring keeps.
    pub slots: usize,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            ring: "vutap".to_string(),
            hop: 0,
            slots: 64,
        }
    }
}

/// The running sums of one hop in progress, per channel.
#[derive(Clone, Copy, Default)]
struct Partial {
    peak: i32,
    squares: f64,
    frames: usize,
}

pub struct Measure {
    settings: Settings,
    dir: PathBuf,
    shape: Option<Shape>,
    writer: Option<Writer>,
    hop: usize,
    partial: [Partial; MAX_CHANNELS],
    /// Bytes per density group for one-bit audio, frames for DoP.
    group: usize,
    chans: [Vec<i16>; MAX_CHANNELS],
}

impl Measure {
    /// Rings go under `dir`.
    pub fn new(dir: impl Into<PathBuf>, settings: Settings) -> Self {
        let hop = if settings.hop == 0 { HOP } else { settings.hop };
        Self {
            settings,
            dir: dir.into(),
            shape: None,
            writer: None,
            hop,
            partial: [Partial::default(); MAX_CHANNELS],
            group: 0,
            chans: [Vec::new(), Vec::new()],
        }
    }

    pub fn shape(&self) -> Option<Shape> {
        self.shape
    }

    /// Frames between measurements.
    pub fn hop(&self) -> usize {
        self.hop
    }

    /// The ring's file while a stream has one.
    pub fn ring_path(&self) -> Option<&Path> {
        self.writer.as_ref().map(|w| w.path())
    }

    /// A stream of this shape starts, or none: the old ring goes, a new one
    /// is made. Returns what went wrong making the ring, if anything; the
    /// measuring goes on without it.
    pub fn set_shape(&mut self, shape: Option<Shape>) -> Result<(), String> {
        self.shape = shape;
        self.writer = None;
        self.partial = [Partial::default(); MAX_CHANNELS];
        let Some(s) = shape else {
            return Ok(());
        };
        self.group = if s.layout.is_dsd() {
            dsd::group_bytes(s.rate as u64 * s.layout.bytes() as u64 * 8)
        } else {
            dop::group_frames(s.rate)
        };
        match Writer::create(
            &self.dir,
            &self.settings.ring,
            s.rate,
            s.channels,
            self.hop as u32,
            self.settings.slots,
        ) {
            Ok(w) => {
                self.writer = Some(w);
                Ok(())
            }
            Err(e) => Err(format!("no ring: {e}")),
        }
    }

    /// Forget the hop in progress; the next starts from silence.
    pub fn reset(&mut self) {
        self.partial = [Partial::default(); MAX_CHANNELS];
    }

    /// Measure `samples`, interleaved as the stream's shape says, and hand
    /// `sink` every hop they complete, with how many of these samples'
    /// frames had gone in when the hop completed, so the caller can place
    /// the hop in time. Frames are words for one-bit audio.
    pub fn take(&mut self, samples: &[i16], sink: &mut dyn FnMut(&Frame, usize)) {
        let Some(shape) = self.shape else {
            return;
        };
        let channels = (shape.channels as usize).max(1);
        let n = samples.len() / channels;
        if n == 0 {
            return;
        }
        let used = channels.min(MAX_CHANNELS);
        for (ch, out) in self.chans.iter_mut().enumerate().take(used) {
            out.clear();
            out.extend(samples.iter().skip(ch).step_by(channels).take(n));
        }
        if shape.layout.is_dsd() || dop::is_stream(&self.chans[0]) {
            self.take_one_bit(shape, n, used, sink);
            return;
        }
        let mut frame = Frame::default();
        let mut start = 0;
        while start < n {
            let room = self.hop - self.partial[0].frames;
            let end = (start + room).min(n);
            for ch in 0..used {
                let p = &mut self.partial[ch];
                for &s in &self.chans[ch][start..end] {
                    let v = s as i32;
                    p.peak = p.peak.max(v.abs());
                    p.squares += (v as f64) * (v as f64);
                }
                p.frames += end - start;
            }
            if self.partial[0].frames >= self.hop {
                for ch in 0..MAX_CHANNELS {
                    let p = self.partial[ch.min(used - 1)];
                    frame.peak[ch] = (p.peak.min(32767) as f32) / 32767.0;
                    frame.rms[ch] =
                        ((p.squares / p.frames.max(1) as f64).sqrt() / 32767.0).min(1.0) as f32;
                }
                frame.one_bit = false;
                sink(&frame, end);
                self.partial = [Partial::default(); MAX_CHANNELS];
            }
            start = end;
        }
    }

    /// One-bit audio has no amplitude per sample; the level of each hop's
    /// worth of words stands in, so the meter moves as the audio does.
    fn take_one_bit(
        &mut self,
        shape: Shape,
        n: usize,
        used: usize,
        sink: &mut dyn FnMut(&Frame, usize),
    ) {
        let per_frame = if shape.layout.is_dsd() {
            shape.layout.bytes().max(1)
        } else {
            1
        };
        let block = (self.hop * per_frame).max(1);
        let group = self.group;
        let level = |s: &[i16]| dsd::level(s.iter().map(|v| *v as u8), group) as f32 / 32767.0;
        let mut frame = Frame {
            one_bit: true,
            ..Frame::default()
        };
        let mut start = 0;
        while start < n {
            let end = (start + block).min(n);
            let left = level(&self.chans[0][start..end]);
            let right = if used > 1 {
                level(&self.chans[1][start..end])
            } else {
                left
            };
            frame.peak = [left, right];
            frame.rms = [left, right];
            sink(&frame, end / per_frame);
            start = end;
        }
    }

    /// Put a frame into the stream's ring, if it has one.
    pub fn publish(&mut self, frame: &Frame) {
        if let Some(w) = self.writer.as_mut() {
            w.publish(frame);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ring::Reader;

    fn dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("vutap-measure-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn settings() -> Settings {
        Settings {
            ring: "test".into(),
            hop: 128,
            slots: 8,
        }
    }

    #[test]
    fn linear_audio_is_measured_into_the_stream_s_ring() {
        let d = dir("linear");
        let mut m = Measure::new(&d, settings());
        assert!(m.ring_path().is_none());
        let shape = Shape {
            rate: 48_000,
            channels: 2,
            layout: Layout::from_alsa(2).unwrap(),
        };
        m.set_shape(Some(shape)).unwrap();
        let path = m.ring_path().unwrap().to_path_buf();
        assert!(path.exists());
        // A half-scale left channel and a quiet right one, interleaved.
        let mut samples = Vec::new();
        for i in 0..1024 {
            let s = (0.5
                * (2.0 * std::f32::consts::PI * 1000.0 * i as f32 / 48_000.0).sin()
                * 32767.0) as i16;
            samples.push(s);
            samples.push(0);
        }
        let mut got: Vec<(Frame, usize)> = Vec::new();
        m.take(&samples, &mut |f, n| got.push((f.clone(), n)));
        for (f, _) in &got {
            m.publish(f);
        }
        assert_eq!(
            got.iter().map(|g| g.1).collect::<Vec<_>>(),
            (1..=8).map(|k| k * 128).collect::<Vec<_>>(),
            "each hop says how many frames had gone in"
        );
        let reader = Reader::open(&path).unwrap();
        let frame = reader.latest().expect("a frame after eight hops");
        assert!(
            (frame.peak[0] - 0.5).abs() < 0.02,
            "left peak {}",
            frame.peak[0]
        );
        // A sine's RMS is its peak over the square root of two.
        assert!(
            (frame.rms[0] - 0.5 / 2f32.sqrt()).abs() < 0.02,
            "left rms {}",
            frame.rms[0]
        );
        assert_eq!(frame.peak[1], 0.0);
        assert_eq!(frame.rms[1], 0.0);
        assert!(!frame.one_bit);
        assert_eq!(reader.info().rate, 48_000);
        // A hop spans two takes: 100 frames then 28 complete it.
        let mut got2 = Vec::new();
        m.take(&samples[..200], &mut |f, n| got2.push((f.clone(), n)));
        assert!(got2.is_empty());
        m.take(&samples[200..256], &mut |f, n| got2.push((f.clone(), n)));
        assert_eq!(got2.len(), 1);
        assert_eq!(got2[0].1, 28);
        // A new shape makes a new ring and the old file goes.
        m.set_shape(Some(Shape {
            rate: 44_100,
            ..shape
        }))
        .unwrap();
        assert!(!path.exists());
        assert_ne!(m.ring_path().unwrap(), path);
        m.set_shape(None).unwrap();
        assert!(m.ring_path().is_none());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn mono_fills_both_channels() {
        let d = dir("mono");
        let mut m = Measure::new(&d, settings());
        m.set_shape(Some(Shape {
            rate: 48_000,
            channels: 1,
            layout: Layout::from_alsa(2).unwrap(),
        }))
        .unwrap();
        let samples: Vec<i16> = vec![16384; 128];
        let mut got = Vec::new();
        m.take(&samples, &mut |f, _| got.push(f.clone()));
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].peak[0], got[0].peak[1]);
        assert!((got[0].peak[0] - 0.5).abs() < 0.001);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn one_bit_audio_is_measured_by_density_and_dop_is_told_apart() {
        let d = dir("dsd");
        let mut m = Measure::new(&d, settings());
        let dsd_shape = Shape {
            rate: 88_200,
            channels: 2,
            layout: Layout::from_alsa(50).unwrap(),
        };
        m.set_shape(Some(dsd_shape)).unwrap();
        let path = m.ring_path().unwrap().to_path_buf();
        // Bytes at half density on the left, all ones on the right: quiet and beyond full.
        let group = dsd::group_bytes(88_200 * 32);
        let mut samples = Vec::new();
        for _ in 0..group * 8 {
            samples.push(0x55i16);
            samples.push(0xFFi16);
        }
        let mut got: Vec<(Frame, usize)> = Vec::new();
        m.take(&samples, &mut |f, n| got.push((f.clone(), n)));
        for (f, _) in &got {
            m.publish(f);
        }
        let reader = Reader::open(&path).unwrap();
        let frame = reader.latest().expect("a density frame");
        assert_eq!(frame.peak[0], 0.0);
        assert_eq!(frame.peak[1], 1.0);
        assert!(frame.one_bit);
        // DoP inside 24-bit frames: markers in the high byte, DSD bits below.
        let dop_shape = Shape {
            rate: 176_400,
            channels: 2,
            layout: Layout::from_alsa(32).unwrap(),
        };
        m.set_shape(Some(dop_shape)).unwrap();
        let path = m.ring_path().unwrap().to_path_buf();
        let mut samples = Vec::new();
        for i in 0..2048 {
            let marker = if i % 2 == 0 { 0x05 } else { 0xFA };
            samples.push(((marker << 8) | 0xFF) as i16);
            samples.push(((marker << 8) | 0x55) as i16);
        }
        let mut got: Vec<(Frame, usize)> = Vec::new();
        m.take(&samples, &mut |f, n| got.push((f.clone(), n)));
        for (f, _) in &got {
            m.publish(f);
        }
        let frame = Reader::open(&path).unwrap().latest().expect("a DoP frame");
        assert_eq!(frame.peak[0], 1.0, "all ones is beyond full scale");
        assert_eq!(frame.peak[1], 0.0, "half density is silence");
        assert!(frame.one_bit);
        let _ = std::fs::remove_dir_all(&d);
    }
}
