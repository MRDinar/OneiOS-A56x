// SPDX-License-Identifier: MIT
//! Port of virtio_blk.c: the virtio-blk device (VirtIO 1.2, 5.2) over blk.rs. A read-only disk offers
//! VIRTIO_BLK_F_RO, which NativeVirtioBlock requires of the verified images.
//!
//! Requests are served inside the guest's notify, on the vCPU that wrote the register (OneiOSHostedDisk polls for the
//! answer), except a flush of a writable disk when the driver accepted F_LATE_FLUSH (OneiOS's own bit, the top device
//! bit, which virtio-blk does not use): then the disk's flush thread runs the fdatasync(2) and completes the request
//! from there, and the driver waits for it asleep (fix-widget-kill-loop, 2026-11-15). Before, the vCPU stayed in the
//! host until the phone's file system had written the Data store out: every APFS transaction, every F_FULLFSYNC and
//! F_BARRIERFSYNC of iOS (SQLite, plists) held a vCPU, with the guest's timers and IPIs on it waiting.
//! Config blk-late-flush=0 does not offer the bit (the flush on the vCPU, as before).
//!
//! A lasting line per disk, once a minute while it is used (docs/booter.md "Disks"):
//!   disk N: 60 s: reads R (M MiB, T ms, slowest S ms), writes W (M MiB, T ms, slowest S ms), flushes F (on its thread:
//!   T ms, mean A ms, slowest S ms)
//! The read and write times are what the vCPUs spent in the host; the flush time is the fdatasync's.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, Weak};

use xh_core::time::monotonic_ns;
use xh_core::virtio::{Device, Info, Viov, Virtio};
use xh_core::{loge, logi};

use crate::blk::Blk;

pub const F_RO: u64 = 1 << 5;
pub const F_BLK_SIZE: u64 = 1 << 6;
pub const F_FLUSH: u64 = 1 << 9;
pub const F_LATE_FLUSH: u64 = 1 << 23;
const T_IN: u32 = 0;
const T_OUT: u32 = 1;
const T_FLUSH: u32 = 4;
const T_GET_ID: u32 = 8;
const S_OK: u8 = 0;
const S_IOERR: u8 = 1;
const S_UNSUPP: u8 = 2;

/// A minute's counts.
#[derive(Default)]
struct Stats {
    reads: u64,
    writes: u64,
    flushes: u64,
    read_bytes: u64,
    write_bytes: u64,
    read_ns: u64,
    write_ns: u64,
    flush_ns: u64,
    read_max_ns: u64,
    write_max_ns: u64,
    flush_max_ns: u64,
}

/// What the flush thread is given: one request at a time, as the driver sends them.
struct Late {
    thread_up: bool,
    tried: bool,
    pending: bool,
    head: u16,
    status: Viov,
    /// a reset drops a flush in progress (it is not completed into the new queue)
    generation: u32,
    s: Stats,
    window_start: u64,
}

/// The device model (blk_dev).
pub struct BlkDev {
    disk: Arc<Blk>,
    index: u32,
    late: Mutex<Late>,
    wake: Condvar,
    /// the device itself, for the flush thread
    me: OnceLock<Weak<Virtio<BlkDev>>>,
}

pub type BlkDevice = Arc<Virtio<BlkDev>>;

fn ms(ns: u64) -> f64 {
    ns as f64 / 1e6
}

impl BlkDev {
    fn late(&self) -> MutexGuard<'_, Late> {
        self.late.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Adds one request's time to the minute and writes the line when the minute is over.
    fn account(&self, l: &mut Late, late_on: bool, kind: u32, bytes: u64, ns: u64, at: u64) {
        let s = &mut l.s;
        match kind {
            T_IN => {
                s.reads += 1;
                s.read_bytes += bytes;
                s.read_ns += ns;
                s.read_max_ns = s.read_max_ns.max(ns);
            }
            T_OUT => {
                s.writes += 1;
                s.write_bytes += bytes;
                s.write_ns += ns;
                s.write_max_ns = s.write_max_ns.max(ns);
            }
            T_FLUSH => {
                s.flushes += 1;
                s.flush_ns += ns;
                s.flush_max_ns = s.flush_max_ns.max(ns);
            }
            _ => {}
        }
        if l.window_start == 0 {
            l.window_start = at;
            return;
        }
        if at - l.window_start < 60_000_000_000 {
            return;
        }
        let s = &l.s;
        logi!(
            "disk {}: {:.0} s: reads {} ({:.1} MiB, {:.0} ms, slowest {:.1} ms), writes {} ({:.1} MiB, {:.0} ms, slowest {:.1} ms), flushes {} ({}: {:.0} ms, mean {:.2} ms, slowest {:.1} ms)",
            self.index,
            (at - l.window_start) as f64 / 1e9,
            s.reads,
            s.read_bytes as f64 / 1048576.0,
            ms(s.read_ns),
            ms(s.read_max_ns),
            s.writes,
            s.write_bytes as f64 / 1048576.0,
            ms(s.write_ns),
            ms(s.write_max_ns),
            s.flushes,
            if late_on { "on its thread" } else { "on the vCPU" },
            ms(s.flush_ns),
            if s.flushes != 0 { ms(s.flush_ns) / s.flushes as f64 } else { 0.0 },
            ms(s.flush_max_ns)
        );
        l.s = Stats::default();
        l.window_start = at;
    }

    /// The flush thread's loop.
    fn flush_thread(dev: Arc<Virtio<BlkDev>>) {
        let b = dev.device();
        let mut l = b.late();
        loop {
            while !l.pending {
                l = b.wake.wait(l).unwrap_or_else(|e| e.into_inner());
            }
            let (head, status, generation) = (l.head, l.status, l.generation);
            drop(l);
            let began = monotonic_ns();
            let rc = b.disk.flush();
            let done = monotonic_ns();
            // The completion goes under the device's lock, as a vCPU's would: a reset between the take and here dropped it.
            let _guard = dev.lock();
            l = b.late();
            b.account(&mut l, true, T_FLUSH, 0, done - began, done);
            if generation == l.generation {
                status.write_at(0, &[if rc.is_ok() { S_OK } else { S_IOERR }]);
                drop(l);
                dev.push(0, head, 1);
                l = b.late();
            }
            l.pending = false; // after the push: wait_flushed then sees the completion
            b.wake.notify_all();
            drop(_guard);
        }
    }

    /// A flush for the flush thread. false when there is no thread (the caller flushes itself).
    fn late_flush(&self, head: u16, status: Viov) -> bool {
        let mut l = self.late();
        if !l.tried {
            l.tried = true;
            let me = self.me.get().and_then(Weak::upgrade);
            let spawned = me.map(|dev| {
                xh_core::thread::spawn_with_stack("xh-blk-flush", 64 << 10, move || Self::flush_thread(dev))
            });
            match spawned {
                Some(Ok(_)) => l.thread_up = true,
                Some(Err(e)) => {
                    loge!(
                        "disk {}: no flush thread ({}): flushes stay on the vCPU",
                        self.index,
                        xh_core::sys::error_text(&e)
                    )
                }
                None => {}
            }
        }
        let queued = l.thread_up && !l.pending;
        if queued {
            l.pending = true;
            l.head = head;
            l.status = status;
            self.wake.notify_all();
        }
        queued
    }

    /// Waits until no late flush is in progress (the tests: the completion comes from the flush thread).
    pub fn wait_flushed(&self) {
        let mut l = self.late();
        while l.pending {
            l = self.wake.wait(l).unwrap_or_else(|e| e.into_inner());
        }
    }

    pub fn disk(&self) -> &Arc<Blk> {
        &self.disk
    }
}

impl Device for BlkDev {
    fn handle(&self, dev: &Virtio<Self>, _queue: u32, head: u16, iov: &[Viov]) -> Option<u32> {
        let n = iov.len();
        // header (readable, 16 bytes), data parts, status (writable, 1 byte, last)
        if n < 2 || iov[0].writable() || iov[0].len() != 16 || !iov[n - 1].writable() || iov[n - 1].is_empty() {
            return Some(0);
        }
        let mut header = [0u8; 16];
        iov[0].read_at(0, &mut header);
        let kind = u32::from_le_bytes(header[..4].try_into().expect("4 bytes"));
        let sector = u64::from_le_bytes(header[8..].try_into().expect("8 bytes"));
        let late_on = dev.driver_features() & F_LATE_FLUSH != 0;
        if kind == T_FLUSH && late_on && self.late_flush(head, iov[n - 1]) {
            return None; // completed by the flush thread
        }
        let began = monotonic_ns();
        let mut status = S_OK;
        let start = sector.wrapping_mul(512);
        let (mut offset, mut written) = (start, 0u64);
        if sector > u64::MAX / 512 {
            status = S_IOERR;
        }
        for part in &iov[1..n - 1] {
            if status != S_OK {
                break;
            }
            match kind {
                T_IN => match self.disk.read_iov(offset, part) {
                    Ok(()) => written += part.len() as u64,
                    Err(_) => status = S_IOERR,
                },
                T_OUT => {
                    if part.writable() || self.disk.write_iov(offset, part).is_err() {
                        status = S_IOERR;
                    }
                }
                T_GET_ID => {
                    if part.writable() && part.len() >= 20 {
                        let mut id = [0u8; 20];
                        id[..14].copy_from_slice(b"oneios-xnuhost");
                        part.write_at(0, &id);
                        written += 20;
                    }
                }
                _ => status = S_UNSUPP,
            }
            offset = offset.wrapping_add(part.len() as u64);
        }
        if kind == T_FLUSH {
            status = if self.disk.flush().is_ok() { S_OK } else { S_IOERR };
        } else if !matches!(kind, T_IN | T_OUT | T_GET_ID) {
            status = S_UNSUPP;
        }
        let done = monotonic_ns();
        let bytes = offset.wrapping_sub(start);
        self.account(&mut self.late(), late_on, kind, bytes, done - began, done);
        // Debugging: XNUHOST_BLKLOG=1 logs every request (which disk, what, where, how it ended).
        static TRACE: OnceLock<bool> = OnceLock::new();
        if *TRACE.get_or_init(|| std::env::var_os("XNUHOST_BLKLOG").is_some()) {
            logi!(
                "blk {:p}: type {} sector {}, {} parts, {} bytes, status {}",
                Arc::as_ptr(&self.disk),
                kind,
                sector,
                n - 2,
                bytes,
                status
            );
        }
        iov[n - 1].write_at(0, &[status]);
        Some(written as u32 + 1)
    }

    fn config_read(&self, _dev: &Virtio<Self>, offset: u32, size: u32) -> u32 {
        // capacity (u64), size_max, seg_max, geometry (4 bytes), blk_size, the rest: 64 bytes
        let mut c = [0u8; 64];
        c[..8].copy_from_slice(&(self.disk.bytes() / 512).to_le_bytes());
        c[12..16].copy_from_slice(&62u32.to_le_bytes());
        c[20..24].copy_from_slice(&512u32.to_le_bytes());
        let (offset, size) = (offset as usize, size as usize);
        let mut v = [0u8; 4];
        if offset < c.len() && size <= c.len() - offset {
            let n = size.min(4);
            v[..n].copy_from_slice(&c[offset..offset + n]);
        }
        u32::from_le_bytes(v)
    }

    /// A reset (status 0, under the device's lock) drops a late flush in progress: its completion would land in the
    /// driver's new queue.
    fn reset(&self, _dev: &Virtio<Self>) {
        let mut l = self.late();
        l.generation = l.generation.wrapping_add(1);
    }
}

/// xh_virtio_blk_create: the device of `disk` at `base` (0: no MMIO window, the tests) with interrupt `spi`.
/// `late_flush`: offer F_LATE_FLUSH on a writable disk (config blk-late-flush).
pub fn create(base: u64, spi: u32, disk: Arc<Blk>, late_flush: bool) -> Option<BlkDevice> {
    static COUNT: AtomicU32 = AtomicU32::new(0);
    let writable = disk.writable();
    let features =
        if writable { 0 } else { F_RO } | F_BLK_SIZE | F_FLUSH | if writable && late_flush { F_LATE_FLUSH } else { 0 };
    let info = Info { device_id: 2, features, nqueues: 1, manual: 0 };
    let late = Late {
        thread_up: false,
        tried: false,
        pending: false,
        head: 0,
        status: Viov::EMPTY,
        generation: 0,
        s: Stats::default(),
        window_start: 0,
    };
    let index = COUNT.fetch_add(1, Ordering::Relaxed);
    let device = BlkDev { disk, index, late: Mutex::new(late), wake: Condvar::new(), me: OnceLock::new() };
    match Virtio::create(base, spi, info, device, "virtio-blk") {
        Ok(dev) => {
            let _ = dev.device().me.set(Arc::downgrade(&dev));
            Some(dev)
        }
        Err(e) => {
            loge!("virtio-blk at {:#x}: {:?}", base, e);
            None
        }
    }
}

#[cfg(test)]
mod tests;
