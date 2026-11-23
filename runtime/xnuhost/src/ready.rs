// SPDX-License-Identifier: MIT
//! Port of ready.c: when the guest is up, the rule behind session/ready and the `guest ready: first frame` line
//! (docs/booter.md 2.8). Pure and unit-tested; session.rs holds the state under a lock and feeds it the frames the
//! virtio-gpu device shows and its 100 ms tick.
//!
//! The guest's first frame of its own (not the boot console's: virtio_gpu's frame_shown leaves those out) is
//! backboardd's, and it is black: on the phone (main 3233fed, 2026-11-15) ready came at +133 s at that frame, the
//! OneOS boot screen ended there, and the panel stayed black until iOS drew its Apple logo. So the guest is ready at
//! the first frame of its own that shows something (pixels_lit). An iOS that only ever draws black is still ready
//! `ready-content-wait` seconds after its first frame (0: at the first frame, the rule before 2026-11-15), so the
//! switch's dead-man timer and the boot screen do not wait for ever on it.

/// What a frame, the clock or the ready call made of the rule (xh_ready_why).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadyWhy {
    /// not yet
    Not,
    /// it was before
    Already,
    /// now: a frame of the guest's own with content
    Content,
    /// now: the first frame of its own (ready-content-wait=0, the rule before 2026-11-15)
    AnyFrame,
    /// now: ready-content-wait seconds after its first frame of its own, all black
    BlackTimeout,
    /// now: the kernel's ready host call
    Call,
}

impl ReadyWhy {
    /// Whether the guest became ready now.
    pub fn now(self) -> bool {
        !matches!(self, ReadyWhy::Not | ReadyWhy::Already)
    }
}

/// The rule's state (xh_ready_state).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ReadyState {
    /// the clock at the guest's first frame of its own
    pub first_ms: u64,
    /// frames of its own seen
    pub frames: u64,
    pub ready: bool,
}

impl ReadyState {
    pub fn new() -> Self {
        Self::default()
    }

    /// A frame of the guest's own; `lit`: it shows something. `wait_s`: ready-content-wait.
    pub fn on_frame(&mut self, lit: bool, wait_s: u32, now_ms: u64) -> ReadyWhy {
        if self.ready {
            return ReadyWhy::Already;
        }
        if self.frames == 0 {
            self.first_ms = now_ms;
        }
        self.frames += 1;
        if lit {
            self.ready = true;
            return ReadyWhy::Content;
        }
        if wait_s == 0 {
            self.ready = true;
            return ReadyWhy::AnyFrame;
        }
        self.on_tick(wait_s, now_ms)
    }

    /// The clock: ready once ready-content-wait seconds have passed since the first frame (never before a frame, nor
    /// with a clock behind it).
    pub fn on_tick(&mut self, wait_s: u32, now_ms: u64) -> ReadyWhy {
        if self.ready {
            return ReadyWhy::Already;
        }
        if self.frames == 0 || wait_s == 0 || now_ms < self.first_ms || now_ms - self.first_ms < wait_s as u64 * 1000 {
            return ReadyWhy::Not;
        }
        self.ready = true;
        ReadyWhy::BlackTimeout
    }

    /// The kernel's ready host call.
    pub fn on_call(&mut self) -> ReadyWhy {
        if self.ready {
            return ReadyWhy::Already;
        }
        self.ready = true;
        ReadyWhy::Call
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, READY_CONTENT_WAIT};

    #[test]
    fn ready_rule() {
        let mut st = ReadyState::new();
        assert_eq!(st.on_tick(120, 1_000_000), ReadyWhy::Not); // no frame yet: never by the clock
        assert_eq!(st.on_frame(false, 120, 133_000), ReadyWhy::Not);
        assert!(st.first_ms == 133_000 && st.frames == 1);
        assert_eq!(st.on_frame(false, 120, 140_000), ReadyWhy::Not);
        assert_eq!(st.on_tick(120, 252_999), ReadyWhy::Not);
        assert_eq!(st.on_frame(true, 120, 150_000), ReadyWhy::Content);
        assert!(st.ready && st.frames == 3);
        assert_eq!(st.on_frame(true, 120, 151_000), ReadyWhy::Already);
        assert_eq!(st.on_tick(120, 999_999), ReadyWhy::Already);
        assert_eq!(st.on_call(), ReadyWhy::Already);
        // The first frame lit: ready at once.
        let mut st = ReadyState::new();
        assert_eq!(st.on_frame(true, 120, 5000), ReadyWhy::Content);
        assert_eq!(st.frames, 1);
        // All black: the tick ends the wait at 120 s after the first frame, or a black frame that late does.
        let mut st = ReadyState::new();
        assert_eq!(st.on_frame(false, 120, 133_000), ReadyWhy::Not);
        assert_eq!(st.on_tick(120, 253_000), ReadyWhy::BlackTimeout);
        assert!(st.ready);
        assert_eq!(st.on_tick(120, 253_100), ReadyWhy::Already);
        let mut st = ReadyState::new();
        assert_eq!(st.on_frame(false, 120, 133_000), ReadyWhy::Not);
        assert_eq!(st.on_frame(false, 120, 253_000), ReadyWhy::BlackTimeout);
        // A clock behind the first frame (not expected) does not end it.
        let mut st = ReadyState::new();
        assert_eq!(st.on_frame(false, 120, 133_000), ReadyWhy::Not);
        assert_eq!(st.on_tick(120, 1000), ReadyWhy::Not);
        // ready-content-wait=0: the first frame of its own, black or not (the rule before 2026-11-15).
        let mut st = ReadyState::new();
        assert_eq!(st.on_frame(false, 0, 133_000), ReadyWhy::AnyFrame);
        assert!(st.ready);
        let mut st = ReadyState::new();
        assert_eq!(st.on_tick(0, 133_000), ReadyWhy::Not);
        // The kernel's ready call, before any frame.
        let mut st = ReadyState::new();
        assert_eq!(st.on_call(), ReadyWhy::Call);
        assert_eq!(st.on_frame(true, 120, 1), ReadyWhy::Already);
        // The key: unset until finished (then READY_CONTENT_WAIT), 0 to 480 s.
        let mut c = Config::new();
        assert!(!c.given("ready-content-wait") && c.ready_content_wait == READY_CONTENT_WAIT);
        assert!(c.set("ready-content-wait", "0").is_ok() && c.ready_content_wait == 0);
        assert!(c.set("ready-content-wait", "480").is_ok() && c.set("ready-content-wait", "481").is_err());
    }
}
