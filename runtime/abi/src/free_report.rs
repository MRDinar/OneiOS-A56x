// SPDX-License-Identifier: MIT
//! `free_report.h` (hosted kernel patch 0058) and `free_report_runs.h` (0066): free-page reporting, the guest's
//! decisions. No state. The C headers stay: the kernel has the same files (osfmk/arm64/hosted/), and
//! `tests/headers.rs` checks that the patches add them line for line; these are their Rust mirrors, with the C tests'
//! cases.
//!
//! The goal: iOS keeps all of its ram=auto memory; the host gets back only pages iOS has freed. The guest gives back
//! pages that sit on XNU's own free queue (memory iOS is not using at all), never cached, compressed or wired pages,
//! through the ram_discard host call (a hole punched in the RAM memfd: the page reads as zero when it is used again).
//! XNU's free count, targets and memorystatus are not changed by this.
//!
//! 0066: 0058 took up to 64 pages at a time off XNU's free queue, coldest first. Freed pages come back to the free
//! queue in the order iOS frees them, so 64 of them are 64 places scattered over the RAM: on the phone (2026-11-12
//! 23:45 boot) 205 MiB went back in 822 batches as 52602 runs: each run one 4 KiB page, one ram_discard call. 0066
//! takes each cold page with the free, held pages next to it in physical memory (vm_resident.c), in batches of up to
//! 1024 pages when the free queue holds that much above keep, and gives a batch back as its runs: sorted by page
//! number, neighbours joined.

/// `xh_fr_batch`: how many pages to give back now. `held` is the number of free-queue pages the host still holds
/// (freed by iOS since they were last given back), `keep` the number the guest always leaves held (the pages the
/// next allocations take: giving them back would only make the host fill them again), `hysteresis` how far above
/// `keep` the count must go before a round starts, `batch` the most pages taken off the free queue at once.
/// `active` is true while a round runs: a round starts above keep + hysteresis and goes on down to keep, so that a
/// free count that moves around keep does not start a round for every few pages.
pub const fn batch(held: u32, keep: u32, hysteresis: u32, batch: u32, active: bool) -> u32 {
    let start = keep as u64 + if active { 0 } else { hysteresis as u64 };
    if held as u64 <= start || batch == 0 {
        return 0;
    }
    if held - keep < batch {
        held - keep
    } else {
        batch
    }
}

/// Writes the runs of consecutive numbers of the sorted `pages` to `first` and `count`, a duplicate merged.
fn write_runs(pages: &[u32], first: &mut [u32], count: &mut [u32]) -> usize {
    let mut runs = 0;
    for &page in pages {
        if runs > 0 {
            let next = first[runs - 1].wrapping_add(count[runs - 1]);
            if page == next {
                count[runs - 1] += 1;
                continue;
            }
            if page < next {
                continue; // a duplicate
            }
        }
        first[runs] = page;
        count[runs] = 1;
        runs += 1;
    }
    runs
}

/// `xh_fr_runs`: the pages of a batch as ranges for the host: sorts `pages` (page numbers) in place (by insertion:
/// a 0058 batch is <= 256) and writes runs of consecutive numbers to `first` and `count` (room for as many as
/// `pages`). Returns the number of runs. Duplicates (none should exist) are merged, so a page is never given twice.
pub fn runs(pages: &mut [u32], first: &mut [u32], count: &mut [u32]) -> usize {
    for i in 1..pages.len() {
        let v = pages[i];
        let mut j = i;
        while j > 0 && pages[j - 1] > v {
            pages[j] = pages[j - 1];
            j -= 1;
        }
        pages[j] = v;
    }
    write_runs(pages, first, count)
}

/// `xh_fr_keep_per_queue`: the share of `keep` each free queue (XNU's page colours) leaves held: the reporter takes
/// from each queue only what it holds above this, coldest first.
pub const fn keep_per_queue(keep: u32, queues: u32) -> u32 {
    if queues == 0 {
        return keep;
    }
    keep.div_ceil(queues)
}

/// Moves `pages[root]` down the heap `pages[..end]` until every parent is at least its children.
fn sift_down(pages: &mut [u32], mut root: usize, end: usize) {
    loop {
        let mut child = 2 * root + 1;
        if child >= end {
            return;
        }
        if child + 1 < end && pages[child] < pages[child + 1] {
            child += 1;
        }
        if pages[root] >= pages[child] {
            return;
        }
        pages.swap(root, child);
        root = child;
    }
}

/// `xh_fr_sort`: sorts page numbers in place, ascending: a heap sort (no recursion, no memory, n log n for a batch
/// of 1024), the kernel's own algorithm.
pub fn sort(pages: &mut [u32]) {
    let n = pages.len();
    if n < 2 {
        return;
    }
    for start in (0..n / 2).rev() {
        sift_down(pages, start, n); // the heap: every parent at least its children
    }
    for end in (1..n).rev() {
        pages.swap(0, end); // the largest to the end, the heap one shorter
        sift_down(pages, 0, end);
    }
}

/// `xh_fr_sorted_runs`: the pages of a batch as ranges for the host: sorts `pages` in place and writes runs of
/// consecutive numbers to `first` and `count` (room for as many as `pages`). Returns the number of runs. Duplicates
/// (none should exist) are merged, so a page is never given twice. The same result as [`runs`], which sorts by
/// insertion (fine for 64).
pub fn sorted_runs(pages: &mut [u32], first: &mut [u32], count: &mut [u32]) -> usize {
    sort(pages);
    write_runs(pages, first, count)
}

/// `xh_fr_batch_pages`: how many pages one batch takes: what the free queues hold above keep (`excess`), at most
/// `most`. `slice` is how many the reporter takes, or puts back, under one hold of the free queue's lock (it drops
/// the lock between slices, so an allocation waits for at most one slice); a batch is a whole number of slices where
/// it can be.
pub const fn batch_pages(excess: u32, most: u32, slice: u32) -> u32 {
    if excess == 0 || most == 0 {
        return 0;
    }
    let mut n = if excess < most { excess } else { most };
    if slice != 0 && n > slice && n % slice != 0 && n != excess {
        n -= n % slice;
    }
    n
}

/// `xh_fr_neighbours`: the pages a cold page brings with it: up to `room` more (what the batch and the free queue's
/// excess still allow), at most `run_most` in one run. `ahead` and `behind` are how many free, held pages follow and
/// precede it in physical memory (the caller counts at most run_most of each). Returns (ahead, behind) to take.
/// Pages ahead first: the coldest page is often the first of what an exit freed.
pub const fn neighbours(ahead: u32, behind: u32, room: u32, run_most: u32) -> (u32, u32) {
    let mut most = if run_most > 0 { run_most - 1 } else { 0 }; // the cold page itself is one of the run
    if most > room {
        most = room;
    }
    let a = if ahead < most { ahead } else { most };
    let b = if behind < most - a { behind } else { most - a };
    (a, b)
}

#[cfg(test)]
mod tests {
    use super::*;

    // The batch and the hysteresis: a round starts above keep + hysteresis and runs down to keep.
    #[test]
    fn batch_and_hysteresis() {
        assert_eq!(batch(100, 100, 50, 64, false), 0);
        assert_eq!(batch(150, 100, 50, 64, false), 0); // at keep + hysteresis: not yet
        assert_eq!(batch(151, 100, 50, 64, false), 51);
        assert_eq!(batch(400, 100, 50, 64, false), 64); // at most a batch at once
        assert_eq!(batch(120, 100, 50, 64, true), 20); // in a round: on down to keep
        assert!(batch(100, 100, 50, 64, true) == 0 && batch(99, 100, 50, 64, true) == 0);
        assert!(batch(0, 0, 0, 64, false) == 0 && batch(1, 0, 0, 64, false) == 1);
        assert_eq!(batch(500, 100, 50, 0, false), 0);
        assert_eq!(batch(0xffff_ffff, 0xffff_fff0, 0x100, 64, false), 0); // keep + hysteresis past 32 bits
                                                                          // A sequence of free counts: no round below the start, one round per rise, each down to keep exactly.
        let (mut held, mut given, mut rounds) = (0u32, 0u32, 0u32);
        for frees in [30, 30, 30, 200, 0, 10, 10, 300, 0, 0] {
            held += frees;
            let mut active = false;
            let mut started = false;
            loop {
                let n = batch(held, 100, 50, 64, active);
                if n == 0 {
                    break;
                }
                started |= !active;
                active = true;
                held -= n;
                given += n;
            }
            rounds += started as u32;
            assert!(held <= 150);
        }
        assert!(rounds == 2 && held == 100 && given == 510);
    }

    // Runs: sorted, consecutive pages merged, a duplicate given once.
    #[test]
    fn insertion_runs() {
        let mut pages = [9, 3, 4, 5, 20, 8, 21, 5, 100];
        let (mut first, mut count) = ([0; 9], [0; 9]);
        assert_eq!(runs(&mut pages, &mut first, &mut count), 4);
        assert_eq!((&first[..4], &count[..4]), (&[3, 8, 20, 100][..], &[3, 2, 2, 1][..]));
        let (mut one, mut f1, mut c1) = ([7], [0], [0]);
        assert!(runs(&mut one, &mut f1, &mut c1) == 1 && f1[0] == 7 && c1[0] == 1);
        assert_eq!(runs(&mut one[..0], &mut f1, &mut c1), 0);
        let mut many: [u32; 256] = core::array::from_fn(|i| 1000 + (i as u32 * 37) % 256); // 1000..1255, shuffled
        let (mut fm, mut cm) = ([0; 256], [0; 256]);
        assert!(runs(&mut many, &mut fm, &mut cm) == 1 && fm[0] == 1000 && cm[0] == 256);
        assert!(keep_per_queue(2048, 32) == 64 && keep_per_queue(2049, 32) == 65);
        assert!(keep_per_queue(10, 0) == 10 && keep_per_queue(1, 32) == 1);
    }

    // 0066: the heap sort and its runs give what the insertion sort gave, for any order and with duplicates.
    #[test]
    fn heap_sort_runs() {
        let mut seed: u32 = 12345;
        for round in 0..200u32 {
            let n = (1 + round * 5 % 1024) as usize;
            let mut a = [0u32; 1024];
            for (i, page) in a[..n].iter_mut().enumerate() {
                seed = seed.wrapping_mul(1103515245).wrapping_add(12345);
                *page = match round % 3 {
                    0 => (seed >> 8) % (n as u32 * 2), // random
                    1 => 5000 + i as u32,              // ascending
                    _ => 9000 - i as u32,              // descending
                };
            }
            let mut b = a;
            let (mut fa, mut ca, mut fb, mut cb) = ([0; 1024], [0; 1024], [0; 1024], [0; 1024]);
            let ra = if n <= 256 { runs(&mut a[..n], &mut fa, &mut ca) } else { 0 };
            let rb = sorted_runs(&mut b[..n], &mut fb, &mut cb);
            assert!(b[..n].windows(2).all(|w| w[0] <= w[1]));
            if n <= 256 {
                assert!(ra == rb && fa[..ra] == fb[..rb] && ca[..ra] == cb[..rb]);
            }
            if round % 3 != 0 {
                assert!(rb == 1 && cb[0] == n as u32); // n consecutive pages are one run
            }
        }
        let mut dup = [4, 4, 5, 3, 3, 9];
        let (mut fd, mut cd) = ([0; 6], [0; 6]);
        assert_eq!(sorted_runs(&mut dup, &mut fd, &mut cd), 2);
        assert_eq!((&fd[..2], &cd[..2]), (&[3, 9][..], &[3, 1][..]));
        assert_eq!(sorted_runs(&mut dup[..0], &mut fd, &mut cd), 0);
    }

    // The batch: what is above keep, at most `most`, whole slices where it is cut.
    #[test]
    fn batch_slices() {
        assert!(batch_pages(0, 1024, 128) == 0 && batch_pages(50, 0, 128) == 0);
        assert!(batch_pages(50, 1024, 128) == 50 && batch_pages(300, 1024, 128) == 300);
        assert!(batch_pages(5000, 1024, 128) == 1024 && batch_pages(5000, 1000, 128) == 896);
        assert_eq!(batch_pages(5000, 64, 128), 64); // 0058's batch: smaller than a slice
    }

    // Neighbours: ahead first, the run at most run_most pages with the cold page, never past the batch's room.
    #[test]
    fn neighbour_pages() {
        assert_eq!(neighbours(10, 10, 100, 256), (10, 10));
        assert_eq!(neighbours(300, 300, 1000, 256), (255, 0));
        assert_eq!(neighbours(100, 300, 1000, 256), (100, 155));
        assert_eq!(neighbours(100, 300, 20, 256), (20, 0));
        assert_eq!(neighbours(5, 5, 100, 1), (0, 0)); // run 1: 0058's way
        assert_eq!(neighbours(5, 5, 0, 256), (0, 0));
    }
}
