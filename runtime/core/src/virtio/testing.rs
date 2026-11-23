// SPDX-License-Identifier: MIT
//! A guest's side of the virtio devices for host tests (test_xnuhost.c's `driver_*` and test_bridges.c's `guest_up`,
//! `post`, `reap`): a guest made of plain host memory, driven over the device's registers as a guest driver would.
//!
//! [`install`] puts in the hooks the tests need: guest RAM where a guest physical address is the host address
//! ([`crate::hooks::identity_ram`]) and an interrupt recorder ([`spi_raised`], [`spi_level`]). [`GuestRam`] is memory for
//! the rings and buffers; [`Driver`] brings a device up and moves chains through its queues.
use crate::hooks;
use crate::mmio::MmioDevice;
use std::alloc::{alloc_zeroed, dealloc, Layout};
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

const LINES: usize = 1024;
static RAISED: [AtomicU64; LINES] = [const { AtomicU64::new(0) }; LINES];
static LEVEL: [AtomicBool; LINES] = [const { AtomicBool::new(false) }; LINES];

fn record_spi(intid: u32, level: bool) {
    let i = intid as usize % LINES;
    if level {
        RAISED[i].fetch_add(1, Ordering::Relaxed);
    }
    LEVEL[i].store(level, Ordering::Relaxed);
}

/// Installs identity guest RAM and the interrupt recorder (idempotent; every test may call it).
pub fn install() {
    hooks::install_ram(hooks::identity_ram);
    hooks::install_spi(record_spi);
}

/// How often line `intid` was raised since the start.
pub fn spi_raised(intid: u32) -> u64 {
    RAISED[intid as usize % LINES].load(Ordering::Relaxed)
}

/// Line `intid`'s level now.
pub fn spi_level(intid: u32) -> bool {
    LEVEL[intid as usize % LINES].load(Ordering::Relaxed)
}

/// Zeroed, page-aligned host memory whose "guest physical" address is its host address ([`install`]'s RAM).
pub struct GuestRam {
    ptr: NonNull<u8>,
    len: usize,
}

// SAFETY: plain memory owned by the GuestRam; access goes through raw copies (the device side writes it from other
// threads as guest RAM is written).
unsafe impl Send for GuestRam {}
// SAFETY: as above.
unsafe impl Sync for GuestRam {}

impl std::fmt::Debug for GuestRam {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "GuestRam({:#x}+{:#x})", self.ptr.as_ptr() as usize, self.len)
    }
}

impl GuestRam {
    fn layout(len: usize) -> Layout {
        Layout::from_size_align(len.max(1), 4096).expect("guest RAM layout")
    }

    /// `len` bytes of zeroed memory.
    pub fn new(len: usize) -> GuestRam {
        // SAFETY: a non-zero size and a valid alignment.
        let p = unsafe { alloc_zeroed(Self::layout(len)) };
        GuestRam { ptr: NonNull::new(p).expect("guest RAM allocation"), len }
    }

    /// Its size.
    pub fn len(&self) -> usize {
        self.len
    }

    /// Whether it is empty.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The guest physical address of byte `offset` (= its host address).
    pub fn pa(&self, offset: usize) -> u64 {
        assert!(offset <= self.len, "offset {offset:#x} outside the guest RAM");
        self.ptr.as_ptr() as u64 + offset as u64
    }

    fn at(&self, offset: usize, len: usize) -> *mut u8 {
        assert!(
            offset.checked_add(len).is_some_and(|end| end <= self.len),
            "{offset:#x}+{len:#x} outside the guest RAM"
        );
        // SAFETY: inside the allocation (checked above).
        unsafe { self.ptr.as_ptr().add(offset) }
    }

    /// Copies `data` in at `offset`.
    pub fn write(&self, offset: usize, data: &[u8]) {
        // SAFETY: the range is inside the allocation; raw copies make no reference the device side could alias.
        unsafe { std::ptr::copy_nonoverlapping(data.as_ptr(), self.at(offset, data.len()), data.len()) }
    }

    /// Copies `out.len()` bytes out from `offset`.
    pub fn read_into(&self, offset: usize, out: &mut [u8]) {
        // SAFETY: as in write.
        unsafe { std::ptr::copy_nonoverlapping(self.at(offset, out.len()), out.as_mut_ptr(), out.len()) }
    }

    /// `len` bytes from `offset`.
    pub fn read(&self, offset: usize, len: usize) -> Vec<u8> {
        let mut v = vec![0; len];
        self.read_into(offset, &mut v);
        v
    }

    /// Sets `len` bytes from `offset` to `byte`.
    pub fn fill(&self, offset: usize, byte: u8, len: usize) {
        // SAFETY: as in write.
        unsafe { std::ptr::write_bytes(self.at(offset, len), byte, len) }
    }

    /// The little-endian u16 at `offset`.
    pub fn u16(&self, offset: usize) -> u16 {
        u16::from_le_bytes(self.read(offset, 2).try_into().unwrap())
    }

    /// The little-endian u32 at `offset`.
    pub fn u32(&self, offset: usize) -> u32 {
        u32::from_le_bytes(self.read(offset, 4).try_into().unwrap())
    }

    /// The little-endian u64 at `offset`.
    pub fn u64(&self, offset: usize) -> u64 {
        u64::from_le_bytes(self.read(offset, 8).try_into().unwrap())
    }

    /// Writes a little-endian u16 at `offset`.
    pub fn set_u16(&self, offset: usize, v: u16) {
        self.write(offset, &v.to_le_bytes())
    }

    /// Writes a little-endian u32 at `offset`.
    pub fn set_u32(&self, offset: usize, v: u32) {
        self.write(offset, &v.to_le_bytes())
    }

    /// Writes a little-endian u64 at `offset`.
    pub fn set_u64(&self, offset: usize, v: u64) {
        self.write(offset, &v.to_le_bytes())
    }
}

impl Drop for GuestRam {
    fn drop(&mut self) {
        // SAFETY: allocated by `new` with this layout.
        unsafe { dealloc(self.ptr.as_ptr(), Self::layout(self.len)) }
    }
}

/// A part of a chain the driver posts: guest physical address, length, device-writable.
pub type Part = (u64, u32, bool);

/// The bytes of guest memory a queue's rings take ([`Driver::up`]): descriptors at +0, avail at +0x1000, used at
/// +0x2000.
pub const RING_BYTES: u64 = 0x4000;

#[derive(Clone, Copy, Debug, Default)]
struct DriverQueue {
    pa: u64,
    avail: u16,
    used_seen: u16,
    next_desc: u16,
}

/// A guest's virtio-mmio driver over a device's registers, its rings in guest memory reached through
/// [`crate::hooks::ram`] (so it works over any installed RAM, not only [`GuestRam`]).
pub struct Driver {
    dev: Arc<dyn MmioDevice>,
    qsize: u16,
    q: Vec<DriverQueue>,
}

impl std::fmt::Debug for Driver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Driver({} queues of {})", self.q.len(), self.qsize)
    }
}

fn mem(pa: u64, len: u64) -> *mut u8 {
    hooks::ram(pa, len).unwrap_or_else(|| panic!("guest {pa:#x}+{len:#x} is not RAM")).as_ptr()
}

fn poke<T: Copy>(pa: u64, v: T) {
    // SAFETY: `mem` mapped size_of::<T>() bytes of guest RAM at pa; an unaligned raw write.
    unsafe { mem(pa, size_of::<T>() as u64).cast::<T>().write_unaligned(v) }
}

fn peek<T: Copy>(pa: u64) -> T {
    // SAFETY: as in poke, a read.
    unsafe { mem(pa, size_of::<T>() as u64).cast::<T>().read_unaligned() }
}

/// The guest's view of the ring index at `pa` (volatile: the device writes it from other threads).
fn peek_index(pa: u64) -> u16 {
    // SAFETY: as in peek; volatile so that a test loop sees the device's update.
    unsafe { mem(pa, 2).cast::<u16>().read_volatile() }
}

impl Driver {
    fn r(&self, off: u64) -> u32 {
        self.dev.mmio_read(off, 4) as u32
    }

    fn w(&self, off: u64, v: u32) {
        self.dev.mmio_write(off, 4, u64::from(v))
    }

    /// Brings the device up as a guest driver does (`driver_init`): reset, ACKNOWLEDGE, DRIVER, the features (bits
    /// 0-31 of `features`, and VERSION_1 with bits 32-63), FEATURES_OK, then `nqueues` queues of `qsize` with their
    /// rings at `rings + q * RING_BYTES` (zeroed), and DRIVER_OK. `None` if the device refuses a step.
    pub fn up(dev: Arc<dyn MmioDevice>, features: u64, nqueues: u32, qsize: u16, rings: u64) -> Option<Driver> {
        let mut d = Driver { dev, qsize, q: Vec::new() };
        if d.r(0) != 0x7472_6976 || d.r(4) != 2 {
            return None;
        }
        d.w(0x70, 0);
        d.w(0x70, 1);
        d.w(0x70, 3);
        d.w(0x24, 0);
        d.w(0x20, features as u32);
        d.w(0x24, 1);
        d.w(0x20, 1 | (features >> 32) as u32);
        d.w(0x70, 11);
        if d.r(0x70) & 8 == 0 {
            return None;
        }
        for q in 0..nqueues {
            let pa = rings + u64::from(q) * RING_BYTES;
            // SAFETY: mem mapped RING_BYTES of guest RAM at pa.
            unsafe { std::ptr::write_bytes(mem(pa, RING_BYTES), 0, RING_BYTES as usize) };
            d.w(0x30, q);
            if d.r(0x34) < u32::from(qsize) {
                return None;
            }
            d.w(0x38, u32::from(qsize));
            for (reg, at) in [(0x80, pa), (0x90, pa + 0x1000), (0xa0, pa + 0x2000)] {
                d.w(reg, at as u32);
                d.w(reg + 4, (at >> 32) as u32);
            }
            d.w(0x44, 1);
            d.q.push(DriverQueue { pa, ..DriverQueue::default() });
        }
        d.w(0x70, 15);
        (d.r(0x70) & 15 == 15).then_some(d)
    }

    /// A register read of 4 bytes.
    pub fn read(&self, offset: u64) -> u32 {
        self.r(offset)
    }

    /// A register write of 4 bytes.
    pub fn write(&self, offset: u64, value: u32) {
        self.w(offset, value)
    }

    /// Kicks queue `q` (QueueNotify).
    pub fn kick(&self, q: u32) {
        self.w(0x50, q)
    }

    fn queue(&self, q: u32) -> &DriverQueue {
        &self.q[q as usize]
    }

    /// The guest physical address of queue `q`'s descriptor table (`queue_pa`).
    pub fn ring_pa(&self, q: u32) -> u64 {
        self.queue(q).pa
    }

    /// The device's used index of queue `q`.
    pub fn used_idx(&self, q: u32) -> u16 {
        peek_index(self.queue(q).pa + 0x2002)
    }

    /// The used entries the driver has taken of queue `q`.
    pub fn seen(&self, q: u32) -> u16 {
        self.queue(q).used_seen
    }

    /// The used element `n` (modulo the size) of queue `q`: (id, length).
    pub fn used_elem(&self, q: u32, n: u16) -> (u32, u32) {
        let at = self.queue(q).pa + 0x2004 + 8 * u64::from(n % self.qsize);
        (peek(at), peek(at + 4))
    }

    /// The guest address descriptor `i` of queue `q` names.
    pub fn desc_addr(&self, q: u32, i: u32) -> u64 {
        peek(self.queue(q).pa + 16 * u64::from(i))
    }

    fn write_chain(&mut self, q: u32, first: u16, parts: &[Part]) {
        let (pa, qsize) = (self.queue(q).pa, self.qsize);
        for (i, &(addr, len, writable)) in parts.iter().enumerate() {
            let at = (first + i as u16) % qsize;
            let next = (at + 1) % qsize;
            let flags = (u16::from(writable) * 2) | u16::from(i + 1 < parts.len());
            let d = pa + 16 * u64::from(at);
            poke(d, addr);
            poke(d + 8, len);
            poke(d + 12, flags);
            poke(d + 14, next);
        }
        let dq = &mut self.q[q as usize];
        poke(pa + 0x1004 + 2 * u64::from(dq.avail % qsize), first);
        dq.avail = dq.avail.wrapping_add(1);
        poke(pa + 0x1002, dq.avail);
    }

    /// Posts a chain from descriptor `first` and kicks (`driver_send_at`): the used length of the next used element,
    /// or `None` when the device returned nothing (or `kick` is false: only posted).
    pub fn send_at(&mut self, q: u32, first: u16, parts: &[Part], kick: bool) -> Option<u32> {
        self.write_chain(q, first, parts);
        if !kick {
            return None;
        }
        self.kick(q);
        self.reap(q).map(|(_, len)| len)
    }

    /// [`send_at`](Self::send_at) where single buffers take one descriptor each and longer chains alternate between
    /// the queue's halves (`driver_send`).
    pub fn send(&mut self, q: u32, parts: &[Part], kick: bool) -> Option<u32> {
        let avail = self.queue(q).avail;
        let first = if parts.len() == 1 { avail % self.qsize } else { (avail % 2) * (self.qsize / 2) };
        self.send_at(q, first, parts, kick)
    }

    /// Posts a chain on the next free descriptors (round the table) and kicks (test_bridges.c's `post`); returns its
    /// head. Do not mix with [`send`](Self::send) on one queue.
    pub fn post(&mut self, q: u32, parts: &[Part]) -> u16 {
        let head = self.queue(q).next_desc;
        self.write_chain(q, head, parts);
        let dq = &mut self.q[q as usize];
        dq.next_desc = ((usize::from(dq.next_desc) + parts.len()) % usize::from(self.qsize)) as u16;
        self.kick(q);
        head
    }

    /// The next used element of queue `q` the device returned, `(id, length)`, or `None` (`reap`).
    pub fn reap(&mut self, q: u32) -> Option<(u32, u32)> {
        let seen = self.queue(q).used_seen;
        if self.used_idx(q) == seen {
            return None;
        }
        let elem = self.used_elem(q, seen);
        self.q[q as usize].used_seen = seen.wrapping_add(1);
        Some(elem)
    }
}
