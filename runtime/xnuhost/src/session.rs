// SPDX-License-Identifier: MIT
//! Port of main.c: the session (docs/booter.md, oneos/interface.md): the machine's assembly from the config, the main
//! loop and its timers, the signals, the stop and the return.
//!
//! What the other modules call: [`stop`], [`stopping`], [`mono_ms`] (any thread), [`guest_ready`] (the kernel's
//! ready host call) and [`guest_frame`] (virtio-gpu, a frame of the guest's own).

use std::fs::OpenOptions;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

use xh_core::{loge, logi};

use crate::config::{Config, READY_CONTENT_WAIT};
use crate::ready::{ReadyState, ReadyWhy};

/// The exit codes (docs/booter.md, oneos/interface.md 5).
pub const EXIT_GUEST_OFF: i32 = 0;
pub const EXIT_SETUP: i32 = 2;
pub const EXIT_RESTART: i32 = 3;
pub const EXIT_PANIC: i32 = 4;

/// The finished config of the running session (set once, before the machine starts).
static CONFIG: OnceLock<Config> = OnceLock::new();

/// The session's config; None before the session set it (the tests, a tool).
pub fn config() -> Option<&'static Config> {
    CONFIG.get()
}

// ---- the stop ------------------------------------------------------------------------------------------------------

/// Why the machine stops: the first request wins.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StopRequest {
    pub reason: u32,
    pub code: u32,
    pub why: String,
}

static STOP_REQUESTED: AtomicBool = AtomicBool::new(false);
static STOP: Mutex<Option<StopRequest>> = Mutex::new(None);

/// xh_stop: asks the machine to stop (any thread; returns). Only the first request is kept.
pub fn stop(reason: u32, code: u32, why: &str) {
    let mut s = STOP.lock().unwrap_or_else(|e| e.into_inner());
    if s.is_none() {
        // what C kept of it: 95 bytes
        let mut why = why.to_owned();
        if why.len() > 95 {
            let mut end = 95;
            while !why.is_char_boundary(end) {
                end -= 1;
            }
            why.truncate(end);
        }
        *s = Some(StopRequest { reason, code, why });
        STOP_REQUESTED.store(true, Ordering::Release);
    }
}

/// xh_stopping: a stop was asked.
pub fn stopping() -> bool {
    STOP_REQUESTED.load(Ordering::Acquire)
}

/// The stop that was asked, if any.
pub fn stop_request() -> Option<StopRequest> {
    STOP.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// xh_mono_ms: CLOCK_MONOTONIC in milliseconds.
pub fn mono_ms() -> u64 {
    let mut t = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    // SAFETY: clock_gettime writes the timespec it is given; CLOCK_MONOTONIC always exists on Linux.
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut t) };
    t.tv_sec as u64 * 1000 + t.tv_nsec as u64 / 1_000_000
}

// ---- the session files (oneos/interface.md 3) ----------------------------------------------------------------------

/// Writes `<session>/<name>` (0600, replaced whole); nothing without a session folder.
fn session_file(name: &str, content: &str) {
    let Some(dir) = config().map(|c| c.session.as_str()).filter(|d| !d.is_empty()) else { return };
    let path = format!("{dir}/{name}");
    if path.len() >= 600 {
        return;
    }
    let file = OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&path);
    match file {
        Ok(mut f) => {
            let _ = f.write_all(content.as_bytes());
        }
        Err(e) => loge!("session: {}: {}", path, xh_core::sys::error_text(&e)),
    }
}

// ---- the guest is up -----------------------------------------------------------------------------------------------
// session/ready (the switch's dead-man timer stops; the OneOS boot screen hands over to the display helper) and the
// `guest ready: first frame` line, which the fork's boot stages and oneios-tool look for. When, is ready.rs's rule: the
// first frame of the guest's own with content, or ready-content-wait seconds after its first one. The device's thread
// (guest_frame), the main loop's tick and the ready host call all come through here.

static READY: Mutex<ReadyState> = Mutex::new(ReadyState { first_ms: 0, frames: 0, ready: false });
static GUEST_READY: AtomicBool = AtomicBool::new(false);
/// a frame of the guest's own has been shown: the console's copy stops
static GUEST_DRAWING: AtomicBool = AtomicBool::new(false);

fn ready_content_wait() -> u32 {
    config().map_or(READY_CONTENT_WAIT, |c| c.ready_content_wait)
}

/// Whether the guest is up (session/ready written).
pub fn is_guest_ready() -> bool {
    GUEST_READY.load(Ordering::Acquire)
}

/// Whether a frame of the guest's own was shown.
pub fn guest_drawing() -> bool {
    GUEST_DRAWING.load(Ordering::Acquire)
}

fn ready_now(rule: &ReadyState, why: ReadyWhy, now: u64) {
    if !why.now() || GUEST_READY.swap(true, Ordering::AcqRel) {
        return;
    }
    session_file("ready", "ready\n");
    let after = now.saturating_sub(rule.first_ms) as f64 / 1000.0;
    let frames = rule.frames;
    match why {
        ReadyWhy::Content if frames <= 1 => logi!("guest ready: first frame (it has content)"),
        ReadyWhy::Content => logi!(
            "guest ready: first frame with content, {:.1} s and {} frames after the guest's first (black) frame",
            after,
            frames - 1
        ),
        ReadyWhy::BlackTimeout => logi!(
            "guest ready: first frame, all black: no frame with content in {} s after the guest's first ({} frames); ready all the same (ready-content-wait)",
            ready_content_wait(),
            frames
        ),
        ReadyWhy::AnyFrame => logi!("guest ready: first frame (ready-content-wait=0: content not waited for)"),
        _ => logi!("guest ready: first frame (the kernel's ready call)"),
    }
}

/// xh_guest_ready: the kernel's ready host call.
pub fn guest_ready() {
    let mut rule = READY.lock().unwrap_or_else(|e| e.into_inner());
    let why = rule.on_call();
    ready_now(&rule, why, mono_ms());
}

/// xh_guest_frame: a frame of the guest's own was shown (virtio_gpu calls it before the frame goes to the display
/// helper, so that session/ready is there first); `lit`: pixels_lit of the display's frame. True once the guest is
/// ready, after which it need not be called again.
pub fn guest_frame(lit: bool) -> bool {
    let now = mono_ms();
    let wait = ready_content_wait();
    let mut rule = READY.lock().unwrap_or_else(|e| e.into_inner());
    let first = rule.frames == 0 && !rule.ready;
    let why = rule.on_frame(lit, wait, now);
    if first {
        GUEST_DRAWING.store(true, Ordering::Release);
        // The marker of the rule before 2026-11-15 (ready at this frame), kept for the timings.
        if !lit && why == ReadyWhy::Not {
            logi!(
                "display: the guest's first frame of its own: all black; session/ready waits for one with content (up to {} s)",
                wait
            );
        }
    }
    ready_now(&rule, why, now);
    rule.ready
}

/// The main loop's tick of the ready rule.
pub fn ready_tick() {
    if is_guest_ready() {
        return;
    }
    let now = mono_ms();
    let mut rule = READY.lock().unwrap_or_else(|e| e.into_inner());
    let why = rule.on_tick(ready_content_wait(), now);
    ready_now(&rule, why, now);
}
