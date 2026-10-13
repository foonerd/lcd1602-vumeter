//! The relay between the audio thread and the measuring thread: a ring of
//! samples the audio thread fills without waiting or locking, a ring of
//! chunk marks that say when each transfer arrived, and an event the
//! measuring thread sleeps on. One producer, one consumer, by agreement:
//! the audio thread only pushes, the measuring thread only drains and
//! waits.

use std::os::fd::RawFd;
use std::sync::atomic::{AtomicI16, AtomicU32, AtomicU64, AtomicUsize, Ordering};

/// One transfer's worth of samples, as the producer handed it over.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Chunk {
    /// When the producer handed it over, on the monotonic clock.
    pub time_ns: u64,
    /// How many samples of it the relay took.
    pub samples: u32,
}

/// Chunk marks the relay holds at most; a transfer a period at a time
/// needs a few dozen a second, and the consumer drains them within 50 ms.
const CHUNKS: usize = 1024;

pub struct Relay {
    buf: Vec<AtomicI16>,
    mask: usize,
    /// Samples pushed so far; only the producer writes it.
    head: AtomicUsize,
    /// Samples drained so far; only the consumer writes it.
    tail: AtomicUsize,
    chunk_time: Vec<AtomicU64>,
    chunk_len: Vec<AtomicU32>,
    chunk_head: AtomicUsize,
    chunk_tail: AtomicUsize,
    /// Samples that found no room and were dropped.
    dropped: AtomicU64,
    event: RawFd,
}

// The atomics carry the samples; the event is a file descriptor.
unsafe impl Send for Relay {}
unsafe impl Sync for Relay {}

impl Relay {
    /// Room for `capacity` samples, rounded up to a power of two.
    pub fn new(capacity: usize) -> Result<Self, String> {
        let capacity = capacity.max(2).next_power_of_two();
        let event = unsafe { libc::eventfd(0, libc::EFD_NONBLOCK | libc::EFD_CLOEXEC) };
        if event < 0 {
            return Err(format!("eventfd: {}", std::io::Error::last_os_error()));
        }
        Ok(Self {
            buf: (0..capacity).map(|_| AtomicI16::new(0)).collect(),
            mask: capacity - 1,
            head: AtomicUsize::new(0),
            tail: AtomicUsize::new(0),
            chunk_time: (0..CHUNKS).map(|_| AtomicU64::new(0)).collect(),
            chunk_len: (0..CHUNKS).map(|_| AtomicU32::new(0)).collect(),
            chunk_head: AtomicUsize::new(0),
            chunk_tail: AtomicUsize::new(0),
            dropped: AtomicU64::new(0),
            event,
        })
    }

    pub fn capacity(&self) -> usize {
        self.mask + 1
    }

    /// Samples waiting to be drained.
    pub fn len(&self) -> usize {
        self.head
            .load(Ordering::Acquire)
            .wrapping_sub(self.tail.load(Ordering::Acquire))
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Samples dropped for want of room since the relay was made.
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    /// Producer: append one transfer's samples, as many as fit, marked with
    /// the time they arrived, and wake the consumer. Returns how many were
    /// taken; none when there is no room for the mark.
    pub fn push(&self, samples: &[i16], time_ns: u64) -> usize {
        let chunk_head = self.chunk_head.load(Ordering::Relaxed);
        let chunk_tail = self.chunk_tail.load(Ordering::Acquire);
        if chunk_head.wrapping_sub(chunk_tail) >= CHUNKS {
            self.dropped
                .fetch_add(samples.len() as u64, Ordering::Relaxed);
            return 0;
        }
        let head = self.head.load(Ordering::Relaxed);
        let tail = self.tail.load(Ordering::Acquire);
        let room = self.capacity() - head.wrapping_sub(tail);
        let taken = samples.len().min(room);
        for (i, s) in samples[..taken].iter().enumerate() {
            self.buf[head.wrapping_add(i) & self.mask].store(*s, Ordering::Relaxed);
        }
        self.head.store(head.wrapping_add(taken), Ordering::Release);
        if taken < samples.len() {
            self.dropped
                .fetch_add((samples.len() - taken) as u64, Ordering::Relaxed);
        }
        let at = chunk_head % CHUNKS;
        self.chunk_time[at].store(time_ns, Ordering::Relaxed);
        self.chunk_len[at].store(taken as u32, Ordering::Relaxed);
        self.chunk_head
            .store(chunk_head.wrapping_add(1), Ordering::Release);
        self.wake();
        taken
    }

    /// Consumer: move every whole chunk waiting to the end of `samples`,
    /// its marks to the end of `chunks`, in order. Returns how many
    /// samples were moved.
    pub fn drain(&self, samples: &mut Vec<i16>, chunks: &mut Vec<Chunk>) -> usize {
        let chunk_tail = self.chunk_tail.load(Ordering::Relaxed);
        let chunk_head = self.chunk_head.load(Ordering::Acquire);
        let mut total = 0usize;
        for i in chunk_tail..chunk_head {
            let at = i % CHUNKS;
            let chunk = Chunk {
                time_ns: self.chunk_time[at].load(Ordering::Relaxed),
                samples: self.chunk_len[at].load(Ordering::Relaxed),
            };
            total += chunk.samples as usize;
            chunks.push(chunk);
        }
        let tail = self.tail.load(Ordering::Relaxed);
        samples.reserve(total);
        for i in 0..total {
            samples.push(self.buf[tail.wrapping_add(i) & self.mask].load(Ordering::Relaxed));
        }
        self.tail.store(tail.wrapping_add(total), Ordering::Release);
        self.chunk_tail.store(chunk_head, Ordering::Release);
        total
    }

    /// Wake the consumer without pushing, for a stop or a change of stream.
    pub fn wake(&self) {
        let one: u64 = 1;
        unsafe {
            libc::write(self.event, &one as *const u64 as *const libc::c_void, 8);
        }
    }

    /// Consumer: sleep until the producer pushes, or `timeout_ms` passes.
    pub fn wait(&self, timeout_ms: i32) {
        let mut fds = libc::pollfd {
            fd: self.event,
            events: libc::POLLIN,
            revents: 0,
        };
        unsafe {
            libc::poll(&mut fds, 1, timeout_ms);
            let mut count: u64 = 0;
            libc::read(self.event, &mut count as *mut u64 as *mut libc::c_void, 8);
        }
    }
}

impl Drop for Relay {
    fn drop(&mut self) {
        unsafe {
            libc::close(self.event);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    #[test]
    fn chunks_cross_in_order_with_their_marks_and_the_overflow_is_counted() {
        let relay = Relay::new(8).unwrap();
        assert_eq!(relay.capacity(), 8);
        assert_eq!(relay.push(&[1, 2, 3], 100), 3);
        assert_eq!(relay.push(&[4], 200), 1);
        assert_eq!(relay.len(), 4);
        let mut out = Vec::new();
        let mut marks = Vec::new();
        assert_eq!(relay.drain(&mut out, &mut marks), 4);
        assert_eq!(out, [1, 2, 3, 4]);
        assert_eq!(
            marks,
            [
                Chunk {
                    time_ns: 100,
                    samples: 3
                },
                Chunk {
                    time_ns: 200,
                    samples: 1
                }
            ]
        );
        assert!(relay.is_empty());
        // Ten into eight: two dropped, the mark says eight.
        let ten: Vec<i16> = (10..20).collect();
        assert_eq!(relay.push(&ten, 300), 8);
        assert_eq!(relay.dropped(), 2);
        out.clear();
        marks.clear();
        relay.drain(&mut out, &mut marks);
        assert_eq!(out, (10..18).collect::<Vec<i16>>());
        assert_eq!(marks[0].samples, 8);
        // Wrapping around the ring keeps the order.
        relay.push(&[7, 7, 7, 7, 7], 400);
        out.clear();
        marks.clear();
        relay.drain(&mut out, &mut marks);
        relay.push(&[1, 2, 3, 4, 5, 6], 500);
        out.clear();
        marks.clear();
        relay.drain(&mut out, &mut marks);
        assert_eq!(out, [1, 2, 3, 4, 5, 6]);
        assert_eq!(marks.len(), 1);
    }

    #[test]
    fn a_relay_full_of_marks_takes_nothing_more_until_drained() {
        let relay = Relay::new(1 << 16).unwrap();
        for i in 0..CHUNKS {
            assert_eq!(relay.push(&[1], i as u64), 1);
        }
        assert_eq!(relay.push(&[1, 1], 9), 0, "no room for the mark");
        assert_eq!(relay.dropped(), 2);
        let mut out = Vec::new();
        let mut marks = Vec::new();
        assert_eq!(relay.drain(&mut out, &mut marks), CHUNKS);
        assert_eq!(marks.len(), CHUNKS);
        assert_eq!(relay.push(&[1, 1], 10), 2);
    }

    #[test]
    fn the_consumer_wakes_when_the_producer_pushes() {
        let relay = Arc::new(Relay::new(1024).unwrap());
        let producer = relay.clone();
        let handle = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(30));
            producer.push(&[42; 100], 1);
        });
        let started = Instant::now();
        let mut out = Vec::new();
        let mut marks = Vec::new();
        while relay.drain(&mut out, &mut marks) == 0 && started.elapsed() < Duration::from_secs(2) {
            relay.wait(500);
        }
        handle.join().unwrap();
        assert_eq!(out.len(), 100);
        assert!(
            started.elapsed() < Duration::from_millis(400),
            "woke by the event, not the timeout"
        );
        // Without a push, the wait ends at the timeout.
        let started = Instant::now();
        relay.wait(20);
        assert!(started.elapsed() >= Duration::from_millis(15));
    }
}
