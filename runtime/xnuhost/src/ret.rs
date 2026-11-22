// SPDX-License-Identifier: MIT
//! Port of return.c: the return's graceful-shutdown state machine (docs/booter.md "The return"). Pure and
//! unit-tested; session.rs feeds it the return signals and the clock and does what it says. The way back to One UI
//! never depends on it: the deadline is absolute, and anything unexpected ends the wait at once.

/// What a return signal (SIGTERM/SIGINT/SIGHUP) asks (xh_return_decision).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReturnDecision {
    /// start the wait: ask iOS to shut down, keep the vCPUs running
    Graceful,
    /// graceful off, or iOS not ready: stop immediately
    StopNow,
    /// a second return request while already waiting: stop immediately
    StopSecond,
}

/// The wait's state (xh_return_state).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ReturnState {
    pub active: bool,
    pub started_ms: u64,
    pub deadline_ms: u64,
}

impl ReturnState {
    pub fn new() -> Self {
        Self::default()
    }

    /// A return signal. `graceful_ms`: return-graceful-ms (0 = the immediate kill).
    pub fn on_signal(&mut self, graceful_ms: u32, guest_ready: bool, now_ms: u64) -> ReturnDecision {
        if self.active {
            return ReturnDecision::StopSecond; // a second return request (or the chord again): stop now
        }
        if graceful_ms == 0 || !guest_ready {
            return ReturnDecision::StopNow; // graceful off, or iOS is not up to shut down: the immediate stop
        }
        self.active = true;
        self.started_ms = now_ms;
        self.deadline_ms = now_ms + graceful_ms as u64;
        ReturnDecision::Graceful // ask iOS to shut down and keep running until it does or the deadline
    }

    /// The deadline has passed.
    pub fn expired(&self, now_ms: u64) -> bool {
        self.active && now_ms >= self.deadline_ms
    }

    pub fn elapsed_ms(&self, now_ms: u64) -> u64 {
        if self.active && now_ms >= self.started_ms {
            now_ms - self.started_ms
        } else {
            0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Graceful done, timeout, panic during the wait, a second request, and the immediate kill when the window is 0 or
    // iOS is not yet up.
    #[test]
    fn return_state_machine() {
        // Graceful: a return signal while iOS is up and the window is on -> wait; it ends when iOS stops itself (the
        // loop exits on stopping()) before the deadline.
        let mut st = ReturnState::new();
        assert_eq!(st.on_signal(10000, true, 1000), ReturnDecision::Graceful);
        assert!(st.active && st.deadline_ms == 11000);
        assert!(!st.expired(5000) && !st.expired(10999));
        assert_eq!(st.elapsed_ms(4200), 3200);
        // Timeout: still waiting at the deadline -> stop.
        assert!(st.expired(11000) && st.expired(20000));
        // A second return request during the wait -> stop now, whatever the clock says.
        assert_eq!(st.on_signal(10000, true, 5000), ReturnDecision::StopSecond);
        // The window off (0) -> the immediate kill, as before; never a wait.
        let mut st = ReturnState::new();
        assert_eq!(st.on_signal(0, true, 1000), ReturnDecision::StopNow);
        assert!(!st.active && !st.expired(1_000_000));
        // iOS not up yet (no first frame) -> the immediate kill (nothing to shut down; the switch's dead-man may be
        // returning).
        let mut st = ReturnState::new();
        assert_eq!(st.on_signal(10000, false, 1000), ReturnDecision::StopNow);
        assert!(!st.active);
        // A panic during the wait is not a signal: the loop's stop(PANIC) ends the wait at once, so the state machine
        // is never asked again. The guard that a stopped-but-inactive state never looks expired:
        let st = ReturnState::new();
        assert!(!st.expired(1_000_000) && st.elapsed_ms(1_000_000) == 0);
    }
}
