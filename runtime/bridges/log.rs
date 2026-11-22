// SPDX-License-Identifier: MIT
//! The bridges' log (C: `br_log`, `br_log_sink` in cpu/topology.c): every bridge writes its lines here; the sink
//! defaults to stderr and xnuhost points it at its own log. A line is at most 511 bytes, as C's `char line[512]`.
//!
//! ```ignore
//! use crate::log::Level;
//! crate::br_info!("haptics: on: {node}");
//! crate::br_log!(Level::Error, "net: off: /dev/tun: {err}");
//! ```

use std::sync::{Arc, Mutex, MutexGuard, PoisonError, RwLock};

/// The level of a line: 0 error, 1 info, 2 debug (C's int levels; xnuhost maps them to its own).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Error = 0,
    Info = 1,
    Debug = 2,
}

/// Where lines go.
pub type Sink = Arc<dyn Fn(Level, &str) + Send + Sync>;

/// The longest line C's `br_log` kept (its buffer of 512 with the NUL).
pub const LINE_MAX: usize = 511;

static SINK: RwLock<Option<Sink>> = RwLock::new(None);

fn stderr_sink(level: Level, line: &str) {
    eprintln!("{}{line}", if level == Level::Error { "bridges: error: " } else { "bridges: " });
}

/// Sets the sink (None: stderr, the default) and returns the one before.
pub fn set_sink(sink: Option<Sink>) -> Option<Sink> {
    std::mem::replace(&mut *SINK.write().unwrap_or_else(PoisonError::into_inner), sink)
}

/// One line, cut to [`LINE_MAX`] bytes (at a character boundary).
pub fn log(level: Level, line: &str) {
    let line = truncate(line, LINE_MAX);
    let sink = SINK.read().unwrap_or_else(PoisonError::into_inner).clone();
    match sink {
        Some(sink) => sink(level, line),
        None => stderr_sink(level, line),
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
    ($level:expr, $($arg:tt)*) => { $crate::log::log($level, &::std::format!($($arg)*)) };
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

/// The lines logged while a [`Capture`] lives (tests of every bridge). Captures take turns: the sink is the
/// process's, so a test that captures waits for the one before it to end.
pub struct Capture {
    lines: Arc<Mutex<Vec<(Level, String)>>>,
    old: Option<Sink>,
    _turn: MutexGuard<'static, ()>,
}

static TURN: Mutex<()> = Mutex::new(());

impl Capture {
    pub fn start() -> Capture {
        let turn = TURN.lock().unwrap_or_else(PoisonError::into_inner);
        let lines: Arc<Mutex<Vec<(Level, String)>>> = Arc::default();
        let into = Arc::clone(&lines);
        let old = set_sink(Some(Arc::new(move |level, line: &str| {
            into.lock().unwrap_or_else(PoisonError::into_inner).push((level, line.to_owned()));
        })));
        Capture { lines, old, _turn: turn }
    }

    /// Every line so far.
    pub fn lines(&self) -> Vec<(Level, String)> {
        self.lines.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// The text of every line so far, one per line.
    pub fn text(&self) -> String {
        self.lines().iter().map(|(_, l)| format!("{l}\n")).collect()
    }

    /// Whether some line contains `needle`.
    pub fn has(&self, needle: &str) -> bool {
        self.lines().iter().any(|(_, l)| l.contains(needle))
    }

    /// Forgets the lines so far.
    pub fn clear(&self) {
        self.lines.lock().unwrap_or_else(PoisonError::into_inner).clear();
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        set_sink(self.old.take());
    }
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
