// SPDX-License-Identifier: MIT
//! oneios-encoder: the video encoder helper process (bin/oneios-encoder in the runtime package; helper.c). MediaCodec
//! is a shared library, so it cannot run inside the static xnuhost; this helper does, exactly as oneios-camera and
//! oneios-audio do. It reads the device's messages ([`crate::link`]) on the inherited socket (fd given as argv[1],
//! default 3), runs the real encoder (backend_ndk on Android; the stand-in on a host, for the test, or on Android with
//! ONEIOS_ENCODER_STUB set), and streams the coded access units back. One session per virtio-media session; the
//! session's frames are drained after each feed and after a drain.
use crate::backend::{Backend, Polled, Session};
use crate::backend_stub::StubBackend;
use crate::link::*;
use std::fs::File;
use std::io::Read;
use std::os::fd::FromRawFd;

const HELPER_SESSIONS: usize = 8;
const CODED_CAP: usize = 8 << 20;

fn backend() -> Box<dyn Backend> {
    #[cfg(target_os = "android")]
    if std::env::var_os("ONEIOS_ENCODER_STUB").is_none() {
        return Box::new(crate::backend_ndk::NdkBackend);
    }
    Box::new(StubBackend) // the host build: no MediaCodec on the box
}

fn send_coded(link: &mut File, id: u32, p: &Polled, coded: &[u8]) {
    let flags = u32::from(p.key) | u32::from(p.eos) << 1;
    let _ = EncMsg { pts: p.pts_us, a: flags, ..EncMsg::new(ENC_MSG_CODED, id) }.send(link, &coded[..p.bytes]);
}

/// After a feed: every coded unit the session has ready (an error, or nothing more, ends it; so does an eos).
fn after_feed(link: &mut File, id: u32, enc: &mut dyn Session, coded: &mut [u8]) {
    while let Ok(p) = enc.poll(coded) {
        if p.bytes == 0 && !p.eos {
            break;
        }
        if p.bytes > 0 {
            send_coded(link, id, &p, coded);
        }
        if p.eos {
            break;
        }
    }
}

/// After a drain: every coded unit, then the EOS message once the drain's end came, or after about 2 s of nothing
/// (the codec's flush is waited for that long at most).
fn after_drain(link: &mut File, id: u32, enc: &mut dyn Session, coded: &mut [u8]) {
    let mut waited = 0;
    while waited < 1000 {
        let p = enc.poll(coded).unwrap_or_default();
        if p.bytes > 0 {
            send_coded(link, id, &p, coded);
            continue;
        }
        if p.eos {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
        waited += 1;
    }
    let _ = EncMsg::new(ENC_MSG_EOS, id).send(link, &[]);
}

/// The helper's main: its exit code.
pub fn main() -> i32 {
    let fd = std::env::args().nth(1).map_or(3, |a| a.trim().parse().unwrap_or(0));
    // SAFETY: the socket xnuhost gave this process; it is ours for the process's life.
    let mut link = unsafe { File::from_raw_fd(fd) };
    let backend = backend();
    let mut sessions: Vec<(u32, Box<dyn Session>)> = Vec::with_capacity(HELPER_SESSIONS);
    let mut coded = vec![0u8; CODED_CAP];
    let mut payload = Vec::new();
    let hello = EncMsg { a: ENC_LINK_VERSION, ..EncMsg::new(ENC_MSG_HELLO, 0) };
    if hello.send(&mut link, &[]).is_err() {
        return 1;
    }
    while let Ok(m) = EncMsg::receive(&mut link) {
        payload.resize(m.len as usize, 0);
        if link.read_exact(&mut payload).is_err() {
            break;
        }
        let slot = sessions.iter().position(|(id, _)| *id == m.session);
        match (m.type_, slot) {
            (ENC_MSG_OPEN, _) => {
                let enc = (sessions.len() < HELPER_SESSIONS).then(|| backend.open(m.a, m.b, m.c)).flatten();
                let ok = enc.is_some();
                if let Some(enc) = enc {
                    sessions.push((m.session, enc));
                }
                let _ = EncMsg { a: u32::from(ok), ..EncMsg::new(ENC_MSG_OPEN_REPLY, m.session) }.send(&mut link, &[]);
            }
            (ENC_MSG_FEED, Some(i)) => {
                let enc = &mut sessions[i].1;
                let _ = enc.feed(&payload, m.pts, m.a & 1 != 0);
                after_feed(&mut link, m.session, enc.as_mut(), &mut coded); // whatever came out
            }
            (ENC_MSG_DRAIN, Some(i)) => {
                let enc = &mut sessions[i].1;
                let _ = enc.drain();
                after_drain(&mut link, m.session, enc.as_mut(), &mut coded);
            }
            (ENC_MSG_CLOSE, Some(i)) => drop(sessions.swap_remove(i)),
            _ => {}
        }
    }
    0
}
