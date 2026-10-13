//! The tap's measurements and the ring they travel through. The tap sits in
//! the player's ALSA chain (`bins/vutap-alsa`), passes every stream through
//! byte for byte, and measures it on a thread of its own: the peak and RMS
//! of each channel for linear audio, the density level of one-bit audio.
//! The display (`bins/lcd1602-vumeter`) reads the ring.
//!
//! The sample layouts, the relay and the one-bit measures descend from the
//! glass tap (foonerd/glass, MIT).

pub mod dop;
pub mod dsd;
pub mod measure;
pub mod relay;
pub mod ring;
pub mod sample;

pub use ring::{Frame, Reader, Writer, MAX_CHANNELS};
