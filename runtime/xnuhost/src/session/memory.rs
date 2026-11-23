// SPDX-License-Identifier: MIT
//! main.c's lines about the guest's RAM: the choice at the start (ram=auto, what fits), the RAM held fixed (ramfix),
//! and the main loop's lasting lines: the host's memory every host-mem-log seconds, the host swapping iOS's RAM, the
//! lock's valve, the 2 MiB pages.

use std::sync::atomic::Ordering::{Acquire, Relaxed, Release};

use xh_core::{loge, logi};
use xnuhost_abi::XH_FEATURE_RAM_FIXED;

use super::mono_s;
use crate::config::{self, Config, RamBound};
use crate::discard::{self, DISCARD, FIXED_REFUSED};
use crate::mem;
use crate::ramfix::{self, Counters, Ramfix};

/// The memfd's held bytes (st_blocks: swapped pages count too).
fn held(fd: i32) -> Option<u64> {
    let mut st = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: fstat fills st for a descriptor; its result is checked before st is read.
    (fd >= 0 && unsafe { libc::fstat(fd, st.as_mut_ptr()) } == 0)
        // SAFETY: fstat succeeded, so st is filled.
        .then(|| unsafe { st.assume_init() }.st_blocks as u64 * 512)
}

/// The RAM's bytes in the host's RAM from `from` to the end (mincore of that part; [0, from) counted as in).
fn resident_from(m: &mem::Mem, from: u64) -> u64 {
    if from >= m.size {
        return m.size;
    }
    from + discard::ram_resident(m.host.wrapping_add(from as usize), m.size - from, xh_core::sys::page_size())
}

/// What the main loop's memory lines remember between two looks.
#[derive(Default)]
pub struct MemoryWatch {
    /// the fixed RAM (None: not started, or ram-giveback=1's way)
    pub ramfix: Option<&'static Ramfix>,
    /// the `memory: fixed RAM` line's counters at its last line (first: the start)
    was: Option<Counters>,
    host_last: Option<u64>,
    swap_last: Option<u64>,
    swap_next: u64,
    swap_was: (u64, u64),
    huge_said: bool,
    valve_last: Option<u64>,
}

impl MemoryWatch {
    fn all_locked(&self, m: &mem::Mem) -> bool {
        self.ramfix.is_some_and(|r| !r.unlocked.load(Relaxed) && r.locked.load(Relaxed) == m.size && m.size != 0)
    }

    /// The guest's RAM held fixed for the session (ramfix.rs, docs/booter.md 2.2 "Fixed RAM"): the kernel is told
    /// before it starts (XH_FEATURE_RAM_FIXED: no free pages given back), and the whole memfd is prefaulted, locked
    /// where Linux allows and it fits, on a nice-19 thread while the boot goes on. ram-giveback=1: 0058's way, nothing
    /// of this.
    pub fn ram_fix_start(&mut self, config: &Config) {
        self.was = Some(ramfix::sample());
        if config.ram_giveback {
            logi!("memory: ram-giveback=1: the guest's RAM is faulted in as it is used and its free pages are given back (0058); not prefaulted, not locked");
            return;
        }
        crate::hostcalls::update_info(|i| i.features |= XH_FEATURE_RAM_FIXED);
        if !config.ram_prefault {
            logi!("memory: the guest's RAM is held for the session, not prefaulted (ram-prefault=0); free pages are not given back");
            return;
        }
        let m = mem::state();
        let facts = ramfix::facts();
        let mut opts = ramfix::Options {
            lock: config.ram_lock,
            lock_why: (!config.ram_lock).then(|| "ram-lock=0".to_owned()),
            step: 0,
            unlock_below: config.ram_unlock_below,
            meminfo: Some(config.ram_meminfo.clone()),
            ..ramfix::Options::default()
        };
        // A lock is safe only with room beside it: the RAM must be within what fits (config.rs, ram_fit) at the start.
        if opts.lock && config.ram_fit_available != 0 && m.size > config.ram_fit {
            opts.lock = false;
            let r: &'static Ramfix = Box::leak(Box::new(Ramfix::new(opts)));
            // Not prefaulted either: an unlocked page iOS has not used is the first the host swaps (PC boot of
            // 2026-11-14).
            logi!(
                "memory: the guest's RAM is held for the session: {} MiB, not locked and not prefaulted (more than fits beside the session's needs: {} MiB); free pages are not given back (ram-giveback=0); {}",
                m.size >> 20,
                config.ram_fit >> 20,
                facts
            );
            // No prefault runs: done, so that the `memory: fixed RAM` line says "held, not locked" (it said "prefault
            // running" for the whole session before 2026-11-15) and the 2 MiB pages line comes at once.
            r.done.store(true, Release);
            self.ramfix = Some(r);
            return;
        }
        let r: &'static Ramfix = Box::leak(Box::new(Ramfix::new(opts)));
        self.ramfix = Some(r);
        let valve = if r.opts.lock {
            format!(
                " (the valve: unlocked once if host MemAvailable falls under {} MiB, checked once a second)",
                r.opts.unlock_below >> 20
            )
        } else {
            String::new()
        };
        logi!(
            "memory: the guest's RAM is held for the session: {} MiB, prefaulted on a thread at nice 19 in {} MiB steps, {}{}; free pages are not given back (ram-giveback=0); {}",
            m.size >> 20,
            ramfix::STEP >> 20,
            if r.opts.lock { "locked as it goes (mlock)" } else { r.opts.lock_why.as_deref().unwrap_or("") },
            valve,
            facts
        );
        if ramfix::start(r, m.host, m.size).is_err() {
            loge!("memory: the guest's RAM is not prefaulted");
        }
    }

    /// Every 5 s: the guest's RAM that the host swapped out (held, st_blocks, less in the host's RAM, mincore), said in
    /// plain words once past 64 MiB and again at each further 256 MiB (swap_note): a swapped page stalls whichever vCPU
    /// or task touches it next (the phone, 2026-11-14: 2126 MiB swapped out, Settings frozen). Read only. Cheap
    /// (fix-ram-locked, 2026-11-15): while the whole RAM is locked nothing of it can be swapped, so nothing is read at
    /// all; with part of it locked the mincore covers only the rest.
    pub fn swap_watch(&mut self, config: &Config) {
        let now = mono_s();
        if self.swap_last.is_some_and(|last| now - last < 5) {
            return;
        }
        let m = mem::state();
        if self.all_locked(&m) {
            self.swap_last = None; // the next look (after a valve) starts afresh
            return;
        }
        // The host's swap traffic over the last interval (/proc/vmstat pswpin, pswpout: pages; zram on the phone, where
        // it is also heat: 2026-11-14, about 9.5 MB/s in and 8 MB/s out while iOS's RAM was swapped).
        let (now_ins, now_outs) = ramfix::vmstat_swaps().unwrap_or((0, 0));
        let seconds = self.swap_last.map_or(0, |last| now - last);
        let (was_ins, was_outs) = self.swap_was;
        let (ins, outs) = if seconds != 0 && now_ins >= was_ins && now_outs >= was_outs {
            ((now_ins - was_ins) / seconds, (now_outs - was_outs) / seconds)
        } else {
            (0, 0)
        };
        self.swap_was = (now_ins, now_outs);
        self.swap_last = Some(now);
        if m.host.is_null() {
            return;
        }
        let Some(held) = held(m.fd) else { return };
        let r = self.ramfix;
        let locked = r.map_or(0, |r| r.locked.load(Relaxed));
        let unlocked = r.is_some_and(|r| r.unlocked.load(Relaxed));
        let in_ram = resident_from(&m, if unlocked { 0 } else { locked });
        let swapped = held.saturating_sub(in_ram);
        if !config::swap_note(swapped, &mut self.swap_next) {
            return;
        }
        let available = config::meminfo(&config.ram_meminfo).map_or(0, |(_, a)| a);
        let page = xh_core::sys::page_size();
        loge!(
            "memory: the host swaps iOS's RAM: {} MiB (host MemAvailable {} MiB); host swap-ins {} pages/s ({:.1} MB/s), swap-outs {} pages/s ({:.1} MB/s)",
            swapped >> 20,
            available >> 20,
            ins,
            (ins * page) as f64 / 1e6,
            outs,
            (outs * page) as f64 / 1e6
        );
    }

    /// Every host-mem-log seconds (or `now`): what the host has left and how much of the guest's RAM it holds (the
    /// memfd's pages that were touched), what the guest gave back with ram_discard, the guest's RAM the host swapped,
    /// and the fixed RAM's state. The host's low-memory killer may end xnuhost when the host runs out: then the watcher
    /// returns to One UI. Read only.
    pub fn host_memory_tick(&mut self, config: &Config, now: bool) {
        let t = mono_s();
        if config.host_mem_log == 0 && !now {
            return;
        }
        if !now && self.host_last.is_some_and(|last| t - last < config.host_mem_log as u64) {
            return;
        }
        self.host_last = Some(t);
        // One read of /proc/meminfo for every figure here.
        let keys = [
            "MemTotal",
            "MemAvailable",
            "SwapTotal",
            "SwapFree",
            "PageTables",
            "Mlocked",
            "ShmemHugePages",
            "ShmemPmdMapped",
        ];
        let mi: Vec<u64> =
            config::proc_values(&config.ram_meminfo, &keys).into_iter().map(|v| v.unwrap_or(0)).collect();
        let (total, available, swap_total, swap_free, tables) = (mi[0], mi[1], mi[2], mi[3], mi[4]);
        let m = mem::state();
        let held = held(m.fd).unwrap_or(0);
        if total == 0 {
            return;
        }
        // What the guest gave back since the start (ram_discard: its free pages, kernel 0058), and the time it took.
        logi!(
            "memory: host MemAvailable {} MiB of {} MiB; the guest's RAM holds {} of {} MiB; given back {} MiB of free pages in {} calls ({} failed, {} ms)",
            available >> 20,
            total >> 20,
            held >> 20,
            m.size >> 20,
            DISCARD.bytes.load(Relaxed) >> 20,
            DISCARD.calls.load(Relaxed),
            DISCARD.failed.load(Relaxed),
            DISCARD.ns.load(Relaxed) / 1_000_000
        );
        // Where the host's memory is besides (2026-11-13): the guest's RAM that the host swapped out (zram on the
        // phone), the host's swap in use, and its page tables (each iOS task is a Linux process mapping the memfd). A
        // second line, so that the one above keeps its form for the log tools. The locked part is resident while VmLck
        // covers it: the mincore looks only at the rest (nothing with the whole RAM locked).
        let mut c = ramfix::sample();
        c.mlocked = mi[5];
        let from = self.ramfix.map_or(0, |r| r.unlocked_from(c.vm_locked));
        let in_ram = if m.host.is_null() { 0 } else { resident_from(&m, from) };
        logi!(
            "memory: the guest's RAM: {} MiB in the host's RAM, {} MiB swapped out by the host; host swap {} of {} MiB used, page tables {} MiB (xnuhost's own {} MiB); 2 MiB pages: host ShmemHugePages {} MiB, ShmemPmdMapped {} MiB",
            in_ram >> 20,
            held.saturating_sub(in_ram) >> 20,
            swap_total.saturating_sub(swap_free) >> 20,
            swap_total >> 20,
            tables >> 20,
            c.vm_pte >> 20,
            mi[6] >> 20,
            mi[7] >> 20
        );
        // The fixed RAM (2026-11-13, ramfix.rs): what is held and locked, what was refused, and since the last line the
        // host's swap traffic (all processes) and this process's faults that needed the host's I/O. With the RAM held
        // and locked, `held` stays at the RAM's size, `swapped out` at 0, and the KGSL figures flat once the UI is up.
        let k = ramfix::kgsl_memory(std::process::id() as i32);
        let kgsl = if k.is_empty() { String::new() } else { format!("; KGSL: {k}") };
        let r = self.ramfix;
        let state = if config.ram_giveback {
            "given back as freed (ram-giveback=1)"
        } else if !config.ram_prefault {
            "held, not prefaulted"
        } else if !r.is_some_and(|r| r.done.load(Acquire)) {
            "held, prefault running"
        } else if r.is_some_and(|r| r.unlocked.load(Relaxed)) {
            "held, unlocked by the valve"
        } else if self.all_locked(&m) {
            "held and locked"
        } else if r.is_some_and(|r| r.locked.load(Relaxed) != 0) {
            "held, locked in part"
        } else {
            "held, not locked"
        };
        let d = |now: u64, was: fn(&Counters) -> u64| self.was.as_ref().map_or(0, |w| now.wrapping_sub(was(w)));
        logi!(
            "memory: fixed RAM: {}; prefaulted {} of {} MiB, locked {} MiB (VmLck; host Mlocked {} MiB); give-back refused {} calls; since the last line: host swap-ins +{}, swap-outs +{} pages, xnuhost major faults +{}, minor +{}{}",
            state,
            r.map_or(0, |r| r.prefaulted.load(Relaxed)) >> 20,
            m.size >> 20,
            c.vm_locked >> 20,
            c.mlocked >> 20,
            FIXED_REFUSED.load(Relaxed),
            d(c.swap_ins, |w| w.swap_ins),
            d(c.swap_outs, |w| w.swap_outs),
            d(c.major_faults, |w| w.major_faults),
            d(c.minor_faults, |w| w.minor_faults),
            kgsl
        );
        self.was = Some(c);
    }

    /// The window's 2 MiB pages (fix-ram-hugepages): once when the prefault is done (or at once without one) and at
    /// the stop. smaps_rollup walks every mapping of this process, so not every host-mem-log. ShmemPmdMapped is what
    /// xnuhost maps with 2 MiB entries: with shmem THP and the RAM prefaulted, the whole window; the tasks map the same
    /// folios 4 KiB at a time.
    pub fn huge_report(&self, when: &str) {
        let m = mem::state();
        if m.host.is_null() {
            return;
        }
        let Some((shmem, file, anon)) = ramfix::pmd_mapped_self() else { return };
        let c = ramfix::sample();
        // With 4 KiB entries xnuhost's window alone takes 8 bytes per 4 KiB page: the size / 512 that 2 MiB entries save.
        logi!(
            "memory: 2 MiB pages{}: xnuhost maps {} of {} MiB of the guest's RAM with 2 MiB entries (ShmemPmdMapped; FilePmdMapped {}, AnonHugePages {} MiB); its page tables {} MiB (VmPTE; 4 KiB entries for the whole window would be {} MiB)",
            when,
            shmem >> 20,
            m.size >> 20,
            file >> 20,
            anon >> 20,
            c.vm_pte >> 20,
            (m.size / 512) >> 20
        );
    }

    pub fn huge_tick(&mut self, config: &Config) {
        if self.huge_said || !mem::state().huge {
            return;
        }
        if config.ram_prefault && !self.ramfix.is_some_and(|r| r.done.load(Acquire)) {
            return;
        }
        self.huge_said = true;
        self.huge_report(if config.ram_prefault { " (the prefault done)" } else { "" });
    }

    /// Once a second while the guest's RAM is locked: the valve (ramfix.rs). On the main thread (nice 0, SCHED_OTHER),
    /// so busy vCPUs cannot starve it as they could the prefault's nice-19 thread, where it ran before 2026-11-15. One
    /// read of /proc/meminfo a second while locked, nothing otherwise. The munlock it may do (once a session) holds
    /// this loop for as long as it takes; the fork's kill is the backstop under everything.
    pub fn ram_valve_tick(&mut self) {
        let now = mono_s();
        if self.valve_last == Some(now) {
            return;
        }
        self.valve_last = Some(now);
        let Some(r) = self.ramfix else { return };
        if let Some((available, true)) = r.valve_tick(mem::state().host) {
            loge!(
                "memory: host MemAvailable {} MiB is under ram-unlock-below ({} MiB): the guest's RAM is unlocked for the rest of the session; the host may swap it now (it stays held, nothing is given back)",
                available >> 20,
                r.opts.unlock_below >> 20
            );
        }
    }

    /// The stop: what was held to the end; the prefault ends.
    pub fn stop(&self) {
        let locked = self.ramfix.map_or(0, |r| {
            ramfix::stop(r);
            r.locked.load(Relaxed)
        });
        logi!(
            "memory: the guest's RAM: {} MiB held to the end, {} MiB of it locked; the exit gives all of it back to the host",
            mem::state().size >> 20,
            locked >> 20
        );
    }
}

/// The guest's RAM as chosen (config.rs, ram=auto), once at the start.
pub fn ram_choice_log(config: &Config) {
    // What fits beside the session's needs (config.rs ram_fit, 2026-11-14): MemAvailable at the start, less the
    // headroom, the GPU's allowance and the page tables' share of the guest.
    let fits = if config.ram_fit_available != 0 {
        format!(
            "what fits beside the session's needs: {} MiB (MemAvailable {} MiB less {} MiB headroom{}, {} MiB for the GPU and {}% of the guest for page tables)",
            config.ram_fit >> 20,
            config.ram_fit_available >> 20,
            config.ram_session_reserve >> 20,
            if config.ram_lock_expected {
                " with the lock"
            } else if config.ram_lock {
                " (the lock is not expected to work here)"
            } else {
                ""
            },
            config.ram_gpu_reserve >> 20,
            config.ram_pagetable_percent
        )
    } else {
        String::new()
    };
    if config.ram_auto_total == 0 {
        logi!(
            "memory: the guest gets {} MiB (ram={}){}{}",
            config.ram_bytes >> 20,
            if config.given("ram") { "SIZE" } else { "auto: no MemTotal, 4 GiB" },
            if fits.is_empty() { "" } else { "; " },
            fits
        );
        // A size given by hand that the host cannot hold: Linux will swap the guest (zram), and every page iOS touches
        // there stalls it.
        if config.ram_fit_available != 0 && config.ram_bytes > config.ram_fit {
            loge!(
                "memory: ram={} MiB is more than this host can hold without swapping it ({} MiB fit): the host will swap iOS's RAM; give ram=auto or ram={}M",
                config.ram_bytes >> 20,
                config.ram_fit >> 20,
                (config.ram_fit >> 20) & !255
            );
        }
        return;
    }
    let (total, floor) = (config.ram_auto_total, config.ram_host_floor);
    let bound = match config.ram_auto_bound {
        Some(RamBound::Share) => "the share",
        Some(RamBound::Floor) => "the floor",
        Some(RamBound::Available) => "what fits",
        None => "?",
    };
    logi!(
        "memory: ram=auto: host MemTotal {} MiB, MemAvailable {} MiB; the guest gets {} MiB: the least of {}% ({} MiB), MemTotal less the host's floor of {} MiB ({} MiB) and {}, in 256 MiB steps ({} decides)",
        total >> 20,
        config.ram_auto_available >> 20,
        config.ram_bytes >> 20,
        config.ram_percent,
        (total / 100 * config.ram_percent as u64) >> 20,
        floor >> 20,
        total.saturating_sub(floor) >> 20,
        if config.ram_fit_on { fits.as_str() } else { "nothing else (ram-fit=0)" },
        bound
    );
}
