// SPDX-License-Identifier: MIT
//! The monotonic clock (CLOCK_MONOTONIC), as the C runtime read it for its spans and timestamps.

/// A clock's time now (`clock_gettime`).
pub fn now(clock: libc::clockid_t) -> libc::timespec {
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    // SAFETY: a valid timespec to fill; the clocks asked for (MONOTONIC, MONOTONIC_RAW, ...) always exist on Linux.
    unsafe { libc::clock_gettime(clock, &mut ts) };
    ts
}

/// Milliseconds of CLOCK_MONOTONIC.
pub fn monotonic_ms() -> u64 {
    let ts = now(libc::CLOCK_MONOTONIC);
    ts.tv_sec as u64 * 1000 + ts.tv_nsec as u64 / 1_000_000
}

/// Microseconds of CLOCK_MONOTONIC.
pub fn monotonic_us() -> u64 {
    monotonic_ns() / 1000
}

/// Nanoseconds of CLOCK_MONOTONIC.
pub fn monotonic_ns() -> u64 {
    let ts = now(libc::CLOCK_MONOTONIC);
    ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64
}

/// Seconds of `clock`, with the fraction (the C's `tv_sec + tv_nsec / 1e9`).
pub fn seconds(clock: libc::clockid_t) -> f64 {
    let ts = now(clock);
    ts.tv_sec as f64 + ts.tv_nsec as f64 / 1e9
}

/// Seconds of CLOCK_MONOTONIC, with the fraction.
pub fn monotonic_s() -> f64 {
    seconds(libc::CLOCK_MONOTONIC)
}

#[cfg(test)]
mod tests {
    #[test]
    fn moves_forward() {
        let (a, b) = (super::monotonic_ns(), super::monotonic_ns());
        assert!(b >= a && super::monotonic_ms() >= a / 1_000_000 && super::monotonic_us() >= a / 1000);
        assert!(super::monotonic_s() >= a as f64 / 1e9);
    }
}
