// SPDX-License-Identifier: MIT
//! A helper that hangs in its open (test_encoder.c's test_hung_helper; the phone, 2026-11-23: MediaCodec's open never
//! came back, the vCPU that asked waited for ever with the device held, and a stackshot panicked the guest): the open
//! comes back within its limit with no session, the helper is killed, the next open starts a new one that works, and
//! after two hangs no helper is started and every open fails at once. A stop is not a hang. The stand-in's open hangs
//! with ONEIOS_ENCODER_STUB_HANG_OPEN=1 in the helper's environment, which the helper inherits from this process: a
//! test binary of its own, so no other test's stand-in or helper sees it.
use oneencoder::backend::{Backend, Session};
use oneencoder::backend_helper::{self, HelperBackend};
use oneencoder::proto::MEDIA_PIXFMT_HEVC;
use std::time::{Duration, Instant};

const HANG: &str = "ONEIOS_ENCODER_STUB_HANG_OPEN";

fn ms_since(t: Instant) -> u64 {
    t.elapsed().as_millis() as u64
}

/// Opens until one call takes 250 ms or more (the hang) or one succeeds, for 5 s at most; that call's time (ms), or
/// None (a session when a hang was wanted, or neither came), and the session.
fn open_until(b: &HelperBackend, want_hang: bool) -> (Option<u64>, Option<Box<dyn Session>>) {
    let began = Instant::now();
    while ms_since(began) < 5000 {
        let t = Instant::now();
        let s = b.open(MEDIA_PIXFMT_HEVC, 64, 64);
        let took = ms_since(t);
        if s.is_some() {
            return (if want_hang { None } else { Some(took) }, s);
        }
        if want_hang && took >= 250 {
            return (Some(took), None);
        }
        // A killed helper is not gone yet: the next try, a moment later.
        std::thread::sleep(Duration::from_millis(2));
    }
    (None, None)
}

#[test]
fn hung_helper() {
    let log = xh_core::log::Capture::start();
    let b = HelperBackend::with_open_ms(env!("CARGO_BIN_EXE_oneios-encoder"), 300);
    // A helper that works (run()'s, in the C), then the stop: it goes, and a stop is not a hang.
    std::env::remove_var(HANG);
    let (took, s) = open_until(&b, false);
    assert!(took.is_some() && s.is_some(), "the first helper");
    drop(s);
    backend_helper::stop();

    std::env::set_var(HANG, "1");
    let (took, s) = open_until(&b, true);
    assert!(took.is_some_and(|t| (250..1500).contains(&t)) && s.is_none(), "bounded: the vCPU is back ({took:?})");
    assert!(
        log.has("did not answer an open in 300 ms (state ")
            && log.has("): killed (1 of 2); the next open starts a new one")
    );
    std::env::remove_var(HANG);
    let (took, s) = open_until(&b, false); // a new helper, after the killed one ended
    assert!(took.is_some_and(|t| t < 1000) && s.is_some(), "a new helper ({took:?})");
    drop(s);

    std::env::set_var(HANG, "1");
    b.stop(); // this backend's own stop, the same
    let (took, s) = open_until(&b, true); // the second hang
    assert!(took.is_some_and(|t| (250..1500).contains(&t)) && s.is_none(), "the second hang ({took:?})");
    assert!(log.has("): killed (2 of 2); the next open starts a new one"));
    std::env::remove_var(HANG);

    // Two hangs: no more helpers, every open fails at once.
    let began = Instant::now();
    let (mut any, mut slow) = (false, false);
    while ms_since(began) < 500 {
        let t = Instant::now();
        any |= b.open(MEDIA_PIXFMT_HEVC, 64, 64).is_some();
        slow |= ms_since(t) > 100;
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(!any && !slow, "no more helpers (any {any}, slow {slow})");
    let off =
        "encoder: the helper hung 2 times: no more helpers this session (VideoToolbox keeps its software encoder)";
    assert_eq!(log.texts().iter().filter(|l| l.contains(off)).count(), 1, "said once: {:?}", log.texts());
    assert_eq!(
        log.texts().iter().filter(|l| l.contains("killed (")).count(),
        2,
        "a stop is not a hang: {:?}",
        log.texts()
    );
    b.stop();
}
