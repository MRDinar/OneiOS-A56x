// SPDX-License-Identifier: MIT
//! The guest's frames on their way to wlan0 (docs/hardware.md section 3, "After the join"): a frame that finds the
//! AF_PACKET socket's send buffer full (`EAGAIN`) waits here, in order, until the socket can take it again (the
//! frames thread's `POLLOUT`, wifi::wlan) or until it has waited [`WAIT_MS`]; [`HELD_MAX`] frames wait at most.
//! While any frame waits, the guest's next frames wait behind it, so the radio sends them in the guest's order. The
//! guest's vCPU never waits on the socket: it sends (the waiting frames first, as far as the socket takes them), or
//! it puts the frame here (one wake of the frames thread for the first one). Until 2026-11-23 such a frame was
//! dropped at once: the phone's session of 19:40 that day dropped 70, 1624 and 112 frames a minute (`from iOS: 1624
//! send failed, last errno 11`), most of them TCP ACKs and requests of downloads, which then stalled and sent again.

use super::traffic::Traffic;
use std::collections::VecDeque;
use std::sync::atomic::Ordering;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

/// How long a frame waits for the send buffer before it is dropped (the receive side's ring wait for a data frame:
/// devices::virtio_net::DATA_WAIT_MS).
pub const WAIT_MS: u64 = 20;
/// The frames that wait at most; one more is dropped at once (`dropped with 256 waiting`).
pub const HELD_MAX: usize = 256;

struct Held {
    at: Instant,
    packet: Vec<u8>, // the virtio_net_hdr and the frame, as the socket takes them
}

/// The frames waiting for the socket.
#[derive(Default)]
pub struct Uplink {
    held: Mutex<VecDeque<Held>>,
}

/// What [`Uplink::send`] did with a frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sent {
    /// the socket took it
    Now,
    /// it waits; `first`: the first one waiting (the frames thread must watch for `POLLOUT`: wake it)
    Held { first: bool },
}

/// What one look at the list did ([`step`]).
enum Step {
    /// nothing waits
    Empty,
    /// the socket is full again; the oldest frame's wait ends then
    Full(Instant),
    /// a frame left the list (sent, dropped after its wait, or failed)
    Gone,
}

/// The oldest waiting frame: dropped when it waited [`WAIT_MS`], else offered to `send`.
fn step(
    q: &mut VecDeque<Held>,
    now: Instant,
    hdr: usize,
    t: &Traffic,
    send: &mut impl FnMut(&[u8]) -> Result<(), i32>,
) -> Step {
    let wait = Duration::from_millis(WAIT_MS);
    let Some(h) = q.front() else { return Step::Empty };
    let waited = now.saturating_duration_since(h.at);
    if waited >= wait {
        Traffic::add(&t.tx_wait_dropped, 1);
    } else {
        match send(&h.packet) {
            Ok(()) => {
                Traffic::add(&t.tx_frames, 1);
                Traffic::add(&t.tx_bytes, (h.packet.len() - hdr) as u64);
                Traffic::add(&t.tx_wait_sent, 1);
                t.tx_wait_longest_us.fetch_max(waited.as_micros() as u64, Ordering::Relaxed);
            }
            Err(libc::EAGAIN) => return Step::Full(h.at + wait),
            Err(rc) => {
                Traffic::add(&t.tx_failed, 1);
                t.tx_last_errno.store(rc, Ordering::Relaxed);
            }
        }
    }
    q.pop_front();
    Step::Gone
}

impl Uplink {
    fn lock(&self) -> MutexGuard<'_, VecDeque<Held>> {
        self.held.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The guest's frame (`packet`: the header and the frame; `hdr`: the header's length, not counted as bytes),
    /// sent by `send` (positive errnos) or held. The frames that wait go first, as far as the socket takes them: the
    /// socket may have room again before the frames thread sees POLLOUT (it can be in the receive ring's wait for
    /// the guest's buffers, devices::virtio_net), and while it stays full this costs the one refused send the frame
    /// cost before. Errors: `send`'s (counted as `send failed`), `EAGAIN` when [`HELD_MAX`] frames wait already
    /// (counted as `dropped with 256 waiting`).
    pub fn send(
        &self,
        packet: &[u8],
        hdr: usize,
        now: Instant,
        t: &Traffic,
        mut send: impl FnMut(&[u8]) -> Result<(), i32>,
    ) -> Result<Sent, i32> {
        let mut q = self.lock();
        while let Step::Gone = step(&mut q, now, hdr, t, &mut send) {}
        if q.is_empty() {
            match send(packet) {
                Ok(()) => {
                    Traffic::add(&t.tx_frames, 1);
                    Traffic::add(&t.tx_bytes, (packet.len() - hdr) as u64);
                    return Ok(Sent::Now);
                }
                Err(libc::EAGAIN) => {}
                Err(rc) => {
                    Traffic::add(&t.tx_failed, 1);
                    t.tx_last_errno.store(rc, Ordering::Relaxed);
                    return Err(rc);
                }
            }
        } else if q.len() >= HELD_MAX {
            Traffic::add(&t.tx_held_full, 1);
            return Err(libc::EAGAIN);
        }
        let first = q.is_empty();
        q.push_back(Held { at: now, packet: packet.to_vec() });
        Traffic::add(&t.tx_waited, 1);
        Ok(Sent::Held { first })
    }

    /// The socket may take frames again (`POLLOUT`), or the oldest frame's wait ran out (the frames thread): sends
    /// what waits, oldest first, until the socket is full again; a frame that waited [`WAIT_MS`] is dropped
    /// (`dropped after a 20 ms wait`). When the next wait ends (None: nothing waits). The lock is taken per frame:
    /// the vCPU waits for one send at most.
    pub fn flush(
        &self,
        now: Instant,
        hdr: usize,
        t: &Traffic,
        mut send: impl FnMut(&[u8]) -> Result<(), i32>,
    ) -> Option<Instant> {
        loop {
            match step(&mut self.lock(), now, hdr, t, &mut send) {
                Step::Empty => return None,
                Step::Full(at) => return Some(at),
                Step::Gone => {}
            }
        }
    }

    /// When the oldest frame's wait ends (None: nothing waits).
    pub fn deadline(&self) -> Option<Instant> {
        self.lock().front().map(|h| h.at + Duration::from_millis(WAIT_MS))
    }

    /// How many frames wait.
    pub fn len(&self) -> usize {
        self.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.lock().is_empty()
    }
}

/// Milliseconds from `now` to `deadline` for poll(2), rounded up (0 once it passed; -1: no deadline).
pub fn poll_ms(now: Instant, deadline: Option<Instant>) -> i32 {
    match deadline {
        None => -1,
        Some(d) => {
            let left = d.saturating_duration_since(now);
            left.as_micros().div_ceil(1000).min(i32::MAX as u128) as i32
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

    const HDR: usize = 10;

    fn packet(n: u8) -> Vec<u8> {
        let mut p = vec![0u8; HDR + 60];
        p[HDR] = n;
        p
    }

    /// The frames the socket got, by their first byte: a `send` that takes `room` frames, then answers EAGAIN.
    fn socket(room: usize) -> (RefCell<Vec<u8>>, RefCell<usize>) {
        (RefCell::new(Vec::new()), RefCell::new(room))
    }
    fn offer(s: &(RefCell<Vec<u8>>, RefCell<usize>), p: &[u8]) -> Result<(), i32> {
        let mut room = s.1.borrow_mut();
        if *room == 0 {
            return Err(libc::EAGAIN);
        }
        *room -= 1;
        s.0.borrow_mut().push(p[HDR]);
        Ok(())
    }

    /// EAGAIN, then the socket takes frames again (POLLOUT): the held frames go out in the guest's order, and a
    /// frame sent while others wait goes behind them.
    #[test]
    fn eagain_then_pollout() {
        let (u, t) = (Uplink::default(), Traffic::default());
        let s = socket(1);
        let t0 = Instant::now();
        assert_eq!(u.send(&packet(1), HDR, t0, &t, |p| offer(&s, p)), Ok(Sent::Now));
        assert_eq!(u.send(&packet(2), HDR, t0, &t, |p| offer(&s, p)), Ok(Sent::Held { first: true }));
        // the next frame: the held one is offered first (still full), this one goes behind it, never offered
        let offered = RefCell::new(Vec::new());
        let r = u.send(&packet(3), HDR, t0, &t, |p| {
            offered.borrow_mut().push(p[HDR]);
            Err(libc::EAGAIN)
        });
        assert_eq!((r, offered.into_inner()), (Ok(Sent::Held { first: false }), vec![2]));
        assert_eq!((u.len(), u.deadline()), (2, Some(t0 + Duration::from_millis(WAIT_MS))));
        // the socket full still: nothing leaves, the deadline stays the oldest frame's
        assert_eq!(u.flush(t0 + Duration::from_millis(5), HDR, &t, |p| offer(&s, p)), u.deadline());
        assert_eq!(u.len(), 2);
        // POLLOUT: both, in order
        *s.1.borrow_mut() = 10;
        assert_eq!(u.flush(t0 + Duration::from_millis(7), HDR, &t, |p| offer(&s, p)), None);
        assert_eq!((s.0.borrow().clone(), u.len()), (vec![1, 2, 3], 0));
        let c = t.read();
        assert_eq!(
            (c.tx_frames, c.tx_bytes, c.tx_waited, c.tx_wait_sent, c.tx_wait_dropped, c.tx_failed),
            (3, 180, 2, 2, 0, 0)
        );
        assert_eq!(t.tx_wait_longest_us.load(Ordering::Relaxed), 7000);
        // empty again: the next frame goes straight to the socket
        assert_eq!(u.send(&packet(4), HDR, t0, &t, |p| offer(&s, p)), Ok(Sent::Now));
    }

    /// Room again before the frames thread saw POLLOUT (it may wait for the guest's receive buffers): the guest's
    /// next frame sends the held ones first, then itself, in order.
    #[test]
    fn the_next_frame_sends_the_held_ones_first() {
        let (u, t) = (Uplink::default(), Traffic::default());
        let s = socket(0);
        let t0 = Instant::now();
        assert_eq!(u.send(&packet(1), HDR, t0, &t, |p| offer(&s, p)), Ok(Sent::Held { first: true }));
        assert_eq!(u.send(&packet(2), HDR, t0, &t, |p| offer(&s, p)), Ok(Sent::Held { first: false }));
        // room for one: the oldest goes, the rest wait behind the new one
        *s.1.borrow_mut() = 1;
        let later = t0 + Duration::from_millis(3);
        assert_eq!(u.send(&packet(3), HDR, later, &t, |p| offer(&s, p)), Ok(Sent::Held { first: false }));
        assert_eq!((s.0.borrow().clone(), u.len()), (vec![1], 2));
        // room for all: the two held, then the new one sent at once
        *s.1.borrow_mut() = 10;
        assert_eq!(u.send(&packet(4), HDR, later, &t, |p| offer(&s, p)), Ok(Sent::Now));
        assert_eq!((s.0.borrow().clone(), u.len()), (vec![1, 2, 3, 4], 0));
        let c = t.read();
        assert_eq!((c.tx_frames, c.tx_waited, c.tx_wait_sent, c.tx_wait_dropped), (4, 3, 3, 0));
        assert_eq!(t.tx_wait_longest_us.load(Ordering::Relaxed), 3000);
        // a held frame past its bound is dropped there too, not sent late
        *s.1.borrow_mut() = 0;
        u.send(&packet(5), HDR, t0, &t, |p| offer(&s, p)).unwrap();
        *s.1.borrow_mut() = 10;
        let late = t0 + Duration::from_millis(WAIT_MS);
        assert_eq!(u.send(&packet(6), HDR, late, &t, |p| offer(&s, p)), Ok(Sent::Now));
        assert_eq!((s.0.borrow().last().copied(), t.read().tx_wait_dropped), (Some(6), 1));
    }

    /// EAGAIN until the bound: the frames that waited WAIT_MS are dropped and counted; a younger one still waits;
    /// a full held list drops at once; another error is a failed send.
    #[test]
    fn eagain_to_the_bound() {
        let (u, t) = (Uplink::default(), Traffic::default());
        let full = socket(0);
        let t0 = Instant::now();
        u.send(&packet(1), HDR, t0, &t, |p| offer(&full, p)).unwrap();
        u.send(&packet(2), HDR, t0 + Duration::from_millis(15), &t, |p| offer(&full, p)).unwrap();
        let at = t0 + Duration::from_millis(WAIT_MS);
        assert_eq!(poll_ms(t0, u.deadline()), WAIT_MS as i32);
        assert_eq!(poll_ms(at, u.deadline()), 0);
        assert_eq!(poll_ms(at, None), -1);
        // the wait of frame 1 ran out (poll's timeout); frame 2 has 15 ms left and the socket is still full
        assert_eq!(u.flush(at, HDR, &t, |p| offer(&full, p)), Some(t0 + Duration::from_millis(15 + WAIT_MS)));
        assert_eq!((u.len(), t.read().tx_wait_dropped), (1, 1));
        assert_eq!(u.flush(at + Duration::from_millis(15), HDR, &t, |_| unreachable!()), None);
        let c = t.read();
        assert_eq!((c.tx_wait_dropped, c.tx_wait_sent, c.tx_frames, c.tx_waited), (2, 0, 0, 2));
        assert!(full.0.borrow().is_empty());
        // a full list: the next frame is dropped at once
        for _ in 0..HELD_MAX {
            u.send(&packet(0), HDR, t0, &t, |p| offer(&full, p)).unwrap();
        }
        assert_eq!(u.send(&packet(0), HDR, t0, &t, |p| offer(&full, p)), Err(libc::EAGAIN));
        assert_eq!((u.len(), t.read().tx_held_full), (HELD_MAX, 1));
        // ENOBUFS (the interface's queue full) while flushing: that frame fails, the rest go
        let mut n = 0;
        assert_eq!(
            u.flush(t0, HDR, &t, |_| {
                n += 1;
                if n == 1 {
                    Err(libc::ENOBUFS)
                } else {
                    Ok(())
                }
            }),
            None
        );
        let c = t.read();
        assert_eq!((c.tx_failed, c.tx_last_errno, c.tx_wait_sent), (1, libc::ENOBUFS, HELD_MAX as u64 - 1));
        // a failed send of the guest's own: not held
        assert_eq!(u.send(&packet(0), HDR, t0, &t, |_| Err(libc::ENETDOWN)), Err(libc::ENETDOWN));
        assert!(u.is_empty());
    }

    /// The same on a real socket: a datagram socket pair whose peer does not read answers EAGAIN, and poll(2)'s
    /// POLLOUT comes once the peer reads; the held frames go out then, in order.
    #[test]
    fn a_real_socket() {
        let mut fds = [0; 2];
        // SAFETY: socketpair writes two fds, owned below.
        let rc = unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_DGRAM | libc::SOCK_CLOEXEC, 0, fds.as_mut_ptr()) };
        assert_eq!(rc, 0);
        // SAFETY: two new fds of ours.
        let (a, b) = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
        // the smallest send buffer the kernel allows: a few frames fill it
        let small: i32 = 1;
        // SAFETY: an int option of our socket.
        unsafe { libc::setsockopt(a.as_raw_fd(), libc::SOL_SOCKET, libc::SO_SNDBUF, (&small as *const i32).cast(), 4) };
        let send = |p: &[u8]| {
            // SAFETY: our socket and buffer.
            if unsafe { libc::send(a.as_raw_fd(), p.as_ptr().cast(), p.len(), libc::MSG_DONTWAIT) } < 0 {
                Err(std::io::Error::last_os_error().raw_os_error().unwrap_or(0))
            } else {
                Ok(())
            }
        };
        let pollout = |ms| {
            let mut p = libc::pollfd { fd: a.as_raw_fd(), events: libc::POLLOUT, revents: 0 };
            // SAFETY: one pollfd of ours.
            unsafe { libc::poll(&mut p, 1, ms) };
            p.revents & libc::POLLOUT != 0
        };
        let (u, t) = (Uplink::default(), Traffic::default());
        let t0 = Instant::now();
        let mut n = 0u8;
        let first = loop {
            n += 1;
            assert!(n < 200, "the send buffer never filled");
            match u.send(&packet(n), HDR, t0, &t, send).unwrap() {
                Sent::Now => continue,
                held => break held,
            }
        };
        assert_eq!(first, Sent::Held { first: true });
        assert_eq!(u.send(&packet(n + 1), HDR, t0, &t, send), Ok(Sent::Held { first: false }));
        assert!(!pollout(0), "the peer's queue is full");
        // the peer reads what came before
        let mut buf = [0u8; 128];
        for i in 1..n {
            // SAFETY: our socket and buffer.
            let got = unsafe { libc::recv(b.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len(), libc::MSG_DONTWAIT) };
            assert!(got == 70 && buf[HDR] == i);
        }
        assert!(pollout(1000));
        assert_eq!(u.flush(t0, HDR, &t, send), None);
        for i in [n, n + 1] {
            // SAFETY: our socket and buffer.
            let got = unsafe { libc::recv(b.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len(), libc::MSG_DONTWAIT) };
            assert!(got == 70 && buf[HDR] == i, "in order");
        }
        let c = t.read();
        assert_eq!((c.tx_frames, c.tx_waited, c.tx_wait_sent, c.tx_failed), (u64::from(n) + 1, 2, 2, 0));
    }
}
