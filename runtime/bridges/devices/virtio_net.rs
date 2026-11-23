// SPDX-License-Identifier: MIT
//! virtio-net (VirtIO 1.2, 5.1) for the guest's en0 (C: devices/virtio_net.c), as UniXNU-droidvm's DroidVMVirtioNet.c
//! drives it: VERSION_1 and MAC only, so every packet is one 12-byte virtio_net_hdr_v1 and one frame. Queue 0
//! receives (device to guest), queue 1 transmits. The frames go to a [`NetBackend`]: the tap of net/net.rs, the
//! phone's wlan0 (wifi::wlan), the stand-in radio (wifi::sim).
//!
//! The receive ring has 64 buffers and the guest's worker gives them back in a batch (its kick of queue 0). A frame
//! that finds the ring full waits for that kick, up to [`DATA_WAIT_MS`] (a frame of the Wi-Fi station's own type up to
//! [`CONTROL_WAIT_MS`]), on a condition variable: the backend's receive thread blocks and the host socket's buffer
//! holds what comes meanwhile, as a NIC's ring and its host's queue would. Until 2026-11-23 a data frame was dropped
//! at once (a TCP burst from Wi-Fi while iOS's CPUs were busy: losses, retransmits, pages slow), and a control frame
//! polled for a buffer every millisecond. A frame sent on the guest's own transmit (a DHCP answer of the tap backend,
//! on the vCPU) never waits: the vCPU is what would post the buffers.

use crate::net::{NetBackend, NetConfig, NetOpen, FRAME_MAX};
use std::cell::{Cell, RefCell};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError, Weak};
use std::time::{Duration, Instant};
use xh_core::virtio::{self, Device, Info, Viov, Virtio};

const NET_F_MAC: u64 = 1 << 5;
const NET_HDR: usize = 12;
/// A frame of the Wi-Fi station's own type (drivers/net/oneios_wifi_proto.h: scan records, the scan's end, a join's
/// result) is not traffic that may be dropped when the guest has no buffer free: it waits for one, up to this long.
/// A scan sends as many frames as the receive ring has buffers (64), and the one that was dropped was its end.
const CONTROL_TYPE: u16 = 0x88b5;
pub const CONTROL_WAIT_MS: u64 = 50;
/// How long a data frame waits for the guest to give buffers back before it is dropped.
pub const DATA_WAIT_MS: u64 = 20;

/// The guest's kicks of the receive queue: a count that a waiting frame watches.
#[derive(Default)]
struct Posted {
    kicks: Mutex<u64>,
    more: Condvar,
}

/// The receive path's counts (read by the backend's report: wifi::wlan's 60 s line).
#[derive(Default)]
pub struct RxCounts {
    /// frames the guest got
    pub frames: AtomicU64,
    /// frames dropped: the ring stayed full for the whole wait (or the frame did not fit the buffer)
    pub dropped: AtomicU64,
    /// frames that found the ring full and waited (the ones that got a buffer and the ones dropped)
    pub waited: AtomicU64,
    /// the longest wait that ended with a buffer, in microseconds (since the last [`RxCounts::take_longest`])
    pub longest_us: AtomicU64,
}

impl RxCounts {
    pub fn take_longest(&self) -> u64 {
        self.longest_us.swap(0, Ordering::Relaxed)
    }
}

thread_local! {
    /// Set while the device hands a guest's frame to the backend (on the vCPU): a frame for the guest made there
    /// does not wait for buffers.
    static IN_SEND: Cell<bool> = const { Cell::new(false) };
}

/// The device's state.
pub struct NetDevice {
    mac: [u8; 6],
    backend: RefCell<Option<Box<dyn NetBackend>>>,
    posted: Arc<Posted>,
    pub rx: Arc<RxCounts>,
    pub tx_frames: AtomicU64,
}

impl Device for NetDevice {
    fn config_read(&self, _dev: &Virtio<Self>, offset: u32, size: u32) -> u32 {
        let mut config = [0u8; 12]; // mac[6], status (link up), max_virtqueue_pairs
        config[..6].copy_from_slice(&self.mac);
        config[6] = 1;
        config_word(&config, offset, size)
    }

    fn handle(&self, _dev: &Virtio<Self>, queue: u32, _head: u16, iov: &[Viov]) -> Option<u32> {
        if queue != 1 {
            return Some(0);
        }
        let size = virtio::readable_len(iov);
        if size > NET_HDR + FRAME_MAX {
            return Some(0); // larger than a frame: dropped
        }
        let mut packet = vec![0u8; size];
        virtio::read_chain(iov, 0, &mut packet);
        if size > NET_HDR {
            if let Some(b) = self.backend.borrow().as_ref() {
                self.tx_frames.fetch_add(1, Ordering::Relaxed);
                IN_SEND.with(|s| s.set(true));
                let _ = b.send(&packet[NET_HDR..]);
                IN_SEND.with(|s| s.set(false));
            }
        }
        Some(0)
    }

    /// The guest gave receive buffers back (queue 0 is the device's own: `manual`).
    fn kick(&self, _dev: &Virtio<Self>, queue: u32) {
        if queue == 0 {
            *self.posted.kicks.lock().unwrap_or_else(PoisonError::into_inner) += 1;
            self.posted.more.notify_all();
        }
    }
}

/// A configuration-space word: `size` bytes (at most 4) at `offset` of `config`, 0 past its end.
pub(crate) fn config_word(config: &[u8], offset: u32, size: u32) -> u32 {
    let (offset, size) = (offset as usize, size as usize);
    if offset >= config.len() || size > config.len() - offset {
        return 0;
    }
    let mut v = [0u8; 4];
    let n = size.min(4);
    v[..n].copy_from_slice(&config[offset..offset + n]);
    u32::from_le_bytes(v)
}

/// A frame for the guest (the backend's thread, or the backend's send answering DHCP on a vCPU). True when the guest
/// got it.
fn to_guest(dev: &Virtio<NetDevice>, posted: &Posted, rx: &RxCounts, frame: &[u8]) -> bool {
    if frame.len() > FRAME_MAX {
        rx.dropped.fetch_add(1, Ordering::Relaxed);
        return false;
    }
    let control = frame.len() >= 14 && u16::from_be_bytes([frame[12], frame[13]]) == CONTROL_TYPE;
    let wait = if IN_SEND.with(Cell::get) {
        Duration::ZERO
    } else {
        Duration::from_millis(if control { CONTROL_WAIT_MS } else { DATA_WAIT_MS })
    };
    let mut iov = [Viov::EMPTY; 4];
    let mut started: Option<Instant> = None;
    let (guard, head, n) = loop {
        // The count before the look: a kick between the look and the wait ends the wait at once.
        let kicks = *posted.kicks.lock().unwrap_or_else(PoisonError::into_inner);
        let guard = dev.lock();
        if let Some((head, n)) = dev.pop(0, &mut iov) {
            break (guard, head, n); // the lock stays for the copy below
        }
        drop(guard);
        let t0 = *started.get_or_insert_with(|| {
            rx.waited.fetch_add(1, Ordering::Relaxed);
            Instant::now()
        });
        let left = wait.saturating_sub(t0.elapsed());
        if left.is_zero() {
            rx.dropped.fetch_add(1, Ordering::Relaxed); // the guest gave no buffer back in time: as a full NIC ring
            return false;
        }
        let k = posted.kicks.lock().unwrap_or_else(PoisonError::into_inner);
        let _ = posted.more.wait_timeout_while(k, left, |k| *k == kicks).unwrap_or_else(PoisonError::into_inner);
    };
    if let Some(t0) = started {
        rx.longest_us.fetch_max(t0.elapsed().as_micros() as u64, Ordering::Relaxed);
    }
    let mut packet = vec![0u8; NET_HDR + frame.len()];
    packet[10] = 1; // num_buffers = 1
    packet[NET_HDR..].copy_from_slice(frame);
    let mut done = virtio::write_chain(&iov[..n], 0, &packet);
    let ok = done == packet.len();
    if ok {
        rx.frames.fetch_add(1, Ordering::Relaxed);
    } else {
        rx.dropped.fetch_add(1, Ordering::Relaxed);
        done = 0;
    }
    dev.push(0, head, done as u32);
    drop(guard);
    ok
}

/// "droidvm,net": the device at `base` with its SPI. `open` makes the backend with the device's receive function
/// (`Net::tap_opener()` on the phone; None: the link is up and silent). None when the device cannot be made.
pub fn create(base: u64, spi: u32, config: &NetConfig, open: Option<NetOpen>) -> Option<Arc<Virtio<NetDevice>>> {
    let info = Info { device_id: 1, features: NET_F_MAC, nqueues: 2, manual: 1 };
    let posted = Arc::new(Posted::default());
    let counts = Arc::new(RxCounts::default());
    let device = NetDevice {
        mac: config.guest_mac,
        backend: RefCell::new(None),
        posted: Arc::clone(&posted),
        rx: Arc::clone(&counts),
        tx_frames: AtomicU64::new(0),
    };
    let dev = Virtio::create(base, spi, info, device, "virtio-net").ok()?;
    *LAST.lock().unwrap_or_else(PoisonError::into_inner) = Arc::downgrade(&counts);
    if let Some(open) = open {
        let weak: Weak<Virtio<NetDevice>> = Arc::downgrade(&dev);
        let rx =
            Arc::new(move |frame: &[u8]| weak.upgrade().is_some_and(|dev| to_guest(&dev, &posted, &counts, frame)));
        let backend = open(config, rx);
        *dev.lock().backend.borrow_mut() = backend;
    }
    Some(dev)
}

/// The receive ring of the last device made (there is one en0): the frames that found it full and waited, and the
/// longest wait that got a buffer (microseconds), both since the last call (the Wi-Fi bridge's 60 s line).
pub fn take_ring_waits() -> (u64, u64) {
    let counts = LAST.lock().unwrap_or_else(PoisonError::into_inner).upgrade();
    counts.map_or((0, 0), |c| (c.waited.swap(0, Ordering::Relaxed), c.take_longest()))
}

static LAST: Mutex<Weak<RxCounts>> = Mutex::new(Weak::new());
