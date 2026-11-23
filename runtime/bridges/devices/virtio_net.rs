// SPDX-License-Identifier: MIT
//! virtio-net (VirtIO 1.2, 5.1) for the guest's en0 (C: devices/virtio_net.c), as UniXNU-droidvm's DroidVMVirtioNet.c
//! drives it: VERSION_1 and MAC only, so every packet is one 12-byte virtio_net_hdr_v1 and one frame. Queue 0
//! receives (device to guest), queue 1 transmits. The frames go to a [`NetBackend`]: the tap of net/net.rs, the
//! phone's wlan0 (wifi::wlan), the stand-in radio (wifi::sim).

use crate::net::{NetBackend, NetConfig, NetOpen, FRAME_MAX};
use std::cell::RefCell;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;
use xh_core::virtio::{self, Device, Info, Viov, Virtio};

const NET_F_MAC: u64 = 1 << 5;
const NET_HDR: usize = 12;
/// A frame of the Wi-Fi station's own type (drivers/net/oneios_wifi_proto.h: scan records, the scan's end, a join's
/// result) is not traffic that may be dropped when the guest has no buffer free: it waits for one, up to this long.
/// A scan sends as many frames as the receive ring has buffers (64), and the one that was dropped was its end.
const CONTROL_TYPE: u16 = 0x88b5;
const CONTROL_WAIT_MS: u32 = 50;

/// The device's state.
pub struct NetDevice {
    mac: [u8; 6],
    backend: RefCell<Option<Box<dyn NetBackend>>>,
    pub rx_dropped: AtomicU64,
    pub tx_frames: AtomicU64,
    pub rx_frames: AtomicU64,
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
                let _ = b.send(&packet[NET_HDR..]);
            }
        }
        Some(0)
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

/// A frame for the guest (the backend's thread, or the backend's send answering DHCP on a vCPU).
fn to_guest(dev: &Virtio<NetDevice>, frame: &[u8]) {
    if frame.len() > FRAME_MAX {
        return;
    }
    let control = frame.len() >= 14 && u16::from_be_bytes([frame[12], frame[13]]) == CONTROL_TYPE;
    let mut iov = [Viov::EMPTY; 4];
    let mut waited = 0;
    let (guard, head, n) = loop {
        let guard = dev.lock();
        if let Some((head, n)) = dev.pop(0, &mut iov) {
            break (guard, head, n); // the lock stays for the copy below
        }
        drop(guard);
        if !control || waited >= CONTROL_WAIT_MS {
            dev.lock().rx_dropped.fetch_add(1, Ordering::Relaxed); // the guest has no buffer: as a full NIC ring
            return;
        }
        waited += 1;
        std::thread::sleep(Duration::from_millis(1));
    };
    let mut packet = vec![0u8; NET_HDR + frame.len()];
    packet[10] = 1; // num_buffers = 1
    packet[NET_HDR..].copy_from_slice(frame);
    let mut done = virtio::write_chain(&iov[..n], 0, &packet);
    if done == packet.len() {
        guard.rx_frames.fetch_add(1, Ordering::Relaxed);
    } else {
        guard.rx_dropped.fetch_add(1, Ordering::Relaxed);
        done = 0;
    }
    dev.push(0, head, done as u32);
}

/// "droidvm,net": the device at `base` with its SPI. `open` makes the backend with the device's receive function
/// (`Net::tap_opener()` on the phone; None: the link is up and silent). None when the device cannot be made.
pub fn create(base: u64, spi: u32, config: &NetConfig, open: Option<NetOpen>) -> Option<Arc<Virtio<NetDevice>>> {
    let info = Info { device_id: 1, features: NET_F_MAC, nqueues: 2, manual: 1 };
    let device = NetDevice {
        mac: config.guest_mac,
        backend: RefCell::new(None),
        rx_dropped: AtomicU64::new(0),
        tx_frames: AtomicU64::new(0),
        rx_frames: AtomicU64::new(0),
    };
    let dev = Virtio::create(base, spi, info, device, "virtio-net").ok()?;
    if let Some(open) = open {
        let weak: Weak<Virtio<NetDevice>> = Arc::downgrade(&dev);
        let rx = Arc::new(move |frame: &[u8]| {
            if let Some(dev) = weak.upgrade() {
                to_guest(&dev, frame);
            }
        });
        let backend = open(config, rx);
        *dev.lock().backend.borrow_mut() = backend;
    }
    Some(dev)
}
