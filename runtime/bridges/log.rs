// SPDX-License-Identifier: MIT
//! The bridges' log (C: `br_log` in cpu/topology.c) on the runtime's one log, [`xh_core::log`]: the sink defaults to
//! stderr (`bridges: error: LINE`, `bridges: LINE`) and xnuhost points it at its own log. A bridge's line is at most
//! 511 bytes, as br_log's `char line[512]` kept it (xnuhost's own lines may be longer).
//!
//! ```ignore
//! crate::br_info!("haptics: on: {node}");
//! crate::br_log!(Level::Error, "net: off: /dev/tun: {err}");
//! let log = crate::log::Capture::start(); // tests: the lines from every thread meanwhile
//! ```

pub use xh_core::log::{set_sink, Capture, Level, Sink};

/// The longest line br_log kept (its buffer of 512 with the NUL).
pub const LINE_MAX: usize = 511;

/// One line of a bridge, cut to [`LINE_MAX`] bytes (at a character boundary).
pub fn log(level: Level, line: &str) {
    if xh_core::log::enabled(level) {
        xh_core::log::write_str(level, truncate(line, LINE_MAX));
    }
}

/// `text` cut to at most `max` bytes, at a character boundary (C's snprintf into a buffer of `max + 1`).
pub fn truncate(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// `br_log!(level, "format", args...)`.
#[macro_export]
macro_rules! br_log {
    ($level:expr, $($arg:tt)*) => {{
        let level: $crate::log::Level = $level;
        if ::xh_core::log::enabled(level) {
            $crate::log::log(level, &::std::format!($($arg)*));
        }
    }};
}
/// An error line (level 0).
#[macro_export]
macro_rules! br_error {
    ($($arg:tt)*) => { $crate::br_log!($crate::log::Level::Error, $($arg)*) };
}
/// An info line (level 1).
#[macro_export]
macro_rules! br_info {
    ($($arg:tt)*) => { $crate::br_log!($crate::log::Level::Info, $($arg)*) };
}
/// A debug line (level 2).
#[macro_export]
macro_rules! br_debug {
    ($($arg:tt)*) => { $crate::br_log!($crate::log::Level::Debug, $($arg)*) };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_and_truncate() {
        let c = Capture::start();
        crate::br_info!("hw: {}: backend {}", "touch", "treble");
        crate::br_error!("{}", "é".repeat(300));
        let lines = c.lines();
        assert_eq!(lines[0], (Level::Info, "hw: touch: backend treble".to_owned()));
        assert_eq!(lines[1].0, Level::Error);
        assert_eq!(lines[1].1.len(), 510); // 511 would split a two-byte character
    }
}
