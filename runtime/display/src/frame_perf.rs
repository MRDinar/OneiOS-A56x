// SPDX-License-Identifier: MIT
//! Port of frame_perf.c: bounded, allocation-free display frame timing statistics. Portable host unit tests exercise
//! this code; no Android/guest headers or shared-memory writes are needed. (Host unit tests only for now; the
//! helper-side log wiring is still open: the fork's frame-pacing work.)

pub const WINDOW_NS: u64 = 5_000_000_000;
pub const ACTIVE_GAP_US: u64 = 250_000;
pub const BUCKETS: usize = 14;

const CEILINGS_US: [u64; BUCKETS] =
    [250, 500, 1000, 2000, 4000, 8000, 16000, 33000, 50000, 100000, 250000, 500000, 1000000, u64::MAX];

/// A histogram of microseconds in BUCKETS buckets (their ceilings: 250 us to 1 s, then the rest).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Hist {
    pub count: u64,
    pub sum_us: u64,
    pub max_us: u64,
    pub bins: [u64; BUCKETS],
}

impl Hist {
    fn observe(&mut self, microseconds: u64) {
        self.count += 1;
        self.sum_us += microseconds;
        self.max_us = self.max_us.max(microseconds);
        if let Some(i) = CEILINGS_US.iter().position(|&c| microseconds <= c) {
            self.bins[i] += 1;
        }
    }

    /// The ceiling of the bucket the percentile falls in (the largest value seen for the last bucket); 0 with no
    /// samples or a percentile outside 1..=100.
    pub fn quantile_bound_us(&self, percentile: u32) -> u64 {
        if self.count == 0 || !(1..=100).contains(&percentile) {
            return 0;
        }
        let target = (self.count * percentile as u64).div_ceil(100);
        let mut sum = 0;
        for (bin, &ceiling) in self.bins.iter().zip(&CEILINGS_US) {
            sum += bin;
            if sum >= target {
                return if ceiling == u64::MAX { self.max_us } else { ceiling };
            }
        }
        self.max_us
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FramePerf {
    pub window_start_ns: u64,
    pub last_post_ns: u64,
    pub posts: u64,
    pub full_copies: u64,
    pub copied_pixels: u64,
    pub active_gap_count: u64,
    pub active_gap_sum_us: u64,
    pub gap: Hist,
    pub lock: Hist,
    pub copy: Hist,
    pub post: Hist,
}

/// CLOCK_MONOTONIC in nanoseconds (0 if it cannot be read).
pub fn clock_ns() -> u64 {
    crate::sys::monotonic_ns()
}

impl FramePerf {
    pub fn begin(start_ns: u64) -> FramePerf {
        FramePerf { window_start_ns: start_ns, ..FramePerf::default() }
    }

    pub fn record(
        &mut self,
        post_time_ns: u64,
        lock_ns: u64,
        copy_ns: u64,
        post_ns: u64,
        copied_pixels: u64,
        full_copy: bool,
    ) {
        if self.last_post_ns != 0 && post_time_ns > self.last_post_ns {
            let gap_us = (post_time_ns - self.last_post_ns) / 1000;
            self.gap.observe(gap_us);
            if gap_us > 0 && gap_us <= ACTIVE_GAP_US {
                self.active_gap_count += 1;
                self.active_gap_sum_us += gap_us;
            }
        }
        self.last_post_ns = post_time_ns;
        self.posts += 1;
        self.copied_pixels += copied_pixels;
        if full_copy {
            self.full_copies += 1;
        }
        self.lock.observe(lock_ns / 1000);
        self.copy.observe(copy_ns / 1000);
        self.post.observe(post_ns / 1000);
    }

    pub fn due(&self, now_ns: u64) -> bool {
        now_ns >= self.window_start_ns && now_ns - self.window_start_ns >= WINDOW_NS
    }

    pub fn next_window(&mut self, now_ns: u64) {
        let last_post = self.last_post_ns;
        *self = FramePerf::begin(now_ns);
        self.last_post_ns = last_post; // A long idle gap must not masquerade as 60 FPS.
    }

    /// The "active" cadence excludes idle gaps (>250ms) and must not be reported as actual animation FPS if fewer
    /// than 3 active gaps were observed.
    pub fn active_cadence(&self) -> f64 {
        if self.active_gap_count < 3 || self.active_gap_sum_us == 0 {
            return 0.0;
        }
        1_000_000.0 * self.active_gap_count as f64 / self.active_gap_sum_us as f64
    }

    pub fn post_rate(&self, now_ns: u64) -> f64 {
        if now_ns <= self.window_start_ns {
            return 0.0;
        }
        self.posts as f64 * 1e9 / (now_ns - self.window_start_ns) as f64
    }
}

#[cfg(test)]
mod tests {
    // Port of tests/test_frame_perf.c.
    use super::*;

    #[test]
    fn frame_perf() {
        let t0: u64 = 10_000_000_000;
        let mut p = FramePerf::begin(t0);
        assert!(!p.due(t0 + 4_999_999_999));
        assert!(p.due(t0 + WINDOW_NS));
        assert_eq!(p.post.quantile_bound_us(95), 0);
        assert_eq!(p.active_cadence(), 0.0);
        // Four 16 ms intervals give actual animation-cadence sampling.
        for i in 0..5u64 {
            p.record(t0 + 100_000_000 + 16_000_000 * i, 1_500_000, 2_000_000, 1_100_000, 1080 * 2340, i < 4);
        }
        assert!(p.posts == 5 && p.full_copies == 4 && p.copied_pixels == 5 * 1080 * 2340);
        assert!(p.active_gap_count == 4 && p.active_gap_sum_us == 64000);
        assert!((p.active_cadence() - 62.5).abs() < 0.001);
        assert_eq!(p.lock.quantile_bound_us(95), 2000);
        assert_eq!(p.copy.quantile_bound_us(50), 2000);
        assert!(p.gap.count == 4 && p.gap.max_us == 16000);
        assert_eq!(p.post_rate(t0 + 5_000_000_000), 1.0);
        p.next_window(t0 + WINDOW_NS);
        assert!(p.posts == 0 && p.last_post_ns == t0 + 164_000_000);
        p.record(t0 + 7_000_000_000, 50_000_000, 100_000, 100_000, 2, false);
        assert_eq!(p.active_gap_count, 0); // idle gap is deliberately excluded
        assert_eq!(p.active_cadence(), 0.0);
        assert!(p.gap.count == 1 && p.gap.max_us > 1_000_000);
        assert_eq!(p.lock.quantile_bound_us(95), 50000);
        assert_eq!(p.post.quantile_bound_us(95), 250);
        assert_eq!(p.gap.quantile_bound_us(0), 0);
    }
}
