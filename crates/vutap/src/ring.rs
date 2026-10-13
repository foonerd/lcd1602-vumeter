//! The shared ring: a file under `/dev/shm` with a header and a ring of
//! slots, written by the tap and read by the display. One file per writing
//! process, so two players loaded with the tap never write over each other;
//! the reader takes the one written most recently.
//!
//! Layout, little-endian, all offsets fixed:
//!
//! - header, 64 bytes: magic `VUTAP`, version, sample rate, channels, hop,
//!   writer pid, slot count, then the sequence of the last slot published
//!   and the time it was published, both written last with release ordering.
//! - slots, 48 bytes each: sequence, time in nanoseconds of the monotonic
//!   clock, peak per channel, RMS per channel, flags, and the sequence
//!   again. A reader whose two sequences disagree caught a write in flight
//!   and tries again.

use std::ffi::CString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

pub const MAGIC: &[u8; 8] = b"VUTAP\0\0\0";
pub const VERSION: u32 = 1;
pub const DIR: &str = "/dev/shm";
pub const PREFIX: &str = "vutap.";
pub const MAX_CHANNELS: usize = 2;
pub const HEADER_BYTES: usize = 64;
pub const SLOT_BYTES: usize = 48;
/// A ring written longer ago than this is a leftover of a player that stopped.
pub const LIVE_NS: u64 = 3_000_000_000;

/// Writers made by this process so far, for their file names.
static INSTANCE: AtomicU32 = AtomicU32::new(0);

const H_MAGIC: usize = 0;
const H_VERSION: usize = 8;
const H_RATE: usize = 12;
const H_CHANNELS: usize = 16;
const H_HOP: usize = 20;
const H_PID: usize = 24;
const H_SLOTS: usize = 28;
const H_SEQ: usize = 32;
const H_WRITTEN: usize = 40;

const S_SEQ: usize = 0;
const S_TIME: usize = 8;
const S_PEAK: usize = 16;
const S_RMS: usize = 24;
const S_FLAGS: usize = 32;
const S_TAIL: usize = 40;

const FLAG_ONE_BIT: u32 = 1;

/// One hop's measurements. Peak and RMS are linear, 1.0 being full scale.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Frame {
    pub seq: u64,
    pub time_ns: u64,
    pub peak: [f32; MAX_CHANNELS],
    pub rms: [f32; MAX_CHANNELS],
    /// One-bit audio (DSD, DoP): the peak and RMS are density levels.
    pub one_bit: bool,
}

/// The hops that arrived between two looks at the ring, as one: the peaks
/// and RMS take the highest of them, the sequence and time the latest, so a
/// transient inside one frame of the display still shows.
pub fn merge_hops(hops: impl Iterator<Item = Frame>) -> Option<Frame> {
    let mut merged: Option<Frame> = None;
    for frame in hops {
        match merged.as_mut() {
            None => merged = Some(frame),
            Some(m) => {
                for ch in 0..MAX_CHANNELS {
                    m.peak[ch] = m.peak[ch].max(frame.peak[ch]);
                    m.rms[ch] = m.rms[ch].max(frame.rms[ch]);
                }
                m.one_bit |= frame.one_bit;
                m.seq = frame.seq;
                m.time_ns = frame.time_ns;
            }
        }
    }
    merged
}

/// What the header says about the stream and the ring.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Info {
    pub rate: u32,
    pub channels: u32,
    pub hop: u32,
    pub pid: u32,
    pub slots: u32,
    pub seq: u64,
    pub written_ns: u64,
}

/// Nanoseconds of the monotonic clock.
pub fn now_ns() -> u64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // A valid clock id and a valid pointer: this cannot fail.
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64
}

/// Remove the rings left under `dir` by writers whose process is gone: a
/// player killed outright never drops its writer. Every new writer sweeps
/// first. Returns how many went.
pub fn sweep(dir: &Path) -> usize {
    let mut gone = 0;
    let own = std::process::id() as i32;
    for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(rest) = name.strip_prefix(PREFIX) else {
            continue;
        };
        // `<tag>.<pid>.<n>`
        let fields: Vec<&str> = rest.split('.').collect();
        let Some(pid) = (fields.len() >= 3)
            .then(|| fields[fields.len() - 2].parse::<i32>().ok())
            .flatten()
        else {
            continue;
        };
        if pid == own {
            continue;
        }
        // A process of another user answers with EPERM: alive.
        let alive = unsafe { libc::kill(pid, 0) } == 0
            || io::Error::last_os_error().raw_os_error() == Some(libc::EPERM);
        if !alive && fs::remove_file(entry.path()).is_ok() {
            gone += 1;
        }
    }
    gone
}

/// A mapped file. Unmapped when dropped.
struct Map {
    ptr: *mut u8,
    len: usize,
}

impl Map {
    fn open(fd: i32, len: usize, write: bool) -> io::Result<Self> {
        let prot = if write {
            libc::PROT_READ | libc::PROT_WRITE
        } else {
            libc::PROT_READ
        };
        // A shared mapping of `len` bytes of an open file.
        let ptr = unsafe { libc::mmap(std::ptr::null_mut(), len, prot, libc::MAP_SHARED, fd, 0) };
        if ptr == libc::MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            ptr: ptr as *mut u8,
            len,
        })
    }

    fn bytes(&self) -> &[u8] {
        // The mapping is `len` bytes long for as long as `self` lives.
        unsafe { std::slice::from_raw_parts(self.ptr, self.len) }
    }

    fn bytes_mut(&mut self) -> &mut [u8] {
        unsafe { std::slice::from_raw_parts_mut(self.ptr, self.len) }
    }

    fn atomic_u64(&self, at: usize) -> &AtomicU64 {
        // Every atomic offset is 8-aligned within a page-aligned mapping.
        unsafe { &*(self.ptr.add(at) as *const AtomicU64) }
    }
}

impl Drop for Map {
    fn drop(&mut self) {
        unsafe { libc::munmap(self.ptr as *mut libc::c_void, self.len) };
    }
}

// The ring is shared between processes by design; the mapping itself is
// owned by one writer or one reader at a time.
unsafe impl Send for Map {}

fn put_u32(bytes: &mut [u8], at: usize, v: u32) {
    bytes[at..at + 4].copy_from_slice(&v.to_le_bytes());
}
fn get_u32(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}
fn put_u64(bytes: &mut [u8], at: usize, v: u64) {
    bytes[at..at + 8].copy_from_slice(&v.to_le_bytes());
}
fn get_u64(bytes: &[u8], at: usize) -> u64 {
    let mut b = [0u8; 8];
    b.copy_from_slice(&bytes[at..at + 8]);
    u64::from_le_bytes(b)
}
fn put_f32(bytes: &mut [u8], at: usize, v: f32) {
    bytes[at..at + 4].copy_from_slice(&v.to_le_bytes());
}
fn get_f32(bytes: &[u8], at: usize) -> f32 {
    f32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

/// The writing side. Creating one creates the file; dropping it removes it.
pub struct Writer {
    map: Map,
    path: PathBuf,
    slots: usize,
    seq: u64,
}

impl Writer {
    /// Create the ring for a stream. `slots` is how many hops are kept; the
    /// reader takes what arrived since its last look.
    pub fn create(
        dir: &Path,
        tag: &str,
        rate: u32,
        channels: u32,
        hop: u32,
        slots: usize,
    ) -> io::Result<Self> {
        sweep(dir);
        let slots = slots.max(2);
        let len = HEADER_BYTES + slots * SLOT_BYTES;
        // One file per writer: a process may hold more than one tap.
        let instance = INSTANCE.fetch_add(1, Ordering::Relaxed);
        let path = dir.join(format!("{PREFIX}{tag}.{}.{instance}", std::process::id()));
        let cpath = CString::new(path.to_string_lossy().as_bytes())
            .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
        let fd = unsafe {
            libc::open(
                cpath.as_ptr(),
                libc::O_RDWR | libc::O_CREAT | libc::O_CLOEXEC,
                0o644,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let sized = unsafe { libc::ftruncate(fd, len as libc::off_t) };
        if sized != 0 {
            let err = io::Error::last_os_error();
            unsafe { libc::close(fd) };
            return Err(err);
        }
        let map = Map::open(fd, len, true);
        unsafe { libc::close(fd) };
        let mut map = map?;
        {
            let bytes = map.bytes_mut();
            bytes[..len].fill(0);
            bytes[H_MAGIC..H_MAGIC + 8].copy_from_slice(MAGIC);
            put_u32(bytes, H_VERSION, VERSION);
            put_u32(bytes, H_RATE, rate);
            put_u32(bytes, H_CHANNELS, channels.min(MAX_CHANNELS as u32));
            put_u32(bytes, H_HOP, hop);
            put_u32(bytes, H_PID, std::process::id());
            put_u32(bytes, H_SLOTS, slots as u32);
        }
        Ok(Self {
            map,
            path,
            slots,
            seq: 0,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Publish one hop. The frame's `seq` and `time_ns` are set here.
    pub fn publish(&mut self, frame: &Frame) {
        self.seq += 1;
        let seq = self.seq;
        let time = now_ns();
        let at = HEADER_BYTES + ((seq as usize) % self.slots) * SLOT_BYTES;
        {
            let bytes = self.map.bytes_mut();
            let slot = &mut bytes[at..at + SLOT_BYTES];
            // The sequence at the head marks the slot as being written.
            put_u64(slot, S_SEQ, seq | (1 << 63));
            put_u64(slot, S_TIME, time);
            for ch in 0..MAX_CHANNELS {
                put_f32(slot, S_PEAK + ch * 4, frame.peak[ch]);
                put_f32(slot, S_RMS + ch * 4, frame.rms[ch]);
            }
            put_u32(slot, S_FLAGS, if frame.one_bit { FLAG_ONE_BIT } else { 0 });
            put_u64(slot, S_TAIL, seq);
            put_u64(slot, S_SEQ, seq);
        }
        self.map
            .atomic_u64(H_WRITTEN)
            .store(time, Ordering::Release);
        self.map.atomic_u64(H_SEQ).store(seq, Ordering::Release);
    }
}

impl Drop for Writer {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

/// The reading side of one ring.
pub struct Reader {
    map: Map,
    path: PathBuf,
    info: Info,
}

impl Reader {
    /// Open a ring by path.
    pub fn open(path: &Path) -> io::Result<Self> {
        let meta = fs::metadata(path)?;
        let len = meta.len() as usize;
        if len < HEADER_BYTES {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "ring too short"));
        }
        let cpath = CString::new(path.to_string_lossy().as_bytes())
            .map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
        let fd = unsafe { libc::open(cpath.as_ptr(), libc::O_RDONLY | libc::O_CLOEXEC) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let map = Map::open(fd, len, false);
        unsafe { libc::close(fd) };
        let map = map?;
        let bytes = map.bytes();
        if &bytes[H_MAGIC..H_MAGIC + 8] != MAGIC || get_u32(bytes, H_VERSION) != VERSION {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "not a vutap ring of this version",
            ));
        }
        let info = Info {
            rate: get_u32(bytes, H_RATE),
            channels: get_u32(bytes, H_CHANNELS),
            hop: get_u32(bytes, H_HOP),
            pid: get_u32(bytes, H_PID),
            slots: get_u32(bytes, H_SLOTS),
            seq: 0,
            written_ns: 0,
        };
        let needed = HEADER_BYTES + info.slots as usize * SLOT_BYTES;
        if info.slots == 0 || len < needed {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "ring header disagrees with its size",
            ));
        }
        Ok(Self {
            map,
            path: path.to_path_buf(),
            info,
        })
    }

    /// Every ring under `dir`, the most recently written first.
    pub fn find(dir: &Path) -> Vec<PathBuf> {
        let mut found: Vec<(u64, PathBuf)> = fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|entry| entry.file_name().to_string_lossy().starts_with(PREFIX))
            .filter_map(|entry| {
                let path = entry.path();
                let reader = Reader::open(&path).ok()?;
                Some((reader.info().written_ns, path))
            })
            .collect();
        found.sort_by_key(|a| std::cmp::Reverse(a.0));
        found.into_iter().map(|(_, p)| p).collect()
    }

    /// The ring being written right now, if any.
    pub fn open_live(dir: &Path) -> Option<Self> {
        Self::find(dir)
            .into_iter()
            .find_map(|path| Reader::open(&path).ok().filter(Reader::is_live))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The header, with the sequence and time of the last publish.
    pub fn info(&self) -> Info {
        Info {
            seq: self.map.atomic_u64(H_SEQ).load(Ordering::Acquire),
            written_ns: self.map.atomic_u64(H_WRITTEN).load(Ordering::Acquire),
            ..self.info
        }
    }

    /// Written within the last few seconds and still on disk.
    pub fn is_live(&self) -> bool {
        let written = self.map.atomic_u64(H_WRITTEN).load(Ordering::Acquire);
        written != 0 && now_ns().saturating_sub(written) < LIVE_NS && self.path.exists()
    }

    /// The last published hop, or `None` before the first.
    pub fn latest(&self) -> Option<Frame> {
        let seq = self.map.atomic_u64(H_SEQ).load(Ordering::Acquire);
        if seq == 0 {
            return None;
        }
        self.slot(seq)
    }

    /// Every hop published after `after`, oldest first, as far back as the
    /// ring still holds them.
    pub fn since(&self, after: u64) -> Vec<Frame> {
        let latest = self.map.atomic_u64(H_SEQ).load(Ordering::Acquire);
        if latest <= after {
            return Vec::new();
        }
        let oldest = latest.saturating_sub(self.info.slots as u64 - 1).max(1);
        let first = (after + 1).max(oldest);
        (first..=latest).filter_map(|s| self.slot(s)).collect()
    }

    /// The hop with a given sequence, while the ring still holds it.
    pub fn slot(&self, seq: u64) -> Option<Frame> {
        let latest = self.map.atomic_u64(H_SEQ).load(Ordering::Acquire);
        if seq == 0 || seq > latest || latest - seq >= self.info.slots as u64 {
            return None;
        }
        let at = HEADER_BYTES + ((seq as usize) % self.info.slots as usize) * SLOT_BYTES;
        for _ in 0..3 {
            let bytes = self.map.bytes();
            let slot = &bytes[at..at + SLOT_BYTES];
            let head = get_u64(slot, S_SEQ);
            let tail = get_u64(slot, S_TAIL);
            if head != seq || tail != seq {
                std::hint::spin_loop();
                continue;
            }
            let mut frame = Frame {
                seq,
                time_ns: get_u64(slot, S_TIME),
                one_bit: get_u32(slot, S_FLAGS) & FLAG_ONE_BIT != 0,
                ..Frame::default()
            };
            for ch in 0..MAX_CHANNELS {
                frame.peak[ch] = get_f32(slot, S_PEAK + ch * 4);
                frame.rms[ch] = get_f32(slot, S_RMS + ch * 4);
            }
            // The head is written last; if it still matches, the copy is whole.
            if get_u64(slot, S_SEQ) == seq {
                return Some(frame);
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("vutap-ring-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn rings_of_dead_processes_are_swept_and_live_ones_kept() {
        let dir = temp_dir("sweep");
        let dead = dir.join(format!("{PREFIX}test.{}.0", i32::MAX));
        let live = dir.join(format!("{PREFIX}test.{}.7", std::process::id()));
        let other = dir.join("not-a-ring.txt");
        for p in [&dead, &live, &other] {
            fs::write(p, b"x").unwrap();
        }
        assert_eq!(sweep(&dir), 1);
        assert!(!dead.exists());
        assert!(live.exists() && other.exists());
        fs::write(&dead, b"x").unwrap();
        let writer = Writer::create(&dir, "test", 48_000, 2, 1024, 4).unwrap();
        assert!(!dead.exists(), "a new writer sweeps on its way in");
        assert!(writer.path().exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_frame_published_is_the_frame_read() {
        let dir = temp_dir("rw");
        let mut writer = Writer::create(&dir, "test", 48_000, 2, 1024, 4).unwrap();
        let reader = Reader::open(writer.path()).unwrap();
        assert_eq!(reader.latest(), None, "nothing before the first publish");
        let frame = Frame {
            peak: [0.5, 0.25],
            rms: [0.3, 0.2],
            ..Frame::default()
        };
        writer.publish(&frame);
        let read = reader.latest().expect("a frame");
        assert_eq!(read.seq, 1);
        assert_eq!(read.peak, frame.peak);
        assert_eq!(read.rms, frame.rms);
        assert!(!read.one_bit);
        assert!(read.time_ns > 0);
        writer.publish(&Frame {
            one_bit: true,
            ..frame.clone()
        });
        assert!(reader.latest().unwrap().one_bit);
        let info = reader.info();
        assert_eq!(
            (info.rate, info.channels, info.hop, info.slots, info.seq),
            (48_000, 2, 1024, 4, 2)
        );
        assert!(reader.is_live());
        // The ring keeps the last `slots` hops and no more (two published above).
        for _ in 0..5 {
            writer.publish(&frame);
        }
        assert_eq!(reader.info().seq, 7);
        assert!(reader.slot(7).is_some());
        assert!(reader.slot(4).is_some());
        assert!(reader.slot(3).is_none(), "overwritten");
        assert_eq!(
            reader.since(5).iter().map(|f| f.seq).collect::<Vec<_>>(),
            [6, 7]
        );
        assert_eq!(
            reader.since(0).iter().map(|f| f.seq).collect::<Vec<_>>(),
            [4, 5, 6, 7],
            "as far back as the ring holds"
        );
        assert!(reader.since(7).is_empty());
        assert_eq!(Reader::find(&dir).len(), 1);
        assert!(Reader::open_live(&dir).is_some());
        let path = writer.path().to_path_buf();
        drop(writer);
        assert!(!path.exists(), "the file goes with the writer");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn merged_hops_keep_the_highest_level_and_the_latest_sequence() {
        let a = Frame {
            seq: 1,
            peak: [0.2, 0.9],
            rms: [0.1, 0.5],
            ..Frame::default()
        };
        let b = Frame {
            seq: 2,
            peak: [0.8, 0.1],
            rms: [0.4, 0.2],
            one_bit: true,
            ..Frame::default()
        };
        let m = merge_hops([a, b].into_iter()).unwrap();
        assert_eq!(m.peak, [0.8, 0.9]);
        assert_eq!(m.rms, [0.4, 0.5]);
        assert_eq!(m.seq, 2);
        assert!(m.one_bit);
        assert!(merge_hops(std::iter::empty()).is_none());
    }
}
