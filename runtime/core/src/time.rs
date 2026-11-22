// SPDX-License-Identifier: MIT
//! The monotonic clock (CLOCK_MONOTONIC), as the C runtime read it for its spans and timestamps.

fn now() -> libc::timespec {
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    // SAFETY: a valid timespec to fill; CLOCK_MONOTONIC always exists on Linux.
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    ts
}

/// Milliseconds of CLOCK_MONOTONIC.
pub fn monotonic_ms() -> u64 {
    let ts = now();
    ts.tv_sec as u64 * 1000 + ts.tv_nsec as u64 / 1_000_000
}

/// Nanoseconds of CLOCK_MONOTONIC.
pub fn monotonic_ns() -> u64 {
    let ts = now();
    ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64
}

#[cfg(test)]
mod tests {
    #[test]
    fn moves_forward() {
        let (a, b) = (super::monotonic_ns(), super::monotonic_ns());
        assert!(b >= a && super::monotonic_ms() >= a / 1_000_000);
    }
}
