// SPDX-License-Identifier: MIT
//! The network bridge's host tests (C: tests/test_bridges.c, test_net): the DHCP answer, the fd backend on a
//! socketpair.

use super::*;
use std::os::fd::{AsRawFd, OwnedFd};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;

/// A DHCP message of `kind` from the guest (02:d7:01:02:03:04), 342 bytes.
pub(crate) fn make_dhcp(kind: u8, xid: u32) -> Vec<u8> {
    let mut frame = vec![0u8; 342];
    frame[..6].fill(0xff);
    frame[6..12].copy_from_slice(&[0x02, 0xd7, 0x01, 0x02, 0x03, 0x04]);
    frame[12] = 0x08;
    let (ip, udp, b) = (14, 34, 42);
    frame[ip] = 0x45;
    frame[ip + 9] = 17;
    frame[ip + 16..ip + 20].fill(0xff);
    frame[udp + 1] = 68;
    frame[udp + 3] = 67;
    frame[b..b + 3].copy_from_slice(&[1, 1, 6]);
    frame[b + 4..b + 8].copy_from_slice(&xid.to_ne_bytes());
    let mac: [u8; 6] = frame[6..12].try_into().unwrap();
    frame[b + 28..b + 34].copy_from_slice(&mac);
    frame[b + 236..b + 240].copy_from_slice(&[0x63, 0x82, 0x53, 0x63]);
    frame[b + 240..b + 248].copy_from_slice(&[53, 1, kind, 55, 2, 1, 3, 255]);
    frame
}

/// The value of option `want` in a reply.
pub(crate) fn dhcp_option(reply: &[u8], want: u8) -> Option<&[u8]> {
    let b = &reply[42..];
    let mut i = 240;
    while i + 2 <= b.len() {
        match b[i] {
            255 => break,
            0 => i += 1,
            o if o == want => return Some(&b[i + 2..i + 2 + usize::from(b[i + 1])]),
            _ => i += 2 + usize::from(b[i + 1]),
        }
    }
    None
}

fn socketpair(kind: i32) -> (OwnedFd, OwnedFd) {
    xh_core::fd::socket_pair(kind | libc::SOCK_CLOEXEC).unwrap()
}

#[test]
fn net() {
    let c = NetConfig::default();
    let reply = dhcp_reply(&c, &make_dhcp(1, 0x11223344)).unwrap();
    assert!(reply.len() >= 342);
    assert!(reply[42] == 2 && reply[46..50] == [0x44, 0x33, 0x22, 0x11]); // BOOTREPLY, the request's xid
    assert_eq!(reply[58..62], [0xc0, 0xa8, 0x4d, 0x02]); // yiaddr 192.168.77.2
    assert_eq!(reply[70..76], [0x02, 0xd7, 0x01, 0x02, 0x03, 0x04]);
    assert_eq!(dhcp_option(&reply, 53), Some(&[2u8][..])); // OFFER
    assert_eq!(dhcp_option(&reply, 3), Some(&[0xc0, 0xa8, 0x4d, 0x01][..]));
    assert_eq!(dhcp_option(&reply, 6), Some(&[8, 8, 8, 8, 1, 1, 1, 1][..]));
    assert_eq!(dhcp_option(&reply, 1), Some(&[0xff, 0xff, 0xff, 0x00][..]));
    // The IPv4 header checksum verifies (sums to 0xffff).
    let mut sum: u32 = reply[14..34].chunks(2).map(|w| u32::from(w[0]) << 8 | u32::from(w[1])).sum();
    while sum >> 16 != 0 {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    assert_eq!(sum, 0xffff);
    let reply = dhcp_reply(&c, &make_dhcp(3, 7)).unwrap();
    assert_eq!(dhcp_option(&reply, 53), Some(&[5u8][..])); // REQUEST -> ACK
    assert!(dhcp_reply(&c, &make_dhcp(7, 7)).is_none()); // RELEASE: no answer
    let mut frame = make_dhcp(1, 7);
    frame[14 + 20 + 3] = 53; // not the DHCP server port
    assert!(dhcp_reply(&c, &frame).is_none());
    assert!(dhcp_reply(&c, &frame[..60]).is_none());
    let mut frame = make_dhcp(1, 7);
    frame[42 + 241] = 200; // an option running past the frame
    assert!(dhcp_reply(&c, &frame).is_none());

    // The fd back end: frames both ways, DHCP answered without reaching the wire.
    let (mine, wire) = socketpair(libc::SOCK_DGRAM);
    let (tx, got) = mpsc::channel::<Vec<u8>>();
    let tx = std::sync::Mutex::new(tx);
    let net = Net::open_fd(&c, mine, Arc::new(move |f: &[u8]| tx.lock().unwrap().send(f.to_vec()).is_ok())).unwrap();
    let mut ping = [0u8; 64];
    ping[..14].copy_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 0x08, 0x06]);
    assert_eq!(net.send(&ping), Ok(()));
    let mut buf = [0u8; 600];
    // SAFETY: a valid buffer of its length.
    let n = unsafe { libc::recv(wire.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len(), 0) };
    assert!(n == 64 && buf[..64] == ping);
    // SAFETY: a valid buffer of 60 bytes.
    assert_eq!(unsafe { libc::send(wire.as_raw_fd(), ping.as_ptr().cast(), 60, 0) }, 60);
    assert_eq!(got.recv_timeout(Duration::from_secs(2)).unwrap().len(), 60);
    assert_eq!(net.send(&make_dhcp(1, 9)), Ok(()));
    assert_eq!(got.try_recv().unwrap()[42], 2); // the offer came back to the guest
                                                // SAFETY: a valid buffer of its length.
    let n = unsafe { libc::recv(wire.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len(), libc::MSG_DONTWAIT) };
    assert!(n < 0); // and nothing went to the wire
    assert!(net.send(&ping[..10]) == Err(libc::EINVAL) && net.send(&[0u8; 2000]) == Err(libc::EINVAL));
    drop(net);
}
