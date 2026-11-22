// SPDX-License-Identifier: MIT
//! Small helpers of the camera crate: the helper's own log (`cam_log`, `cam_verbose`), text cut as C's buffers cut
//! it, the C text of an errno, the clock.
use std::sync::atomic::{AtomicU8, Ordering};

pub use xh_core::sys::strerror;

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

static VERBOSE: AtomicU8 = AtomicU8::new(0);

/// `cam_verbose`: 0; 1 (-v) the steps and the first callbacks; 2 (-vv) every callback and frame.
pub fn verbose() -> u8 {
    VERBOSE.load(Ordering::Relaxed)
}

pub fn set_verbose(level: u8) {
    VERBOSE.store(level.min(2), Ordering::Relaxed);
}

/// One line of the helper's (and camctl's) diagnostics on stderr: `oneios-camera: LINE`, the line at most 399 bytes
/// (C's `char line[400]`).
pub fn cam_log_line(line: &str) {
    eprintln!("oneios-camera: {}", truncate(line, 399));
}

/// `cam_log!("format", args...)`: [`cam_log_line`] of the formatted text.
#[macro_export]
macro_rules! cam_log {
    ($($arg:tt)*) => { $crate::util::cam_log_line(&::std::format!($($arg)*)) };
}

/// CLOCK_MONOTONIC in nanoseconds.
pub fn monotonic_ns() -> u64 {
    // SAFETY: a valid timespec to fill.
    let mut t: libc::timespec = unsafe { std::mem::zeroed() };
    // SAFETY: as above; CLOCK_MONOTONIC always exists.
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut t) };
    t.tv_sec as u64 * 1_000_000_000 + t.tv_nsec as u64
}

/// CLOCK_MONOTONIC in seconds (camctl's `now()`).
pub fn now() -> f64 {
    monotonic_ns() as f64 / 1e9
}

/// Sleeps until CLOCK_MONOTONIC reaches `deadline_ns` (the test back end's frame pacing, `clock_nanosleep` with
/// TIMER_ABSTIME).
pub fn sleep_until(deadline_ns: u64) {
    let t = libc::timespec {
        tv_sec: (deadline_ns / 1_000_000_000) as libc::time_t,
        tv_nsec: (deadline_ns % 1_000_000_000) as libc::c_long,
    };
    // SAFETY: a valid timespec; a NULL remainder is allowed with TIMER_ABSTIME.
    unsafe { libc::clock_nanosleep(libc::CLOCK_MONOTONIC, libc::TIMER_ABSTIME, &t, std::ptr::null_mut()) };
}

/// The longest prefix of `text` (after blanks) that is a number of `allowed` characters, as C's strto* take it.
fn number_prefix(text: &str, allowed: impl Fn(usize, char) -> bool) -> &str {
    let t = text.trim_start();
    let end = t.char_indices().find(|&(i, c)| !allowed(i, c)).map_or(t.len(), |(i, _)| i);
    &t[..end]
}

/// C's `atoi`: leading blanks, a sign, digits; 0 when there are none.
pub fn atoi(text: &str) -> i32 {
    let n = number_prefix(text, |i, c| c.is_ascii_digit() || (i == 0 && (c == '-' || c == '+')));
    let (sign, digits) = match n.strip_prefix('-') {
        Some(d) => (-1i64, d),
        None => (1, n.trim_start_matches('+')),
    };
    let value = digits.bytes().fold(0i64, |v, d| (v * 10 + i64::from(d - b'0')).min(i64::from(i32::MAX)));
    (sign * value) as i32
}

/// C's `atof` for plain decimals: the longest prefix that is a number; 0 when there is none.
pub fn atof(text: &str) -> f64 {
    let n = number_prefix(text, |i, c| c.is_ascii_digit() || c == '.' || (i == 0 && (c == '-' || c == '+')));
    (0..=n.len()).rev().find_map(|end| n[..end].parse::<f64>().ok()).unwrap_or(0.0)
}

/// `sscanf(text, "%dx%d")` == 2: two numbers with an x between them (what follows is not looked at).
pub fn parse_size(text: &str) -> Option<(i32, i32)> {
    let int = |s: &str| number_prefix(s, |i, c| c.is_ascii_digit() || (i == 0 && (c == '-' || c == '+'))).len();
    let t = text.trim_start();
    let (w, rest) = t.split_at(int(t));
    let rest = rest.strip_prefix('x')?;
    let h = &rest.trim_start()[..int(rest)];
    let has_digits = |n: &str| n.bytes().any(|b| b.is_ascii_digit());
    (has_digits(w) && has_digits(h)).then(|| (atoi(w), atoi(h)))
}

#[cfg(test)]
mod tests {
    #[test]
    fn numbers() {
        use super::*;
        assert_eq!((atoi("3"), atoi(" 12x"), atoi("-4"), atoi("x")), (3, 12, -4, 0));
        assert_eq!((atof("2.5"), atof("3x"), atof("q")), (2.5, 3.0, 0.0));
        assert_eq!(parse_size("1280x720"), Some((1280, 720)));
        assert_eq!(parse_size("1280"), None);
        assert_eq!(parse_size("ax720"), None);
        assert_eq!(parse_size("12 x720"), None);
    }

    #[test]
    fn cut() {
        assert_eq!(super::truncate("héllo", 2), "h");
        assert_eq!(super::truncate("abc", 5), "abc");
    }
}
