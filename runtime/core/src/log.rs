// SPDX-License-Identifier: MIT
//! The runtime's one log (xnuhost's `log.c` and the bridges' `br_log`).
//!
//! Every crate logs with [`loge!`](crate::loge), [`logi!`](crate::logi), [`logd!`](crate::logd) or
//! [`xh_log!`](crate::xh_log). Where a line goes is the [`Sink`]:
//!
//! - [`Sink::Bridges`], the default: the bridges' standalone tools and tests. Every level to stderr as
//!   `bridges: error: LINE` (errors) or `bridges: LINE`, as `br_log`'s default sink printed it.
//! - [`Sink::Xnuhost`], set by [`open`]: xnuhost's log, `[   sec.ms] xnuhost E LINE` (`I`, `D`) on the log
//!   descriptor (stderr, or the file [`open`] was given), lines above the level left out. The bridges' lines inside
//!   xnuhost go there too, as main.c's `bridge_log` sent them.
//! - [`Sink::Custom`]: a tool's own printing (wifictl, bridgectl) or a test's.
//!
//! [`console`] (the guest's console bytes, line-buffered) and [`signal_line`] (async-signal-safe) always write
//! xnuhost's format to the log descriptor.
use std::ffi::CString;
use std::fmt;
use std::sync::atomic::{AtomicI32, AtomicU8, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

/// A line's level, as `XH_LOG_*` and `br_log`'s 0 (error), 1 (info), 2 (debug).
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Level {
    /// `LOGE`, `br_log(0, ...)`.
    Error = 0,
    /// `LOGI`, `br_log(1, ...)`.
    Info = 1,
    /// `LOGD`, `br_log(2, ...)`.
    Debug = 2,
}

impl Level {
    /// The level of `br_log`'s number: 0 error, 1 info, anything else debug.
    pub fn from_index(n: i64) -> Level {
        match n {
            i64::MIN..=0 => Level::Error,
            1 => Level::Info,
            _ => Level::Debug,
        }
    }

    /// xnuhost's tag of the level: `xnuhost E`, `xnuhost I`, `xnuhost D`.
    fn tag(self) -> &'static str {
        match self {
            Level::Error => "xnuhost E",
            Level::Info => "xnuhost I",
            Level::Debug => "xnuhost D",
        }
    }

    fn from_u8(n: u8) -> Level {
        Level::from_index(n.into())
    }
}

/// A custom sink: gets every line (no level filter) with its level.
pub type SinkFn = dyn Fn(Level, &str) + Send + Sync;

/// Where the lines of [`loge!`](crate::loge) and the others go.
#[derive(Clone)]
pub enum Sink {
    /// stderr, `bridges: error: LINE` or `bridges: LINE`, every level (the default).
    Bridges,
    /// xnuhost's log: timestamped lines on the log descriptor, up to the level [`open`] set.
    Xnuhost,
    /// Each line to this function, every level.
    Custom(Arc<SinkFn>),
}

impl fmt::Debug for Sink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Sink::Bridges => "Sink::Bridges",
            Sink::Xnuhost => "Sink::Xnuhost",
            Sink::Custom(_) => "Sink::Custom",
        })
    }
}

/// The longest line text, as xnuhost's `char line[1024]` (longer text is cut at a character boundary).
pub const LINE_MAX: usize = 1023;
/// The guest console's line buffer (`console_line[512]`): a longer line is broken after 511 bytes.
const CONSOLE_LINE: usize = 512;

static LOG_FD: AtomicI32 = AtomicI32::new(2);
/// xnuhost's level (`xh_log_level`), Info until [`open`].
static LEVEL: AtomicU8 = AtomicU8::new(Level::Info as u8);
/// The highest level the sink takes: [`LEVEL`] for xnuhost's, Debug for the others.
static PASS: AtomicU8 = AtomicU8::new(Level::Debug as u8);

struct State {
    sink: Sink,
    console: Vec<u8>,
}

static STATE: Mutex<State> = Mutex::new(State { sink: Sink::Bridges, console: Vec::new() });

fn state() -> MutexGuard<'static, State> {
    STATE.lock().unwrap_or_else(|e| e.into_inner())
}

fn set_pass(sink: &Sink) {
    let pass = match sink {
        Sink::Xnuhost => LEVEL.load(Ordering::Relaxed),
        _ => Level::Debug as u8,
    };
    PASS.store(pass, Ordering::Relaxed);
}

/// xnuhost's log (`xh_log_open`): its level and, when `path` is given and not empty, the file it appends to (else
/// stderr). From then on the lines go to [`Sink::Xnuhost`]. A file that cannot be opened is said on stderr and the log
/// stays on stderr.
pub fn open(path: Option<&str>, level: Level) {
    LEVEL.store(level as u8, Ordering::Relaxed);
    if let Some(path) = path.filter(|p| !p.is_empty()) {
        match open_append(path) {
            Ok(fd) => LOG_FD.store(fd, Ordering::Relaxed),
            Err(errno) => eprintln!("xnuhost: cannot open log {path}: {}", crate::sys::strerror(errno)),
        }
    }
    set_sink(Sink::Xnuhost);
}

fn open_append(path: &str) -> Result<i32, i32> {
    let c = CString::new(path).map_err(|_| libc::EINVAL)?;
    // SAFETY: a valid NUL-terminated path; the descriptor is kept for the process's life (the log's).
    let fd =
        unsafe { libc::open(c.as_ptr(), libc::O_WRONLY | libc::O_CREAT | libc::O_APPEND | libc::O_CLOEXEC, 0o600) };
    if fd < 0 {
        Err(crate::sys::errno())
    } else {
        Ok(fd)
    }
}

/// xnuhost's level (`xh_log_level`).
pub fn level() -> Level {
    Level::from_u8(LEVEL.load(Ordering::Relaxed))
}

/// Whether a line of `level` goes anywhere: check it before costly work for a debug line (the macros check it).
#[inline]
pub fn enabled(level: Level) -> bool {
    level as u8 <= PASS.load(Ordering::Relaxed)
}

/// Puts in a sink and returns the one it replaces.
pub fn set_sink(sink: Sink) -> Sink {
    let mut st = state();
    set_pass(&sink);
    std::mem::replace(&mut st.sink, sink)
}

/// The log descriptor (stderr, or [`open`]'s file): for a program that hands it to a child.
pub fn fd() -> i32 {
    LOG_FD.load(Ordering::Relaxed)
}

fn write_fd(bytes: &[u8]) {
    let fd = LOG_FD.load(Ordering::Relaxed);
    let mut rest = bytes;
    while !rest.is_empty() {
        // SAFETY: `rest` is a valid buffer of its length; write(2) is async-signal-safe.
        let n = unsafe { libc::write(fd, rest.as_ptr().cast(), rest.len()) };
        if n <= 0 {
            return;
        }
        rest = &rest[n as usize..];
    }
}

/// `[%6ld.%03ld] TAG TEXT\n` (the newline only when the text has none) in one write.
fn emit(tag: &str, text: &[u8]) {
    let ns = crate::time::monotonic_ns();
    let mut line = format!("[{:6}.{:03}] {tag} ", ns / 1_000_000_000, ns / 1_000_000 % 1000).into_bytes();
    line.extend_from_slice(text);
    if text.last() != Some(&b'\n') {
        line.push(b'\n');
    }
    write_fd(&line);
}

/// Cuts `text` to [`LINE_MAX`] bytes at a character boundary.
fn clip(text: &str) -> &str {
    crate::text::truncate(text, LINE_MAX)
}

/// Logs one line (what the macros call).
pub fn write(level: Level, args: fmt::Arguments<'_>) {
    if !enabled(level) {
        return;
    }
    let owned;
    let text = match args.as_str() {
        Some(s) => s,
        None => {
            owned = args.to_string();
            &owned
        }
    };
    write_str(level, text);
}

/// Logs one line of text.
pub fn write_str(level: Level, text: &str) {
    if !enabled(level) {
        return;
    }
    let text = clip(text);
    let st = state();
    match &st.sink {
        Sink::Xnuhost => emit(level.tag(), text.as_bytes()),
        Sink::Bridges => {
            drop(st);
            eprintln!("{}{text}", if level == Level::Error { "bridges: error: " } else { "bridges: " });
        }
        Sink::Custom(f) => {
            let f = Arc::clone(f);
            drop(st); // a sink may log itself
            f(level, text);
        }
    }
}

/// The guest console's bytes (`xh_log_raw`), line-buffered: each line as `guest     LINE` in xnuhost's format, `\r`
/// dropped, a line longer than 511 bytes broken there.
pub fn console(text: &[u8]) {
    let mut st = state();
    console_feed(&mut st.console, text, |line| emit("guest    ", line));
}

/// The console's line buffering: `line` holds the unfinished line; each finished one goes to `out`.
fn console_feed(line: &mut Vec<u8>, text: &[u8], mut out: impl FnMut(&[u8])) {
    for &c in text {
        if c == b'\r' {
            continue;
        }
        if c == b'\n' || line.len() == CONSOLE_LINE - 1 {
            out(line);
            line.clear();
            if c == b'\n' {
                continue;
            }
        }
        line.push(c);
    }
}

/// The async-signal-safe line (`xh_log_signal`): `xnuhost !! TEXT 0x%016x 0x%016x\n` straight to the log descriptor,
/// no lock, no allocation; the text is cut at 85 bytes.
pub fn signal_line(text: &str, a: u64, b: u64) {
    signal_write(&format_signal_line(text, a, b));
}

fn signal_write(line: &SignalLine) {
    write_fd(&line.buf[..line.len]);
}

struct SignalLine {
    buf: [u8; 160],
    len: usize,
}

fn format_signal_line(text: &str, a: u64, b: u64) -> SignalLine {
    const PREFIX: &[u8] = b"xnuhost !! ";
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut line = SignalLine { buf: [0; 160], len: 0 };
    let mut put = |c: u8| {
        line.buf[line.len] = c;
        line.len += 1;
    };
    PREFIX.iter().for_each(|&c| put(c));
    text.bytes().take(96 - PREFIX.len()).for_each(&mut put);
    for v in [a, b] {
        b" 0x".iter().for_each(|&c| put(c));
        for shift in (0..=60).rev().step_by(4) {
            put(HEX[((v >> shift) & 15) as usize]);
        }
    }
    put(b'\n');
    line
}

static CAPTURE: Mutex<()> = Mutex::new(());

/// The lines logged (from any thread) while it lives: a test's view of the log. Captures are serialized (one at a
/// time in the process, [`capture`] too), so tests that run at once do not mix lines; the sink before is put back on
/// drop (also on a panic).
pub struct Capture {
    lines: Arc<Mutex<Vec<(Level, String)>>>,
    before: Option<Sink>,
    _serial: MutexGuard<'static, ()>,
}

impl fmt::Debug for Capture {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Capture({} lines)", self.held().len())
    }
}

impl Capture {
    /// Starts capturing (waits for another capture to end).
    pub fn start() -> Capture {
        let serial = CAPTURE.lock().unwrap_or_else(|e| e.into_inner());
        let lines = Arc::new(Mutex::new(Vec::new()));
        let into = Arc::clone(&lines);
        let before = set_sink(Sink::Custom(Arc::new(move |level, line: &str| {
            into.lock().unwrap_or_else(|e| e.into_inner()).push((level, line.to_owned()))
        })));
        Capture { lines, before: Some(before), _serial: serial }
    }

    fn held(&self) -> MutexGuard<'_, Vec<(Level, String)>> {
        self.lines.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The lines so far.
    pub fn lines(&self) -> Vec<(Level, String)> {
        self.held().clone()
    }

    /// The lines so far, without their levels.
    pub fn texts(&self) -> Vec<String> {
        self.held().iter().map(|(_, l)| l.clone()).collect()
    }

    /// Whether a line so far contains `text`.
    pub fn has(&self, text: &str) -> bool {
        self.held().iter().any(|(_, l)| l.contains(text))
    }

    /// Forgets the lines so far.
    pub fn clear(&self) {
        self.held().clear();
    }

    /// Takes the lines so far (and forgets them).
    pub fn take(&self) -> Vec<(Level, String)> {
        std::mem::take(&mut *self.held())
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        if let Some(sink) = self.before.take() {
            set_sink(sink);
        }
    }
}

/// Runs `f` with the lines logged meanwhile captured ([`Capture`]), and returns them with `f`'s result.
pub fn capture<R>(f: impl FnOnce() -> R) -> (R, Vec<(Level, String)>) {
    let c = Capture::start();
    let result = f();
    let lines = c.take();
    drop(c);
    (result, lines)
}

/// Logs a line of `level` ([`Level`]) with Rust's format syntax: `xh_log!(Level::Info, "disk {}: {} MiB", n, mib)`.
#[macro_export]
macro_rules! xh_log {
    ($level:expr, $($arg:tt)+) => {{
        let level: $crate::log::Level = $level;
        if $crate::log::enabled(level) {
            $crate::log::write(level, format_args!($($arg)+));
        }
    }};
}

/// An error line (`LOGE`, `br_log(0, ...)`).
#[macro_export]
macro_rules! loge {
    ($($arg:tt)+) => { $crate::xh_log!($crate::log::Level::Error, $($arg)+) };
}

/// An info line (`LOGI`, `br_log(1, ...)`).
#[macro_export]
macro_rules! logi {
    ($($arg:tt)+) => { $crate::xh_log!($crate::log::Level::Info, $($arg)+) };
}

/// A debug line (`LOGD`, `br_log(2, ...)`).
#[macro_export]
macro_rules! logd {
    ($($arg:tt)+) => { $crate::xh_log!($crate::log::Level::Debug, $($arg)+) };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels() {
        assert_eq!(Level::from_index(0), Level::Error);
        assert_eq!(Level::from_index(1), Level::Info);
        assert_eq!(Level::from_index(2), Level::Debug);
        assert_eq!(Level::from_index(7), Level::Debug);
        assert!(Level::Error < Level::Info && Level::Info < Level::Debug);
    }

    #[test]
    fn capture_gets_every_level_and_restores() {
        let ((), lines) = capture(|| {
            crate::loge!("bad {}", 1);
            crate::logi!("plain");
            crate::logd!("detail {:#x}", 255);
        });
        assert_eq!(
            lines,
            vec![
                (Level::Error, "bad 1".to_owned()),
                (Level::Info, "plain".to_owned()),
                (Level::Debug, "detail 0xff".to_owned())
            ]
        );
        let ((), again) = capture(|| crate::logi!("{}", "x".repeat(2000)));
        assert_eq!(again[0].1.len(), LINE_MAX);
    }

    #[test]
    fn clip_keeps_characters_whole() {
        let s = format!("{}é", "a".repeat(LINE_MAX - 1));
        assert_eq!(clip(&s).len(), LINE_MAX - 1);
        assert_eq!(clip("short"), "short");
    }

    #[test]
    fn console_lines() {
        let mut line = Vec::new();
        let mut got: Vec<Vec<u8>> = Vec::new();
        console_feed(&mut line, b"boot\r\nhal", |l| got.push(l.to_vec()));
        assert_eq!(got, vec![b"boot".to_vec()]);
        assert_eq!(line, b"hal");
        console_feed(&mut line, b"f\n\n", |l| got.push(l.to_vec()));
        assert_eq!(got, vec![b"boot".to_vec(), b"half".to_vec(), Vec::new()]);
        got.clear();
        console_feed(&mut line, &[b'a'; 600], |l| got.push(l.to_vec()));
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].len(), CONSOLE_LINE - 1);
        assert_eq!(line.len(), 600 - (CONSOLE_LINE - 1));
    }

    #[test]
    fn signal_line_text() {
        let l = format_signal_line("pc, sp", 0x1234, u64::MAX);
        assert_eq!(&l.buf[..l.len], b"xnuhost !! pc, sp 0x0000000000001234 0xffffffffffffffff\n");
        let long = "x".repeat(200);
        let l = format_signal_line(&long, 0, 0);
        assert_eq!(l.len, 96 + 2 * 19 + 1);
    }
}
