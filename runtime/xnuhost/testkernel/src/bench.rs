// SPDX-License-Identifier: MIT
//! The benchmarks: the trap path (testkernel-bench; `make bench-arm64`), the parallel test (testkernel-parallel;
//! `make parallel-arm64`) and the Mach RPC round trip (testkernel-rpc; `make rpc-arm64`).

use crate::{
    at, c0, cpu_index, cpu_yield, dec, host, info, irq_pending, irq_upcall, kva, now, page_alloc, put, run_thread,
    set_irq_masked, take_irqs, write64, zero, Racy,
};
use core::arch::asm;
use core::mem::{offset_of, size_of};
use core::ptr::{addr_of_mut, null_mut, write_volatile};
use core::sync::atomic::{AtomicU32, AtomicU64, Ordering::*};
use xnuhost_abi::*;

// Each loop times one kind of trip from the kernel's side, with the counter the iOS thread also reads (CNTVCT_EL0):
// the user code stamps the time just before its svc (x0) and just after its resume, so the two hand-offs are seen
// apart. Times go into a table of 4096 steps of one tick at 24 MHz or less (170 us at 24 MHz, 213 us at the phone's
// 19.2 MHz) and of hz / 24 MHz ticks on a faster counter (qemu-user's is 1 GHz: 41 ns steps, 168 us), so the table
// spans about the same time everywhere; later ones count in the last.
const BT: u64 = 4096;

pub(crate) struct Bhist {
    t: *mut u32,
    n: u64,
    sum: u64,
    step: u64,
}

impl Bhist {
    const fn new() -> Bhist {
        Bhist { t: null_mut(), n: 0, sum: 0, step: 0 }
    }

    /// A table in one 16 KiB page (4096 x 4).
    fn init(&mut self) {
        self.t = at(page_alloc());
        self.n = 0;
        self.sum = 0;
        self.step = (info().timebase_hz / 24_000_000).max(1);
    }

    fn table() -> Bhist {
        let mut h = Bhist::new();
        h.init();
        h
    }

    fn add(&mut self, ticks: u64) {
        let i = (ticks / self.step).min(BT - 1);
        // SAFETY: the table's page holds BT counters.
        unsafe {
            let slot = self.t.add(i as usize);
            write_volatile(slot, slot.read_volatile() + 1);
        }
        self.n += 1;
        self.sum += ticks;
    }

    /// In ticks: the start of the step the quantile falls in.
    fn q(&self, per_mille: u64) -> u64 {
        let want = self.n * per_mille / 1000;
        let mut seen = 0u64;
        for i in 0..BT {
            // SAFETY: as add.
            seen += unsafe { self.t.add(i as usize).read_volatile() } as u64;
            if seen > want {
                return i * self.step;
            }
        }
        BT * self.step
    }

    fn put(&self, name: &str) {
        let hz = info().timebase_hz;
        put("BENCH ");
        put(name);
        put(": n ");
        dec(self.n);
        if self.n == 0 {
            put("\n");
            return;
        }
        put(" mean ");
        // no overflow of the sum
        dec(self.sum / self.n * 1_000_000_000 / hz + self.sum % self.n * 1_000_000_000 / self.n / hz);
        put(" ns, p50 ");
        dec(self.q(500) * 1_000_000_000 / hz);
        put(" p90 ");
        dec(self.q(900) * 1_000_000_000 / hz);
        put(" p99 ");
        dec(self.q(990) * 1_000_000_000 / hz);
        put(" ns\n");
    }
}

const fn zeroed_state() -> XhUserState {
    XhUserState { x: [0; 31], sp: 0, pc: 0, pstate: 0, tsd: 0, fpsr: 0, fpcr: 0, v: [XhVreg { lo: 0, hi: 0 }; 32] }
}

/// A zeroed register state at `pc`.
fn state_at(st: *mut XhUserState, pc: u64) -> &'static mut XhUserState {
    zero(st, size_of::<XhUserState>());
    // SAFETY: a static state of the benchmark's, used by one vCPU at a time.
    let st = unsafe { &mut *st };
    st.pc = pc;
    st
}

fn code_words(pa: u64, words: &[u32]) -> *mut u32 {
    let code: *mut u32 = at(pa);
    for (i, &w) in words.iter().enumerate() {
        // SAFETY: a page of the benchmark's.
        unsafe { code.add(i).write_volatile(w) };
    }
    code
}

/// The upcalls of a benchmark: the interrupt path and a secondary vCPU's entry. Filled at run time (no fixups).
fn set_upcalls(cpu: *mut XhCpu, upcalls: &Racy<XhUpcalls>, entry: unsafe extern "C" fn(*mut XhCpu, u64)) {
    let u = upcalls.get();
    // SAFETY: vCPU 0 alone writes it, before the host reads it.
    unsafe {
        write_volatile(addr_of_mut!((*u).irq), Some(irq_upcall));
        write_volatile(addr_of_mut!((*u).cpu_entry), Some(entry));
    }
    host!(set_upcalls(cpu, u));
    set_irq_masked(cpu, 1);
    host!(timer_set(cpu, u64::MAX));
}

static BENCH_ST: Racy<XhUserState> = Racy::new(zeroed_state());

pub(crate) fn bench(scale: u32) {
    let c = c0();
    let (mut task, mut thread) = (0u64, 0u64);
    if host!(task_create(c, &mut task)) < 0 {
        put("BENCH skipped: no task layer\n");
        return;
    }
    let (code_pa, data_pa) = (page_alloc(), page_alloc());
    // 0x00: the null system call     0x40: a store to an unmapped page, then a system call     0x80: a loop
    let code: *mut u32 = at(code_pa);
    code_words(code_pa, &[0xd53b_e040, 0xd400_1001, 0x17ff_fffe]); // mrs x0, cntvct; svc #0x80; b 0
                                                                   // mrs x0; str x1, [x2]; mrs x3; svc; b 0x40
    code_words(code_pa + 0x40, &[0xd53b_e040, 0xf900_0041, 0xd53b_e043, 0xd400_1001, 0x17ff_fffc]);
    code_words(code_pa + 0x80, &[0x1400_0000]); // b .
    host!(text_fixup(c, code.cast(), 33 * 4));
    let (code_va, data_va) = (0x1_0000_0000u64, 0x2_0000_0000u64);
    host!(task_map(c, task, code_va, 0x4000, XH_PROT_READ | XH_PROT_EXEC, code_pa));
    if host!(thread_create(c, task, &mut thread)) != 0 {
        put("BENCH skipped: no thread\n");
        return;
    }
    let mut trap = XhTrap::default();
    set_irq_masked(c, 1);
    host!(timer_set(c, u64::MAX));
    take_irqs(c);
    let hz = info().timebase_hz;
    let scale = scale as u64;
    let mut bad = 0u64;

    // 1. The null system call: svc, trap, the kernel hands it straight back.
    let (mut up, mut down, mut round) = (Bhist::table(), Bhist::table(), Bhist::table());
    let st = state_at(BENCH_ST.get(), code_va);
    let mut last = 0u64;
    for i in 0..20_000 * scale {
        let before = now();
        if host!(thread_run(c, thread, st, &mut trap)) != 0 || trap.kind != XH_TRAP_SYSCALL {
            bad += 1;
            if trap.kind == XH_TRAP_INTERRUPT {
                take_irqs(c);
            }
            continue;
        }
        let t = now();
        if i != 0 && st.x[0] >= before && st.x[0] <= t {
            up.add(t - st.x[0]);
            down.add(st.x[0] - before);
        }
        if last != 0 {
            round.add(t - last);
        }
        last = t;
    }
    round.put("null syscall, round trip (kernel to kernel)");
    up.put("  svc -> the kernel has the trap");
    down.put("  the kernel releases -> user code runs");
    if bad != 0 {
        put("BENCH   (other traps: ");
        dec(bad);
        put(")\n");
    }

    // 2. A page fault the kernel resolves by mapping the page, then the page is unmapped again (svc).
    let (mut fault, mut map, mut unmap) = (Bhist::table(), Bhist::table(), Bhist::table());
    let st = state_at(BENCH_ST.get(), code_va + 0x40);
    st.x[2] = data_va;
    for _ in 0..5000 * scale {
        if host!(thread_run(c, thread, st, &mut trap)) != 0 || trap.kind != XH_TRAP_FAULT {
            bad += 1;
            if trap.kind == XH_TRAP_INTERRUPT {
                take_irqs(c);
            }
            continue;
        }
        let a = now();
        host!(task_map(c, task, data_va, 0x4000, XH_PROT_READ | XH_PROT_WRITE, data_pa));
        map.add(now() - a);
        if host!(thread_run(c, thread, st, &mut trap)) != 0 || trap.kind != XH_TRAP_SYSCALL {
            bad += 1;
            continue;
        }
        fault.add(st.x[3] - st.x[0]);
        let a = now();
        host!(task_unmap(c, task, data_va, 0x4000));
        unmap.add(now() - a);
    }
    fault.put("page fault filled by task_map, as user code sees it");
    map.put("  task_map (host call)");
    unmap.put("  task_unmap (host call; queued while the thread is stopped)");

    // 3. The interrupt of a running thread: the timer fires 50 us into a loop that never traps.
    let mut kick = Bhist::table();
    let st = state_at(BENCH_ST.get(), code_va + 0x80);
    for _ in 0..2000 * scale {
        let deadline = now() + hz / 20000;
        host!(timer_set(c, deadline));
        if host!(thread_run(c, thread, st, &mut trap)) != 0 || trap.kind != XH_TRAP_INTERRUPT {
            bad += 1;
            continue;
        }
        let t = now();
        if t >= deadline {
            kick.add(t - deadline);
        }
        take_irqs(c);
    }
    kick.put("timer interrupt of a running thread: deadline -> the kernel has the trap");
    host!(timer_set(c, u64::MAX));
    take_irqs(c);

    // 4. Free-page reporting (kernel 0058): ram_discard of one page and of 64 consecutive pages, and what a page
    // costs when it is used again, as XNU zero-fills a page it takes (16 KiB of 8-byte stores): resident, or given
    // back (the host's first touch of each 4 KiB part allocates and zeroes it).
    let (mut give1, mut give64, mut fill_res, mut fill_given) =
        (Bhist::table(), Bhist::table(), Bhist::table(), Bhist::table());
    let run = page_alloc();
    for _ in 1..64 {
        page_alloc();
    }
    let page = |p: u64| kva(run + p * 0x4000);
    let fill = |p: u64, step: usize, v: u64| {
        for k in (0..0x4000 / 8).step_by(step) {
            write64(page(p) + k * 8, v);
        }
    };
    for _ in 0..50 * scale {
        for p in 0..64 {
            let a = now();
            fill(p, 1, 0);
            fill_res.add(now() - a);
        }
        let a = now();
        if host!(ram_discard(c, run, 64 * 0x4000)) != 0 {
            bad += 1;
        }
        give64.add(now() - a);
        for p in 0..64 {
            let a = now();
            fill(p, 1, 0);
            fill_given.add(now() - a);
        }
        for p in 0..64 {
            let a = now();
            if host!(ram_discard(c, run + p * 0x4000, 0x4000)) != 0 {
                bad += 1;
            }
            give1.add(now() - a);
        }
        for p in 0..64 {
            fill(p, 512, 1); // all of each page resident again
        }
    }
    give1.put("ram_discard of one 16 KiB page (host call)");
    give64.put("ram_discard of 64 consecutive pages, 1 MiB (host call)");
    fill_res.put("zero-fill of a resident 16 KiB page");
    fill_given.put("zero-fill of a 16 KiB page given back (the host fills it again)");
    if bad != 0 {
        put("BENCH   (failed calls or other traps: ");
        dec(bad);
        put(")\n");
    }
    host!(thread_destroy(c, thread));
    host!(task_destroy(c, task));
    put("BENCH done\n");
}

// ---- the parallel test (boot argument testkernel-parallel; host-overhead 2026-11-15) ---------------------------------
// N threads of one task, one per vCPU, each the same busy work in user mode (a loop of `spin` iterations, then a
// system call, `rounds` times), all at once: the wall time for 1, 2, 4 and 8 threads. With nothing in xnuhost's path
// that serialises the vCPUs (a lock per task, the GIC's lock, the hand-off) the time stays flat while the host has a
// core per thread, and the speed-up is N. Each vCPU runs its own thread; vCPU 0 starts the others with an SGI and
// takes part.
struct Par {
    go: AtomicU32,
    n: AtomicU32,
    done: AtomicU32,
    rounds: AtomicU64,
    spin: AtomicU64,
    thread: [AtomicU64; 16],
    bad: AtomicU32,
    code_va: AtomicU64,
}

static PAR: Par = Par {
    go: AtomicU32::new(0),
    n: AtomicU32::new(0),
    done: AtomicU32::new(0),
    rounds: AtomicU64::new(0),
    spin: AtomicU64::new(0),
    thread: [const { AtomicU64::new(0) }; 16],
    bad: AtomicU32::new(0),
    code_va: AtomicU64::new(0),
};
static PAR_ST: Racy<[XhUserState; 16]> = Racy::new([const { zeroed_state() }; 16]);

fn par_work(cpu: *mut XhCpu) {
    let i = cpu_index(cpu) as usize;
    let mut trap = XhTrap::default();
    for _ in 0..PAR.rounds.load(Relaxed) {
        // SAFETY: each vCPU uses its own state (its index).
        let st = unsafe { &mut (*PAR_ST.get())[i] };
        st.pc = PAR.code_va.load(Relaxed);
        st.x[1] = PAR.spin.load(Relaxed);
        let rc = run_thread(cpu, PAR.thread[i].load(Relaxed), st, &mut trap, take_irqs);
        if rc != 0 || trap.kind != XH_TRAP_SYSCALL {
            PAR.bad.fetch_add(1, Relaxed);
        }
    }
}

extern "C" fn par_entry(cpu: *mut XhCpu, _arg: u64) {
    host!(irq_enable(cpu, 6, 1));
    let mut seen = 0;
    loop {
        let go = PAR.go.load(Acquire);
        if go != seen {
            seen = go;
            if cpu_index(cpu) < PAR.n.load(Relaxed) {
                par_work(cpu);
                PAR.done.fetch_add(1, AcqRel);
            }
            continue;
        }
        host!(cpu_idle(cpu));
        if irq_pending(cpu) {
            take_irqs(cpu);
        }
    }
}

/// One round on threads 0..n: the others released by an SGI, vCPU 0's own part, then the wait for them.
fn par_round(cpu: *mut XhCpu, n: u32) {
    PAR.n.store(n, Relaxed);
    PAR.done.store(0, Relaxed);
    PAR.go.fetch_add(1, AcqRel);
    for i in 1..n {
        host!(ipi(cpu, i, 6));
    }
    par_work(cpu);
    while PAR.done.load(Acquire) < n - 1 {
        cpu_yield();
    }
}

static PAR_UPCALLS: Racy<XhUpcalls> = Racy::new(XhUpcalls { irq: None, exception: None, cpu_entry: None });

pub(crate) fn parallel_bench(cpu: *mut XhCpu, scale: u32) {
    set_upcalls(cpu, &PAR_UPCALLS, par_entry);
    let ncpu = info().ncpu.min(16);
    let mut task = 0u64;
    if host!(task_create(cpu, &mut task)) < 0 {
        put("PARALLEL skipped: no task layer\n");
        return;
    }
    let code_pa = page_alloc();
    // subs x1, x1, #1; b.ne 0; svc #0x80; b .
    let code = code_words(code_pa, &[0xf100_0421, 0x54ff_ffe1, 0xd400_1001, 0x1400_0000]);
    host!(text_fixup(cpu, code.cast(), 4 * 4));
    let code_va = 0x1_0000_0000u64;
    PAR.code_va.store(code_va, Relaxed);
    host!(task_map(cpu, task, code_va, 0x4000, XH_PROT_READ | XH_PROT_EXEC, code_pa));
    for i in 0..ncpu as usize {
        if host!(thread_create(cpu, task, PAR.thread[i].as_ptr())) != 0 {
            put("PARALLEL skipped: no thread\n");
            return;
        }
    }
    for i in 1..ncpu {
        host!(cpu_start(cpu, i, 0, 0));
    }
    PAR.spin.store(100_000, Relaxed);
    PAR.rounds.store(100 * scale as u64, Relaxed);
    let hz = info().timebase_hz;
    put("PARALLEL: ");
    dec(ncpu as u64);
    put(" vCPUs; each thread ");
    dec(PAR.rounds.load(Relaxed));
    put(" rounds of ");
    dec(PAR.spin.load(Relaxed));
    put(" iterations and a system call\n");
    // One untimed round on every vCPU first: each thread's first run (its Linux thread placed, its pages mapped;
    // under qemu the code translated) is not part of anyone's time.
    par_round(cpu, ncpu);
    let (mut n, mut one) = (1u32, 0u64);
    while n <= ncpu {
        let start = now();
        par_round(cpu, n);
        let took = now() - start;
        if n == 1 {
            one = took;
        }
        let us = took * 1_000_000 / hz;
        let x100 = (one * n as u64 * 100).checked_div(took).unwrap_or(0);
        put("PARALLEL ");
        dec(n as u64);
        put(" threads: ");
        dec(us / 1000);
        put(" ms, speed-up ");
        dec(x100 / 100);
        put(".");
        if x100 % 100 < 10 {
            put("0");
        }
        dec(x100 % 100);
        put(" (ideal ");
        dec(n as u64);
        put(")\n");
        // 1, 2, 4, then all of them (a count that is no power of two too). The C took n to ncpu / 2 and doubled it,
        // which for an odd count gave ncpu - 1 again and again: it never ended.
        n = if n < ncpu && n * 2 > ncpu { ncpu } else { n * 2 };
    }
    let bad = PAR.bad.load(Relaxed);
    if bad != 0 {
        put("PARALLEL   (failed runs or other traps: ");
        dec(bad as u64);
        put(")\n");
    }
    put("PARALLEL done\n");
}

// ---- the Mach RPC round trip (boot argument testkernel-rpc[=SCALE], testkernel-rpc-mode=N; `make rpc-arm64`) -------
// fix-mach-rpc, 2026-11-15. The trip of a synchronous Mach message between two iOS processes, with XNU's own IPC code
// left out: a client thread of task A on vCPU 0 and a server thread of task B on vCPU 1, each a loop of `mrs x0,
// cntvct; svc`. A's svc is the send-and-wait-for-the-reply: vCPU 0 posts the request and sends vCPU 1 an IPI, then
// idles (the client blocks in its receive). vCPU 1, idle (the server blocked in its receive), wakes, takes the
// interrupt and releases B, whose svc is the reply-and-wait-for-the-next: vCPU 1 posts the reply, sends the IPI back
// and idles; vCPU 0 wakes, takes it and releases A. So one round trip is what XNU's is under xnuhost minus its IPC
// code: two user stays (four hand-offs), two IPIs, two wakes of an idle vCPU, two interrupts taken. The round trip is
// A's (its stamp at one svc to its stamp at the next). The mode is how the kernel drives the interrupt controller:
//   0  ArmCpu's way (what the hosted kernel did before 0081): the IPI is a store to GICD_ISPENDR (a trapped MMIO
//      store: a SIGSEGV on the sender's thread), the interrupt is ICC_IAR1_EL1, a store to GICD_ICPENDR, ICC_EOIR1_EL1
//      and ICC_IAR1_EL1 again (three trapped system registers and one trapped store: four signals on the receiver's);
//   1  host calls (0081): ipi; irq_take, irq_eoi, irq_take;
//   2  one vCPU: vCPU 0 releases B itself, no IPI and no wake (the floor: what a direct hand-off of the CPU would
//      cost).
// xnuhost's perf report at the stop says the futex sleeps and wakes, the signals (`emulated:` lines) of the mode.
struct Rpc {
    req: AtomicU64,
    rep: AtomicU64,
    a: AtomicU64,
    b: AtomicU64,
    mode: AtomicU32,
    stop: AtomicU32,
    gicd: AtomicU64,
    sent_at: AtomicU64,
}

static RPC: Rpc = Rpc {
    req: AtomicU64::new(0),
    rep: AtomicU64::new(0),
    a: AtomicU64::new(0),
    b: AtomicU64::new(0),
    mode: AtomicU32::new(0),
    stop: AtomicU32::new(0),
    gicd: AtomicU64::new(0),
    sent_at: AtomicU64::new(0),
};
static RPC_SA: Racy<XhUserState> = Racy::new(zeroed_state());
static RPC_SB: Racy<XhUserState> = Racy::new(zeroed_state());
static RPC_WAKE: Racy<Bhist> = Racy::new(Bhist::new());

fn rpc_wake() -> &'static mut Bhist {
    // SAFETY: the two vCPUs add to it in turn (the request and the reply hand it over), as the C program's table.
    unsafe { &mut *RPC_WAKE.get() }
}

/// A store to a GIC distributor register (a trapped MMIO store).
fn gicd_write(offset: u64, value: u32) {
    // SAFETY: the distributor's page, which io_map gave.
    unsafe { write_volatile((RPC.gicd.load(Relaxed) + offset) as *mut u32, value) };
}

fn rpc_send(cpu: *mut XhCpu, target: u32) {
    if RPC.mode.load(Relaxed) == 0 {
        let id = (XH_IPI_SPI_FIRST + target) as u64;
        // SAFETY: a barrier.
        unsafe { asm!("dsb ishst", options(nostack)) };
        gicd_write(0x200 + (id / 32) * 4, 1 << (id % 32));
    } else {
        host!(ipi(cpu, target, 7));
    }
}

/// The interrupt as each mode takes it; returns once nothing is pending.
fn rpc_irq(cpu: *mut XhCpu) {
    if RPC.mode.load(Relaxed) == 0 {
        loop {
            let id: u64;
            // SAFETY: ICC_IAR1_EL1, which the host emulates.
            unsafe { asm!("mrs {}, S3_0_C12_C12_0", out(reg) id, options(nostack)) };
            if id & 0xff_ffff >= 1020 {
                break;
            }
            if id >= XH_IPI_SPI_FIRST as u64 {
                gicd_write(0x280 + (id / 32) * 4, 1 << (id % 32)); // GICD_ICPENDR
            }
            // SAFETY: ICC_EOIR1_EL1, which the host emulates.
            unsafe { asm!("msr S3_0_C12_C12_1, {}", in(reg) id, options(nostack)) };
        }
        return;
    }
    loop {
        let mut id = host!(irq_take(cpu));
        if id == XH_INTID_NONE {
            break;
        }
        if id & XH_INTID_UNTAKEN != 0 {
            id = host!(irq_ack(cpu));
            if id == XH_INTID_NONE {
                break;
            }
        }
        host!(irq_eoi(cpu, id));
    }
}

extern "C" fn rpc_server(cpu: *mut XhCpu, _arg: u64) {
    host!(irq_enable(cpu, 7, 1));
    host!(irq_enable(cpu, XH_IPI_SPI_FIRST + cpu_index(cpu), 1));
    let mut served = 0;
    let mut trap = XhTrap::default();
    loop {
        host!(cpu_idle(cpu));
        let woke = now();
        if irq_pending(cpu) {
            rpc_irq(cpu);
        }
        if RPC.stop.load(Relaxed) != 0 {
            continue;
        }
        let req = RPC.req.load(Acquire);
        if req == served {
            continue;
        }
        let sent_at = RPC.sent_at.load(Relaxed);
        if sent_at != 0 && woke >= sent_at {
            rpc_wake().add(woke - sent_at);
        }
        served = req;
        run_thread(cpu, RPC.b.load(Relaxed), RPC_SB.get(), &mut trap, rpc_irq);
        RPC.sent_at.store(now(), Relaxed);
        RPC.rep.store(served, Release);
        rpc_send(cpu, 0);
    }
}

static RPC_UPCALLS: Racy<XhUpcalls> = Racy::new(XhUpcalls { irq: None, exception: None, cpu_entry: None });

pub(crate) fn rpc_bench(cpu: *mut XhCpu, scale: u32, mode: u32) {
    set_upcalls(cpu, &RPC_UPCALLS, rpc_server);
    if mode == 1 && (crate::h().size as usize) < offset_of!(XhVector, irq_take) + 8 {
        put("RPC skipped: no irq_take in this xnuhost\n");
        return;
    }
    RPC.mode.store(mode, Relaxed);
    RPC.gicd.store(host!(io_map(cpu, 0x0800_0000, 0x10000)), Relaxed);
    let (mut ta, mut tb) = (0u64, 0u64);
    if host!(task_create(cpu, &mut ta)) < 0 || host!(task_create(cpu, &mut tb)) < 0 {
        put("RPC skipped: no task layer\n");
        return;
    }
    let code_pa = page_alloc();
    // mrs x0, cntvct_el0; svc #0x80; b 0
    let code = code_words(code_pa, &[0xd53b_e040, 0xd400_1001, 0x17ff_fffe]);
    host!(text_fixup(cpu, code.cast(), 3 * 4));
    let code_va = 0x1_0000_0000u64;
    host!(task_map(cpu, ta, code_va, 0x4000, XH_PROT_READ | XH_PROT_EXEC, code_pa));
    host!(task_map(cpu, tb, code_va, 0x4000, XH_PROT_READ | XH_PROT_EXEC, code_pa));
    if host!(thread_create(cpu, ta, RPC.a.as_ptr())) != 0 || host!(thread_create(cpu, tb, RPC.b.as_ptr())) != 0 {
        put("RPC skipped: no thread\n");
        return;
    }
    let sa = state_at(RPC_SA.get(), code_va);
    state_at(RPC_SB.get(), code_va);
    host!(irq_enable(cpu, 7, 1));
    host!(irq_enable(cpu, XH_IPI_SPI_FIRST + cpu_index(cpu), 1));
    let mut round = Bhist::table();
    rpc_wake().init();
    // 4096 steps of 64 ticks: about 11 ms (qemu-user and a loaded host go past 168 us)
    round.step *= 64;
    rpc_wake().step *= 64;
    let hz = info().timebase_hz;
    if mode != 2 {
        host!(cpu_start(cpu, 1, 0, 0));
        let t0 = now();
        while now() - t0 < hz / 10 {
            cpu_yield(); // vCPU 1 up and idle
        }
    }
    // (no table of strings: this program has no fixups, a pointer in data holds its link address)
    put("RPC: mode ");
    dec(mode as u64);
    put(", ");
    put(match mode {
        0 => "ArmCpu's way (trapped GIC registers and stores)\n",
        1 => "host calls (0081)\n",
        _ => "one vCPU, no IPI (the floor)\n",
    });
    let n = 4000 * scale as u64;
    let (mut last, mut bad) = (0u64, 0u64);
    let mut trap = XhTrap::default();
    for i in 0..n + 1 {
        let rc = run_thread(cpu, RPC.a.load(Relaxed), sa, &mut trap, rpc_irq);
        if rc != 0 || (trap.kind != XH_TRAP_SYSCALL && trap.kind != XH_TRAP_ILLEGAL) {
            bad += 1;
            continue;
        }
        if last != 0 && sa.x[0] > last && i > 1 {
            round.add(sa.x[0] - last); // the first trip is not timed
        }
        last = sa.x[0];
        if i == n {
            break;
        }
        if mode == 2 {
            run_thread(cpu, RPC.b.load(Relaxed), RPC_SB.get(), &mut trap, rpc_irq);
            continue;
        }
        let want = i + 1;
        RPC.sent_at.store(now(), Relaxed);
        RPC.req.store(want, Release);
        rpc_send(cpu, 1);
        loop {
            host!(cpu_idle(cpu));
            let woke = now();
            if irq_pending(cpu) {
                rpc_irq(cpu);
            }
            if RPC.rep.load(Acquire) == want {
                let sent_at = RPC.sent_at.load(Relaxed);
                if woke >= sent_at {
                    rpc_wake().add(woke - sent_at);
                }
                break;
            }
        }
    }
    RPC.stop.store(1, Relaxed);
    round.put("rpc round trip, client svc to client resumed");
    rpc_wake().put("  IPI sent -> the idle vCPU out of cpu_idle");
    if bad != 0 {
        put("RPC   (failed runs or other traps: ");
        dec(bad);
        put(")\n");
    }
    put("RPC done\n");
}
