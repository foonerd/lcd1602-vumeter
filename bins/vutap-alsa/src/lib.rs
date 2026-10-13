//! `vutap`: the tap inside the audio player, as an ALSA PCM (`type vutap`
//! in the player's chain, with a `slave` like any filter plugin). The
//! stream passes through byte for byte in the format the player and the
//! slave agreed, so PCM, DoP and native DSD stay bit-perfect. On the way
//! the audio thread copies the samples across the relay, and a thread of
//! the tap's own measures them and publishes into the ring: the peak and
//! RMS of linear audio, the density level of one-bit audio.
//!
//! ALSA loads this library into the audio player's process and calls it
//! from the thread that writes audio. Every entry point catches a panic,
//! nothing allocates once the stream is set up, and nothing blocks.
//!
//! Configuration keys beside `slave`: `ring` (a tag in the ring's file
//! name), `hop` (frames between measurements, default 1024) and `slots`
//! (hops kept in the ring, default 64).
//!
//! The ALSA side descends from the glass tap (foonerd/glass, MIT).

use std::collections::VecDeque;
use std::ffi::{c_char, c_int, c_long, c_uint, c_ulong, c_void, CStr, CString};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use vutap::measure::{self, Measure, Shape};
use vutap::relay::{Chunk, Relay};
use vutap::ring::{now_ns, Frame};
use vutap::sample::Layout;

#[allow(non_camel_case_types)]
mod ffi {
    use super::*;

    #[repr(C)]
    pub struct snd_pcm_t {
        _private: [u8; 0],
    }
    #[repr(C)]
    pub struct snd_config_t {
        _private: [u8; 0],
    }
    #[repr(C)]
    pub struct snd_pcm_hw_params_t {
        _private: [u8; 0],
    }
    #[repr(C)]
    pub struct snd_output_t {
        _private: [u8; 0],
    }
    #[repr(C)]
    pub struct snd_pcm_channel_area_t {
        pub addr: *mut c_void,
        pub first: c_uint,
        pub step: c_uint,
    }
    pub type snd_config_iterator_t = *mut c_void;

    /// The filter plugin handle, as libasound lays it out (protocol 1.0.2).
    #[repr(C)]
    pub struct snd_pcm_extplug_t {
        pub version: c_uint,
        pub name: *const c_char,
        pub callback: *const snd_pcm_extplug_callback_t,
        pub private_data: *mut c_void,
        pub pcm: *mut snd_pcm_t,
        pub stream: c_int,
        pub format: c_int,
        pub subformat: c_int,
        pub channels: c_uint,
        pub rate: c_uint,
        pub slave_format: c_int,
        pub slave_subformat: c_int,
        pub slave_channels: c_uint,
    }

    pub type Transfer = unsafe extern "C" fn(
        *mut snd_pcm_extplug_t,
        *const snd_pcm_channel_area_t,
        c_ulong,
        *const snd_pcm_channel_area_t,
        c_ulong,
        c_ulong,
    ) -> c_long;

    /// The callbacks, in the order libasound declares them.
    #[repr(C)]
    pub struct snd_pcm_extplug_callback_t {
        pub transfer: Option<Transfer>,
        pub close: Option<unsafe extern "C" fn(*mut snd_pcm_extplug_t) -> c_int>,
        pub hw_params:
            Option<unsafe extern "C" fn(*mut snd_pcm_extplug_t, *mut snd_pcm_hw_params_t) -> c_int>,
        pub hw_free: Option<unsafe extern "C" fn(*mut snd_pcm_extplug_t) -> c_int>,
        pub dump: Option<unsafe extern "C" fn(*mut snd_pcm_extplug_t, *mut snd_output_t)>,
        pub init: Option<unsafe extern "C" fn(*mut snd_pcm_extplug_t) -> c_int>,
        pub query_chmaps: Option<unsafe extern "C" fn(*mut snd_pcm_extplug_t) -> *mut c_void>,
        pub get_chmap: Option<unsafe extern "C" fn(*mut snd_pcm_extplug_t) -> *mut c_void>,
        pub set_chmap: Option<unsafe extern "C" fn(*mut snd_pcm_extplug_t, *const c_void) -> c_int>,
    }

    pub const SND_PCM_EXTPLUG_VERSION: c_uint = (1 << 16) | 2;
    pub const SND_PCM_STREAM_PLAYBACK: c_int = 0;

    extern "C" {
        pub fn snd_pcm_extplug_create(
            ext: *mut snd_pcm_extplug_t,
            name: *const c_char,
            root: *mut snd_config_t,
            slave_conf: *mut snd_config_t,
            stream: c_int,
            mode: c_int,
        ) -> c_int;
        pub fn snd_pcm_areas_copy(
            dst: *const snd_pcm_channel_area_t,
            dst_offset: c_ulong,
            src: *const snd_pcm_channel_area_t,
            src_offset: c_ulong,
            channels: c_uint,
            frames: c_ulong,
            format: c_int,
        ) -> c_int;
        pub fn snd_config_search(
            config: *mut snd_config_t,
            key: *const c_char,
            result: *mut *mut snd_config_t,
        ) -> c_int;
        pub fn snd_config_iterator_first(node: *const snd_config_t) -> snd_config_iterator_t;
        pub fn snd_config_iterator_next(it: snd_config_iterator_t) -> snd_config_iterator_t;
        pub fn snd_config_iterator_end(node: *const snd_config_t) -> snd_config_iterator_t;
        pub fn snd_config_iterator_entry(it: snd_config_iterator_t) -> *mut snd_config_t;
        pub fn snd_config_get_id(node: *const snd_config_t, id: *mut *const c_char) -> c_int;
        pub fn snd_config_get_integer(node: *const snd_config_t, value: *mut c_long) -> c_int;
        pub fn snd_config_get_string(node: *const snd_config_t, value: *mut *const c_char)
            -> c_int;
        pub fn snd_pcm_hw_params_get_buffer_size(
            params: *const snd_pcm_hw_params_t,
            val: *mut c_ulong,
        ) -> c_int;
        pub fn snd_pcm_hw_params_get_period_size(
            params: *const snd_pcm_hw_params_t,
            val: *mut c_ulong,
            dir: *mut c_int,
        ) -> c_int;
        pub fn snd_pcm_delay(pcm: *mut snd_pcm_t, delayp: *mut c_long) -> c_int;
    }
}

use ffi::*;

/// Samples the relay holds: two thirds of a second of stereo at 384 kHz,
/// a fifth of a second of DSD256; a player writes at most its buffer
/// ahead, and the measuring thread drains as the hops fall due.
const RELAY_SAMPLES: usize = 1 << 19;
/// How long the measuring thread sleeps between looks when nothing comes.
const WAIT_MS: c_int = 100;
/// Hops held back for their time, at most; beyond that the stream has
/// stalled and the oldest go.
const PENDING_MAX: usize = 1024;

fn note(msg: &str) {
    eprintln!("vutap: {msg}");
}

/// What the configuration asked for.
struct Settings {
    hop: usize,
    ring: String,
    slots: usize,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            hop: 0,
            ring: "vutap".to_string(),
            slots: 64,
        }
    }
}

/// Read the tap's configuration node.
unsafe fn settings_from(conf: *const snd_config_t) -> Settings {
    let mut s = Settings::default();
    let end = snd_config_iterator_end(conf);
    let mut it = snd_config_iterator_first(conf);
    while it != end {
        let node = snd_config_iterator_entry(it);
        it = snd_config_iterator_next(it);
        let mut id: *const c_char = std::ptr::null();
        if snd_config_get_id(node, &mut id) < 0 || id.is_null() {
            continue;
        }
        let key = CStr::from_ptr(id).to_string_lossy().into_owned();
        let integer = |node: *const snd_config_t| -> Option<i64> {
            let mut v: c_long = 0;
            if snd_config_get_integer(node, &mut v) < 0 {
                None
            } else {
                Some(v as i64)
            }
        };
        let string = |node: *const snd_config_t| -> Option<String> {
            let mut v: *const c_char = std::ptr::null();
            if snd_config_get_string(node, &mut v) < 0 || v.is_null() {
                None
            } else {
                Some(CStr::from_ptr(v).to_string_lossy().into_owned())
            }
        };
        match key.as_str() {
            "comment" | "type" | "slave" | "hint" => {}
            "hop" => s.hop = integer(node).unwrap_or(0).clamp(0, 32_768) as usize,
            "slots" => s.slots = integer(node).unwrap_or(64).clamp(2, 4096) as usize,
            "ring" => {
                s.ring = string(node)
                    .filter(|t| {
                        !t.is_empty()
                            && t.chars()
                                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
                    })
                    .unwrap_or_else(|| "vutap".to_string())
            }
            other => note(&format!("unknown key {other} ignored")),
        }
    }
    s
}

/// What the two threads share.
struct Shared {
    relay: Relay,
    /// The stream's shape, from hw_params to the measuring thread.
    shape: Mutex<Option<Shape>>,
    /// Frames the player keeps ahead of what is heard, as the buffer and
    /// period it asked for say: a period written is heard after them. The
    /// fallback when the PCM cannot say its delay.
    lead: AtomicU32,
    /// The tap's own PCM, for the measuring thread to ask how many frames
    /// stand between the last one written and the sound. libasound locks
    /// each PCM for the call, so the audio thread's transfer and this
    /// thread's question take turns; the transfer itself never asks.
    pcm: AtomicPtr<snd_pcm_t>,
    /// Bumped by every hw_params; the measuring thread starts afresh.
    generation: AtomicU32,
    reset: AtomicBool,
    stop: AtomicBool,
}

/// The plugin's private data: what the audio thread keeps.
struct Tap {
    shared: Arc<Shared>,
    worker: Option<JoinHandle<()>>,
    layout: Option<Layout>,
    channels: usize,
    playback: bool,
    /// Samples of one transfer on their way to the relay, never grown.
    scratch: Vec<i16>,
    /// Set after a panic: the tap copies and nothing more.
    failed: bool,
}

fn tap<'a>(ext: *mut snd_pcm_extplug_t) -> Option<&'a mut Tap> {
    let p = unsafe { (*ext).private_data } as *mut Tap;
    if p.is_null() {
        None
    } else {
        Some(unsafe { &mut *p })
    }
}

impl Tap {
    /// The samples of `frames` frames from `areas`, on the 16-bit scale or
    /// as DSD bytes, across the relay.
    unsafe fn hand_over(
        &mut self,
        areas: *const snd_pcm_channel_area_t,
        offset: usize,
        frames: usize,
    ) {
        let Some(layout) = self.layout else {
            return;
        };
        // The whole transfer arrived now; its hops are placed from here.
        let now = now_ns();
        let width = layout.bytes();
        let dsd = layout.is_dsd();
        let capacity = self.scratch.capacity();
        for f in 0..frames {
            for ch in 0..self.channels {
                let a = &*areas.add(ch);
                let step = a.step as usize;
                let base = (a.addr as *const u8).add((a.first as usize + (offset + f) * step) / 8);
                let bytes = std::slice::from_raw_parts(base, width);
                if dsd {
                    for b in bytes {
                        if self.scratch.len() == capacity {
                            self.shared.relay.push(&self.scratch, now);
                            self.scratch.clear();
                        }
                        self.scratch.push(*b as i16);
                    }
                } else {
                    if self.scratch.len() == capacity {
                        self.shared.relay.push(&self.scratch, now);
                        self.scratch.clear();
                    }
                    self.scratch.push(layout.to_i16(bytes));
                }
            }
        }
        if !self.scratch.is_empty() {
            self.shared.relay.push(&self.scratch, now);
            self.scratch.clear();
        }
    }
}

unsafe extern "C" fn transfer(
    ext: *mut snd_pcm_extplug_t,
    dst_areas: *const snd_pcm_channel_area_t,
    dst_offset: c_ulong,
    src_areas: *const snd_pcm_channel_area_t,
    src_offset: c_ulong,
    size: c_ulong,
) -> c_long {
    // The audio first: it goes through whatever else happens.
    let err = snd_pcm_areas_copy(
        dst_areas,
        dst_offset,
        src_areas,
        src_offset,
        (*ext).channels,
        size,
        (*ext).format,
    );
    if err < 0 {
        return err as c_long;
    }
    if let Some(t) = tap(ext) {
        if !t.failed && t.playback {
            let outcome = catch_unwind(AssertUnwindSafe(|| {
                t.hand_over(src_areas, src_offset as usize, size as usize)
            }));
            if outcome.is_err() {
                t.failed = true;
                note("hand-over failed; the tap copies and measures no more");
            }
        }
    }
    size as c_long
}

unsafe extern "C" fn hw_params(
    ext: *mut snd_pcm_extplug_t,
    params: *mut snd_pcm_hw_params_t,
) -> c_int {
    let Some(t) = tap(ext) else {
        return 0;
    };
    let e = &*ext;
    let outcome = catch_unwind(AssertUnwindSafe(|| {
        t.playback = e.stream == SND_PCM_STREAM_PLAYBACK;
        t.channels = (e.channels as usize).max(1);
        t.layout = Layout::from_alsa(e.format);
        if t.layout.is_none() {
            note(&format!("format {} passes unmeasured", e.format));
        }
        // A player writes a period when one has played: what it writes is
        // heard after the rest of its buffer.
        let mut buffer: c_ulong = 0;
        let mut period: c_ulong = 0;
        let mut dir: c_int = 0;
        let lead = if snd_pcm_hw_params_get_buffer_size(params, &mut buffer) == 0
            && snd_pcm_hw_params_get_period_size(params, &mut period, &mut dir) == 0
        {
            buffer.saturating_sub(period)
        } else {
            0
        };
        t.shared
            .lead
            .store(lead.min(u32::MAX as c_ulong) as u32, Ordering::Release);
        let shape = t.layout.map(|layout| Shape {
            rate: e.rate,
            channels: e.channels,
            layout,
        });
        if let Ok(mut s) = t.shared.shape.lock() {
            *s = shape;
        }
        t.shared.generation.fetch_add(1, Ordering::Release);
        t.shared.relay.wake();
    }));
    if outcome.is_err() {
        t.failed = true;
        note("hw_params failed; the tap copies and measures no more");
    }
    0
}

unsafe extern "C" fn init(ext: *mut snd_pcm_extplug_t) -> c_int {
    if let Some(t) = tap(ext) {
        t.shared.reset.store(true, Ordering::Release);
    }
    0
}

unsafe extern "C" fn close(ext: *mut snd_pcm_extplug_t) -> c_int {
    let p = (*ext).private_data as *mut Tap;
    if !p.is_null() {
        (*ext).private_data = std::ptr::null_mut();
        let mut t = Box::from_raw(p);
        t.shared.stop.store(true, Ordering::Release);
        t.shared.relay.wake();
        if let Some(worker) = t.worker.take() {
            let _ = worker.join();
        }
    }
    // Made by Box::leak in the open function; libasound is done with it.
    drop(Box::from_raw(ext));
    0
}

static CALLBACKS: snd_pcm_extplug_callback_t = snd_pcm_extplug_callback_t {
    transfer: Some(transfer),
    close: Some(close),
    hw_params: Some(hw_params),
    hw_free: None,
    dump: None,
    init: Some(init),
    query_chmaps: None,
    get_chmap: None,
    set_chmap: None,
};

static NAME: &[u8] = b"vutap\0";

/// The measuring thread: takes what the relay holds, measures it for the
/// stream's shape, and puts each hop into the ring when its audio is
/// heard: a chunk is heard from the lead after it arrived, and a hop when
/// its last frame plays. So a period written at once fills the ring
/// evenly, and the meters keep step with the sound.
fn run(shared: Arc<Shared>, settings: Settings) {
    let mut measure = Measure::new(
        vutap::ring::DIR,
        measure::Settings {
            ring: settings.ring.clone(),
            hop: settings.hop,
            slots: settings.slots,
        },
    );
    let mut seen = 0u32;
    let mut buf: Vec<i16> = Vec::with_capacity(shared.relay.capacity());
    let mut chunks: Vec<Chunk> = Vec::with_capacity(256);
    let mut pending: VecDeque<(u64, Frame)> = VecDeque::with_capacity(PENDING_MAX);
    // Frames the player has written since the stream began, and when the
    // last hop fell due: a stream starts with an empty buffer, so its first
    // frames are heard at once, and hops keep their cadence when the
    // player writes ahead of the sound.
    let mut written = 0u64;
    let mut last_due = 0u64;
    while !shared.stop.load(Ordering::Acquire) {
        let now = now_ns();
        let timeout = match pending.front() {
            Some((due, _)) => {
                (due.saturating_sub(now) / 1_000_000).clamp(1, WAIT_MS as u64) as c_int
            }
            None => WAIT_MS,
        };
        shared.relay.wait(timeout);
        let generation = shared.generation.load(Ordering::Acquire);
        if generation != seen {
            seen = generation;
            // What the old stream left behind is not the new stream's.
            buf.clear();
            chunks.clear();
            shared.relay.drain(&mut buf, &mut chunks);
            pending.clear();
            written = 0;
            last_due = 0;
            let shape = shared.shape.lock().ok().and_then(|s| *s);
            if let Err(e) = measure.set_shape(shape) {
                note(&e);
            }
        }
        if shared.reset.swap(false, Ordering::AcqRel) {
            measure.reset();
        }
        buf.clear();
        chunks.clear();
        if shared.relay.drain(&mut buf, &mut chunks) > 0 {
            if let Some(shape) = measure.shape() {
                let rate = shape.rate.max(1) as u64;
                let lead = shared.lead.load(Ordering::Acquire) as u64;
                let per_frame = (shape.channels as u64).max(1)
                    * if shape.layout.is_dsd() {
                        shape.layout.bytes() as u64
                    } else {
                        1
                    };
                let hop_ns = measure.hop() as u64 * 1_000_000_000 / rate;
                // The PCM says how many frames stand between the last one
                // written and the sound; from that the moment the last of
                // these chunks ends is known, and every chunk before it.
                let pcm = shared.pcm.load(Ordering::Acquire);
                let mut delay: c_long = -1;
                let told =
                    !pcm.is_null() && unsafe { snd_pcm_delay(pcm, &mut delay) } == 0 && delay >= 0;
                let total: u64 = chunks.iter().map(|c| c.samples as u64).sum::<u64>() / per_frame;
                let end_of_all = now_ns() + delay.max(0) as u64 * 1_000_000_000 / rate;
                let mut before = 0u64;
                let mut offset = 0usize;
                for chunk in &chunks {
                    let n = chunk.samples as usize;
                    let samples = &buf[offset..offset + n];
                    offset += n;
                    let frames = n as u64 / per_frame;
                    let heard_from = if told {
                        end_of_all.saturating_sub((total - before) * 1_000_000_000 / rate)
                    } else {
                        // Heard after whatever the buffer holds ahead of it:
                        // the lead once the player has filled the buffer,
                        // less before.
                        chunk.time_ns + written.min(lead) * 1_000_000_000 / rate
                    };
                    before += frames;
                    written += frames;
                    measure.take(samples, &mut |frame, frames_in| {
                        let due = (heard_from + frames_in as u64 * 1_000_000_000 / rate)
                            .max(last_due + hop_ns);
                        last_due = due;
                        if pending.len() >= PENDING_MAX {
                            pending.pop_front();
                        }
                        pending.push_back((due, frame.clone()));
                    });
                }
            }
        }
        let now = now_ns();
        while pending.front().is_some_and(|(due, _)| *due <= now) {
            if let Some((_, frame)) = pending.pop_front() {
                measure.publish(&frame);
            }
        }
    }
}

/// The mark libasound looks for beside the entry point to accept its
/// protocol version; what it holds does not matter.
#[no_mangle]
pub static __snd_pcm_vutap_open_dlsym_pcm_001: [usize; 3] = [0; 3];

/// The entry point libasound looks for when a PCM's `type` is `vutap`.
///
/// # Safety
/// Called by libasound with live configuration nodes; `pcmp` receives the
/// PCM on success.
#[no_mangle]
pub unsafe extern "C" fn _snd_pcm_vutap_open(
    pcmp: *mut *mut snd_pcm_t,
    name: *const c_char,
    root: *mut snd_config_t,
    conf: *mut snd_config_t,
    stream: c_int,
    mode: c_int,
) -> c_int {
    let outcome = catch_unwind(AssertUnwindSafe(|| {
        let settings = settings_from(conf);
        let key = CString::new("slave").unwrap();
        let mut slave: *mut snd_config_t = std::ptr::null_mut();
        if snd_config_search(conf, key.as_ptr(), &mut slave) < 0 || slave.is_null() {
            note("a vutap PCM needs a slave");
            return -libc::EINVAL;
        }
        let relay = match Relay::new(RELAY_SAMPLES) {
            Ok(r) => r,
            Err(e) => {
                note(&format!("no relay: {e}"));
                return -libc::EIO;
            }
        };
        let shared = Arc::new(Shared {
            relay,
            shape: Mutex::new(None),
            lead: AtomicU32::new(0),
            pcm: AtomicPtr::new(std::ptr::null_mut()),
            generation: AtomicU32::new(0),
            reset: AtomicBool::new(false),
            stop: AtomicBool::new(false),
        });
        let worker_shared = shared.clone();
        let pcm_slot = shared.clone();
        let worker = match std::thread::Builder::new()
            .name("vutap".into())
            .spawn(move || run(worker_shared, settings))
        {
            Ok(h) => h,
            Err(e) => {
                note(&format!("no measuring thread: {e}"));
                return -libc::EIO;
            }
        };
        let tap = Box::new(Tap {
            shared,
            worker: Some(worker),
            layout: None,
            channels: 2,
            playback: stream == SND_PCM_STREAM_PLAYBACK,
            scratch: Vec::with_capacity(RELAY_SAMPLES / 4),
            failed: false,
        });
        let ext = Box::leak(Box::new(snd_pcm_extplug_t {
            version: SND_PCM_EXTPLUG_VERSION,
            name: NAME.as_ptr() as *const c_char,
            callback: &CALLBACKS,
            private_data: Box::into_raw(tap) as *mut c_void,
            pcm: std::ptr::null_mut(),
            stream,
            format: 0,
            subformat: 0,
            channels: 0,
            rate: 0,
            slave_format: 0,
            slave_subformat: 0,
            slave_channels: 0,
        }));
        let err = snd_pcm_extplug_create(ext, name, root, slave, stream, mode);
        if err < 0 {
            // Nothing of libasound's holds the handle yet: take it all back.
            let mut t = Box::from_raw(ext.private_data as *mut Tap);
            t.shared.stop.store(true, Ordering::Release);
            t.shared.relay.wake();
            if let Some(worker) = t.worker.take() {
                let _ = worker.join();
            }
            drop(Box::from_raw(ext as *mut snd_pcm_extplug_t));
            return err;
        }
        pcm_slot.pcm.store(ext.pcm, Ordering::Release);
        *pcmp = ext.pcm;
        0
    }));
    match outcome {
        Ok(code) => code,
        Err(_) => {
            note("open failed");
            -libc::EINVAL
        }
    }
}
