// SPDX-License-Identifier: MIT
//! The frame ring: shared memory between the camera helper (the writer) and xnuhost (the reader).
//!
//! A memfd made by the helper and mapped by xnuhost. One page of header, then `slots` places of one page (lock,
//! [`OcFrame`]) and `slot_bytes` of picture. The writer fills the place after the newest one and then names it in
//! `latest`; a reader copies the newest and checks the place's lock (odd while it is written, changed when it was
//! written again) to see that the copy is whole.
use crate::proto::{OcFrame, OC_FRAME_MAGIC};
use std::io;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd};
use std::ptr::NonNull;
use std::sync::atomic::{fence, AtomicU32, AtomicU64, Ordering};

pub const CAM_RING_MAGIC: &[u8; 8] = b"OCRING1\0";
pub const CAM_RING_VERSION: u32 = 1;
pub const CAM_RING_PAGE: usize = 4096;
pub const CAM_RING_SLOTS: u32 = 4;

// The header page: magic[8], version, slots, slot_bytes, reserved (u32 each), then latest (u64, atomic:
// sequence << 8 | slot of the newest whole frame; 0 = none).
const OFF_VERSION: usize = 8;
const OFF_SLOTS: usize = 12;
const OFF_SLOT_BYTES: usize = 16;
const OFF_LATEST: usize = 24;
// A place's first page: lock (u32, atomic), reserved (u32), the frame's header.
const OFF_FRAME: usize = 8;

/// Why a read gave no frame (`cam_ring_read`'s -EAGAIN, -EMSGSIZE, -EIO).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadError {
    /// Nothing new (or the writer came round four times during the copy).
    Again,
    /// The frame does not fit the buffer given.
    TooBig,
    /// The ring's contents make no sense.
    Corrupt,
}

impl ReadError {
    pub fn errno(self) -> i32 {
        match self {
            ReadError::Again => libc::EAGAIN,
            ReadError::TooBig => libc::EMSGSIZE,
            ReadError::Corrupt => libc::EIO,
        }
    }
}

fn slot_span(slot_bytes: u32) -> usize {
    CAM_RING_PAGE + (slot_bytes as usize).div_ceil(CAM_RING_PAGE) * CAM_RING_PAGE
}

fn ring_size(slots: u32, slot_bytes: u32) -> usize {
    CAM_RING_PAGE + slots as usize * slot_span(slot_bytes)
}

fn errno() -> io::Error {
    io::Error::last_os_error()
}

/// A shared mapping of the ring's memfd, unmapped on drop.
struct Mapping {
    at: NonNull<u8>,
    size: usize,
    fd: OwnedFd,
    slots: u32,
    // copied at create/attach: the mapping's fields are never trusted again
    slot_bytes: u32,
}

// SAFETY: the mapping is plain shared memory; every access that may race goes through atomics (the locks and
// `latest`) or is checked by the slot's lock afterwards.
unsafe impl Send for Mapping {}
// SAFETY: as above.
unsafe impl Sync for Mapping {}

impl Mapping {
    fn map(fd: BorrowedFd<'_>, size: usize, writable: bool) -> io::Result<NonNull<u8>> {
        let prot = if writable { libc::PROT_READ | libc::PROT_WRITE } else { libc::PROT_READ };
        // SAFETY: a fresh shared mapping of a descriptor we own; the result is checked.
        let at = unsafe { libc::mmap(std::ptr::null_mut(), size, prot, libc::MAP_SHARED, fd.as_raw_fd(), 0) };
        if at == libc::MAP_FAILED {
            return Err(errno());
        }
        Ok(NonNull::new(at.cast()).expect("mmap gave NULL"))
    }

    fn u32_at(&self, offset: usize) -> &AtomicU32 {
        debug_assert!(offset + 4 <= self.size && offset % 4 == 0);
        // SAFETY: inside the mapping, 4-aligned (page-relative offsets of the layout above).
        unsafe { &*self.at.as_ptr().add(offset).cast::<AtomicU32>() }
    }

    fn latest(&self) -> &AtomicU64 {
        // SAFETY: offset 24 of the first page, 8-aligned.
        unsafe { &*self.at.as_ptr().add(OFF_LATEST).cast::<AtomicU64>() }
    }

    fn slot_offset(&self, slot: u32) -> usize {
        CAM_RING_PAGE + slot as usize * slot_span(self.slot_bytes)
    }

    fn lock(&self, slot: u32) -> &AtomicU32 {
        self.u32_at(self.slot_offset(slot))
    }

    fn data_ptr(&self, slot: u32) -> *mut u8 {
        // SAFETY: the place's picture area is inside the mapping (checked at create/attach).
        unsafe { self.at.as_ptr().add(self.slot_offset(slot) + CAM_RING_PAGE) }
    }

    fn frame_ptr(&self, slot: u32) -> *mut OcFrame {
        // SAFETY: the place's header is inside its first page.
        unsafe { self.at.as_ptr().add(self.slot_offset(slot) + OFF_FRAME).cast::<OcFrame>() }
    }
}

impl Drop for Mapping {
    fn drop(&mut self) {
        // SAFETY: our own mapping of `size` bytes, unmapped once.
        unsafe { libc::munmap(self.at.as_ptr().cast(), self.size) };
    }
}

/// The helper's end: makes the ring and fills it.
pub struct RingWriter {
    map: Mapping,
    /// the place to fill next
    next: u32,
}

impl RingWriter {
    /// A ring of `slots` places (2..255) of `slot_bytes` (`cam_ring_create`); EINVAL for other counts.
    pub fn create(slots: u32, slot_bytes: u32) -> io::Result<RingWriter> {
        if !(2..=255).contains(&slots) || slot_bytes == 0 {
            return Err(io::Error::from_raw_os_error(libc::EINVAL));
        }
        let size = ring_size(slots, slot_bytes);
        // SAFETY: a NUL-terminated name; the result is checked.
        let raw = unsafe { libc::memfd_create(c"oneios-camera".as_ptr(), libc::MFD_CLOEXEC) };
        if raw < 0 {
            return Err(errno());
        }
        // SAFETY: a fresh descriptor that nothing else owns.
        let fd = unsafe { OwnedFd::from_raw_fd(raw) };
        // SAFETY: our descriptor; the size is a plain number.
        if unsafe { libc::ftruncate(fd.as_raw_fd(), size as libc::off_t) } != 0 {
            return Err(errno());
        }
        let at = Mapping::map(fd.as_fd(), size, true)?;
        let map = Mapping { at, size, fd, slots, slot_bytes };
        // SAFETY: the header page is ours to write before anyone else has the descriptor.
        unsafe { std::ptr::copy_nonoverlapping(CAM_RING_MAGIC.as_ptr(), map.at.as_ptr(), 8) };
        map.u32_at(OFF_VERSION).store(CAM_RING_VERSION, Ordering::Relaxed);
        map.u32_at(OFF_SLOTS).store(slots, Ordering::Relaxed);
        map.u32_at(OFF_SLOT_BYTES).store(slot_bytes, Ordering::Relaxed);
        Ok(RingWriter { map, next: 0 })
    }

    /// The ring's descriptor (passed to xnuhost, or duplicated for a reader of one's own).
    pub fn fd(&self) -> BorrowedFd<'_> {
        self.map.fd.as_fd()
    }

    pub fn slots(&self) -> u32 {
        self.map.slots
    }

    pub fn slot_bytes(&self) -> u32 {
        self.map.slot_bytes
    }

    /// Starts writing the place after the last one (its lock goes odd) and returns it (`cam_ring_begin`).
    pub fn begin(&mut self) -> u32 {
        let at = self.next;
        self.next = (at + 1) % self.map.slots;
        self.map.lock(at).fetch_add(1, Ordering::Acquire); // odd: being written
        fence(Ordering::Release); // the lock before any byte of the picture
        at
    }

    /// The picture area of place `slot`.
    pub fn picture(&mut self, slot: u32) -> &mut [u8] {
        assert!(slot < self.map.slots);
        // SAFETY: the place's picture area, slot_bytes long, inside our writable mapping; `&mut self` keeps it ours.
        unsafe { std::slice::from_raw_parts_mut(self.map.data_ptr(slot), self.map.slot_bytes as usize) }
    }

    /// Publishes place `slot` with `frame` as its header (`cam_ring_commit`).
    pub fn commit(&mut self, slot: u32, frame: &OcFrame) {
        assert!(slot < self.map.slots);
        // SAFETY: the place's header inside our writable mapping; volatile, since a reader may look at it meanwhile.
        unsafe { std::ptr::write_volatile(self.map.frame_ptr(slot), *frame) };
        self.map.lock(slot).fetch_add(1, Ordering::Release); // even: whole
        self.map.latest().store(frame.sequence << 8 | u64::from(slot), Ordering::SeqCst);
    }
}

/// xnuhost's end (and camctl's): maps a ring read-only and copies frames out of it.
pub struct RingReader {
    map: Mapping,
}

impl RingReader {
    /// Maps the ring of `fd` and checks it (`cam_ring_attach`): EINVAL for what is not a ring. Keeps `fd`.
    pub fn attach(fd: OwnedFd) -> io::Result<RingReader> {
        // SAFETY: a zeroed stat buffer for fstat of a descriptor we own.
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        // SAFETY: as above.
        if unsafe { libc::fstat(fd.as_raw_fd(), &mut st) } != 0 {
            return Err(errno());
        }
        let einval = || io::Error::from_raw_os_error(libc::EINVAL);
        if st.st_size < (2 * CAM_RING_PAGE) as libc::off_t {
            return Err(einval());
        }
        let size = st.st_size as usize;
        // Read-only: the reader never writes, and a wrong helper cannot be handed a way into xnuhost.
        let at = Mapping::map(fd.as_fd(), size, false)?;
        let mut header = [0u8; 32];
        // SAFETY: the first page is mapped (size >= 2 pages).
        unsafe { std::ptr::copy_nonoverlapping(at.as_ptr(), header.as_mut_ptr(), 32) };
        let word = |o: usize| u32::from_ne_bytes(header[o..o + 4].try_into().unwrap());
        let (version, slots, slot_bytes) = (word(OFF_VERSION), word(OFF_SLOTS), word(OFF_SLOT_BYTES));
        let map = Mapping { at, size, fd, slots, slot_bytes };
        if &header[..8] != CAM_RING_MAGIC
            || version != CAM_RING_VERSION
            || !(2..=255).contains(&slots)
            || slot_bytes == 0
            || slot_bytes > 1 << 28
            || ring_size(slots, slot_bytes) > size
        {
            return Err(einval()); // `map` unmaps on the way out
        }
        Ok(RingReader { map })
    }

    pub fn slots(&self) -> u32 {
        self.map.slots
    }

    pub fn slot_bytes(&self) -> u32 {
        self.map.slot_bytes
    }

    /// The newest frame's sequence, 0 = none (`cam_ring_newest`).
    pub fn newest(&self) -> u64 {
        self.map.latest().load(Ordering::SeqCst) >> 8
    }

    /// Copies the newest frame after `*last` (then updated) into `dst`, its header into `frame` (`cam_ring_read`).
    pub fn read(&self, last: &mut u64, frame: &mut OcFrame, dst: &mut [u8]) -> Result<(), ReadError> {
        for _ in 0..4 {
            let latest = self.map.latest().load(Ordering::SeqCst);
            let sequence = latest >> 8;
            let slot = (latest & 0xff) as u32;
            if latest == 0 || sequence == *last {
                return Err(ReadError::Again);
            }
            if slot >= self.map.slots {
                return Err(ReadError::Corrupt);
            }
            let lock = self.map.lock(slot);
            let before = lock.load(Ordering::Acquire);
            if before & 1 != 0 {
                continue; // the writer came round to it: take the next newest
            }
            // SAFETY: the place's header inside the mapping; a racing write is caught by the lock check after.
            let header = unsafe { std::ptr::read_volatile(self.map.frame_ptr(slot)) };
            let whole = |lock: &AtomicU32| {
                fence(Ordering::Acquire); // every byte read before the lock is looked at again
                lock.load(Ordering::Relaxed) == before
            };
            if header.magic != OC_FRAME_MAGIC || header.bytes > self.map.slot_bytes {
                if !whole(lock) {
                    continue;
                }
                return Err(ReadError::Corrupt);
            }
            let bytes = header.bytes as usize;
            if bytes > dst.len() {
                return Err(ReadError::TooBig);
            }
            // SAFETY: `bytes` <= slot_bytes, so the source is inside the place's picture area; dst is long enough.
            unsafe { std::ptr::copy_nonoverlapping(self.map.data_ptr(slot), dst.as_mut_ptr(), bytes) };
            if !whole(lock) {
                continue; // torn: written again during the copy
            }
            *frame = header;
            *last = header.sequence;
            return Ok(());
        }
        Err(ReadError::Again)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::OC_FMT_NV12;
    use std::os::fd::AsFd;
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;

    fn put_frame(w: &mut RingWriter, sequence: u64, bytes: u32) {
        let slot = w.begin();
        w.picture(slot)[..bytes as usize].fill(sequence as u8);
        let f = OcFrame {
            magic: OC_FRAME_MAGIC,
            sequence,
            timestamp_ns: sequence * 1000,
            width: 64,
            height: 32,
            stride: 64,
            format: OC_FMT_NV12,
            bytes,
            ..Default::default()
        };
        w.commit(slot, &f);
    }

    fn reader_of(w: &RingWriter) -> RingReader {
        RingReader::attach(w.fd().try_clone_to_owned().unwrap()).unwrap()
    }

    fn header(sequence: u64, bytes: u32) -> OcFrame {
        OcFrame { magic: OC_FRAME_MAGIC, sequence, bytes, ..Default::default() }
    }

    #[test]
    fn newest_whole_frames() {
        let mut buffer = vec![0u8; 70000];
        let mut f = OcFrame::default();
        let mut last = 0;
        assert_eq!(RingWriter::create(1, 100).err().and_then(|e| e.raw_os_error()), Some(libc::EINVAL));
        let mut w = RingWriter::create(2, 60000).unwrap();
        let r = reader_of(&w);
        assert!(r.slots() == 2 && r.slot_bytes() == 60000);
        assert_eq!(r.read(&mut last, &mut f, &mut buffer), Err(ReadError::Again));
        assert_eq!(r.newest(), 0);
        put_frame(&mut w, 1, 3000);
        assert_eq!(r.newest(), 1);
        assert_eq!(r.read(&mut last, &mut f, &mut buffer[..100]), Err(ReadError::TooBig));
        assert_eq!(last, 0);
        assert_eq!(r.read(&mut last, &mut f, &mut buffer), Ok(()));
        assert!(last == 1 && f.sequence == 1 && f.bytes == 3000 && f.timestamp_ns == 1000);
        assert!(buffer[0] == 1 && buffer[2999] == 1);
        assert_eq!(r.read(&mut last, &mut f, &mut buffer), Err(ReadError::Again));
        // The reader takes the newest, not the next.
        for n in 2..=9 {
            put_frame(&mut w, n, 3000);
        }
        assert_eq!(r.read(&mut last, &mut f, &mut buffer), Ok(()));
        assert!(f.sequence == 9 && buffer[100] == 9);
        // The writer is in another place: the newest stays readable. In the newest's own place: nothing whole to read.
        put_frame(&mut w, 10, 3000);
        let slot = w.begin();
        assert_eq!(r.read(&mut last, &mut f, &mut buffer), Ok(()));
        assert_eq!(f.sequence, 10);
        w.commit(slot, &header(11, 10));
        let other = w.begin(); // the place frame 10 is in; latest names frame 11
        let slot = w.begin(); // the place frame 11 is in
        assert_eq!(r.read(&mut last, &mut f, &mut buffer), Err(ReadError::Again));
        assert_eq!(last, 10);
        w.commit(other, &header(12, 10));
        w.commit(slot, &header(13, 10));
        assert_eq!(r.read(&mut last, &mut f, &mut buffer), Ok(()));
        assert_eq!(last, 13);
        // A frame that claims more than a place holds is refused.
        let slot = w.begin();
        w.commit(slot, &header(14, 60001));
        assert_eq!(r.read(&mut last, &mut f, &mut buffer), Err(ReadError::Corrupt));
    }

    #[test]
    fn a_writer_at_full_speed() {
        // Every frame read is whole (one value) and matches its header.
        let mut w = RingWriter::create(CAM_RING_SLOTS, 60000).unwrap();
        let r = reader_of(&w);
        let run = Arc::new(AtomicBool::new(true));
        let go = Arc::clone(&run);
        let writer = std::thread::spawn(move || {
            let mut n = 1;
            while go.load(Ordering::Acquire) {
                put_frame(&mut w, n, 60000);
                n += 1;
            }
        });
        let (mut good, mut torn, mut last) = (0, 0, 0);
        let mut f = OcFrame::default();
        let mut buffer = vec![0u8; 70000];
        let mut i = 0;
        while i < 20000 || good < 50 {
            i += 1;
            if r.read(&mut last, &mut f, &mut buffer).is_err() {
                continue;
            }
            let v = f.sequence as u8;
            let whole = f.bytes == 60000 && (0..60000).step_by(997).all(|k| buffer[k] == v) && buffer[59999] == v;
            if whole {
                good += 1;
            } else {
                torn += 1;
            }
        }
        run.store(false, Ordering::Release);
        writer.join().unwrap();
        assert!(good >= 50 && torn == 0, "good {good} torn {torn}");
    }

    #[test]
    fn not_a_ring() {
        // SAFETY: a NUL-terminated name.
        let raw = unsafe { libc::memfd_create(c"not-a-ring".as_ptr(), 0) };
        // SAFETY: a fresh descriptor.
        let fd = unsafe { OwnedFd::from_raw_fd(raw) };
        let file = std::fs::File::from(fd);
        let attach = |file: &std::fs::File| {
            RingReader::attach(file.as_fd().try_clone_to_owned().unwrap()).err().and_then(|e| e.raw_os_error())
        };
        file.set_len(100).unwrap();
        assert_eq!(attach(&file), Some(libc::EINVAL));
        file.set_len(1 << 20).unwrap();
        assert_eq!(attach(&file), Some(libc::EINVAL));
        // A header that says more than the file holds.
        let mut bad = [0u8; 32];
        bad[..8].copy_from_slice(CAM_RING_MAGIC);
        bad[8..12].copy_from_slice(&CAM_RING_VERSION.to_ne_bytes());
        bad[12..16].copy_from_slice(&200u32.to_ne_bytes());
        bad[16..20].copy_from_slice(&(1u32 << 20).to_ne_bytes());
        std::os::unix::fs::FileExt::write_all_at(&file, &bad, 0).unwrap();
        assert_eq!(attach(&file), Some(libc::EINVAL));
    }
}
