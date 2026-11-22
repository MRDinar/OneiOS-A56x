// SPDX-License-Identifier: MIT
//! iOS tasks and threads through the host calls: the trap kinds, the deferred maps and unmaps (task.rs), the unmap
//! race of two vCPUs, the JIT's aliased pages.

use crate::{
    at, c0, check, cpu_yield, hex, host, info, irq_pending, kva, now, page_alloc, put, read64, run_thread,
    set_irq_masked, take_irqs, write64, zero, Racy, TIMER_FIRES,
};
use core::mem::size_of;
use core::sync::atomic::{AtomicU32, AtomicU64, Ordering::*};
use xnuhost_abi::*;

const fn zeroed_state() -> XhUserState {
    XhUserState { x: [0; 31], sp: 0, pc: 0, pstate: 0, tsd: 0, fpsr: 0, fpcr: 0, v: [XhVreg { lo: 0, hi: 0 }; 32] }
}

/// A zeroed register state at `pc`.
fn state_at(st: *mut XhUserState, pc: u64) -> &'static mut XhUserState {
    zero(st, size_of::<XhUserState>());
    // SAFETY: a static state of the test's, used by one vCPU at a time (handed over by the worker's atomics).
    let st = unsafe { &mut *st };
    st.pc = pc;
    st
}

fn code_words(pa: u64, words: &[(usize, u32)]) -> *mut u32 {
    let code: *mut u32 = at(pa);
    for &(i, w) in words {
        // SAFETY: a page of the test's.
        unsafe { code.add(i).write_volatile(w) };
    }
    code
}

/// vCPU 1's part of test_unmap_race: while armed it spins, and each time `go` moves it runs worker.thread once
/// (from WORKER_ST) and reports the trap; otherwise it idles (SGI 6 wakes it to look again).
struct Worker {
    thread: AtomicU64,
    armed: AtomicU32,
    go: AtomicU32,
    done: AtomicU32,
    kind: AtomicU32,
    far: AtomicU64,
}

static WORKER: Worker = Worker {
    thread: AtomicU64::new(0),
    armed: AtomicU32::new(0),
    go: AtomicU32::new(0),
    done: AtomicU32::new(0),
    kind: AtomicU32::new(0),
    far: AtomicU64::new(0),
};
static WORKER_ST: Racy<XhUserState> = Racy::new(zeroed_state());

pub(crate) fn worker_loop(cpu: *mut XhCpu) -> ! {
    loop {
        if WORKER.armed.load(Relaxed) == 0 {
            host!(cpu_idle(cpu));
            if irq_pending(cpu) {
                take_irqs(cpu);
            }
            continue;
        }
        let go = WORKER.go.load(Acquire);
        if go == WORKER.done.load(Relaxed) {
            cpu_yield();
            continue;
        }
        let mut trap = XhTrap::default();
        let rc = run_thread(cpu, WORKER.thread.load(Relaxed), WORKER_ST.get(), &mut trap, take_irqs);
        WORKER.kind.store(if rc != 0 { 0xffff_ffff } else { trap.kind }, Relaxed);
        WORKER.far.store(trap.far, Relaxed);
        WORKER.done.store(go, Release);
    }
}

static LOAD_ST: Racy<XhUserState> = Racy::new(zeroed_state());

/// The deferred unmaps (task.rs, task.unmaps_out) with a loading thread: `ldr x0, [x1]; brk` at code_va + 32. The
/// trap kind (-1 if the run failed), and x0 into `value`.
fn load_once(cpu: *mut XhCpu, thread: u64, code_va: u64, va: u64, trap: &mut XhTrap, value: Option<&mut u64>) -> i64 {
    let ls = state_at(LOAD_ST.get(), code_va + 32);
    ls.x[1] = va;
    let rc = run_thread(cpu, thread, ls, trap, take_irqs);
    if let Some(v) = value {
        *v = ls.x[0];
    }
    if rc == 0 {
        trap.kind as i64
    } else {
        -1
    }
}

const BRK: i64 = XH_TRAP_BRK as i64;
const FAULT: i64 = XH_TRAP_FAULT as i64;

fn test_unmap_queue(task: u64, thread: u64, code_va: u64, data_pa: u64) {
    let c = c0();
    let mut trap = XhTrap::default();
    let mut got = 0u64;
    let (x_va, fill_va) = (0x4_0000_0000u64, 0x5_0000_0000u64);
    let other_pa = page_alloc();
    write64(kva(data_pa), 0x1111);
    write64(kva(other_pa), 0x2222);
    // In the queue's order: an unmap, then a map of the same address to another page.
    check(
        host!(task_map(c, task, x_va, 0x4000, XH_PROT_READ, data_pa)) == 0
            && load_once(c, thread, code_va, x_va, &mut trap, Some(&mut got)) == BRK
            && got == 0x1111,
        "deferred unmaps: a page mapped and loaded",
    );
    check(
        host!(task_unmap(c, task, x_va, 0x4000)) == 0
            && host!(task_map(c, task, x_va, 0x4000, XH_PROT_READ, other_pa)) == 0
            && load_once(c, thread, code_va, x_va, &mut trap, Some(&mut got)) == BRK
            && got == 0x2222,
        "an unmap and a new map of the same address, queued: the thread sees the new page",
    );
    // A full queue (256): the next unmap is ordered and takes the queue with it; the last one is queued again.
    let mut ok = true;
    for i in 0..300u64 {
        ok &= host!(task_unmap(c, task, fill_va + i * 0x4000, 0x4000)) == 0;
    }
    check(
        ok && host!(task_unmap(c, task, x_va, 0x4000)) == 0
            && load_once(c, thread, code_va, x_va, &mut trap, None) == FAULT
            && trap.far == x_va,
        "301 unmaps while the thread is stopped (more than the queue holds): the last is in force",
    );
    // task_read after a queued unmap: the read must not see the old page (it fails; the kernel would fault it in).
    let mut back = 0u64;
    host!(task_map(c, task, x_va, 0x4000, XH_PROT_READ, data_pa));
    let read = host!(task_read(c, task, x_va, (&mut back as *mut u64).cast(), 8));
    if read == -38 {
        put("SKIP task_read after queued maps and unmaps: this host has no process_vm_readv\n");
        host!(task_unmap(c, task, x_va, 0x4000));
    } else {
        check(
            read == 8
                && back == 0x1111
                && host!(task_unmap(c, task, x_va, 0x4000)) == 0
                && host!(task_read(c, task, x_va, (&mut back as *mut u64).cast(), 8)) < 0,
            "task_read after a queued map reads the page, after a queued unmap it fails",
        );
    }
    // ram_discard (the kernel's free-page reporting, 0058) and a task's mapping of the page. One page in every
    // mapping: given back, the task reads zero; written again by the kernel, the task reads the new value.
    let d_va = 0x6_0000_0000u64;
    let d_pa = page_alloc();
    write64(kva(d_pa), 0x3333);
    if info().features & XH_FEATURE_RAM_FIXED != 0 {
        put("SKIP ram_discard with a task's mapping: the RAM is held fixed (ram-giveback=0); check-arm64's first boot runs it\n");
        return;
    }
    check(
        host!(task_map(c, task, d_va, 0x4000, XH_PROT_READ, d_pa)) == 0
            && load_once(c, thread, code_va, d_va, &mut trap, Some(&mut got)) == BRK
            && got == 0x3333
            && host!(ram_discard(c, d_pa, 0x4000)) == 0
            && load_once(c, thread, code_va, d_va, &mut trap, Some(&mut got)) == BRK
            && got == 0
            && {
                write64(kva(d_pa), 0x4444);
                load_once(c, thread, code_va, d_va, &mut trap, Some(&mut got)) == BRK
            }
            && got == 0x4444,
        "ram_discard of a page a task maps: the task reads zero, then what the kernel writes into the page again",
    );
    // The page's unmap queued (the thread is stopped), the page given back and used again by the kernel before the
    // task runs: the task's next run carries the unmap out first and faults; it never sees the page's new data.
    check(
        host!(task_unmap(c, task, d_va, 0x4000)) == 0
            && host!(ram_discard(c, d_pa, 0x4000)) == 0
            && {
                write64(kva(d_pa), 0x5555);
                load_once(c, thread, code_va, d_va, &mut trap, None) == FAULT
            }
            && trap.far == d_va,
        "ram_discard of a page whose unmap is queued: the task faults at its next run, the page's new data is not seen",
    );
}

/// Two threads of one task on two vCPUs. Thread A is stopped with the page at x_va mapped in its process; the kernel
/// queues 128 maps and then the unmap of x_va (no thread runs); then vCPU 1 releases thread B while vCPU 0 releases
/// thread A (round 0) or gives the task an order (round 1), so one of them takes the queue. Whoever does not must
/// wait until the unmap is done: both threads' loads of x_va fault, every time.
fn test_unmap_race(task: u64, a: u64, code_va: u64, data_pa: u64) {
    let c = c0();
    let mut b = 0u64;
    if host!(thread_create(c, task, &mut b)) != 0 {
        check(false, "unmap race: a second thread");
        return;
    }
    let (x_va, fill_va) = (0x4_0000_0000u64, 0x6_0000_0000u64);
    let mut trap = XhTrap::default();
    let (rounds, mut bad_a, mut bad_b, mut setup) = (200u64, 0u64, 0u64, 0u64);
    WORKER.thread.store(b, Relaxed);
    WORKER.done.store(0, Relaxed);
    WORKER.go.store(0, Relaxed);
    WORKER.armed.store(1, Release);
    host!(ipi(c, 1, 6));
    for i in 0..rounds {
        if host!(task_map(c, task, x_va, 0x4000, XH_PROT_READ, data_pa)) != 0
            || load_once(c, a, code_va, x_va, &mut trap, None) != BRK
        {
            setup += 1;
            continue;
        }
        for j in 0..128u64 {
            host!(task_map(c, task, fill_va + j * 0x4000, 0x4000, XH_PROT_READ, data_pa));
        }
        host!(task_unmap(c, task, x_va, 0x4000));
        state_at(WORKER_ST.get(), code_va + 32).x[1] = x_va;
        let go = WORKER.go.load(Relaxed) + 1;
        WORKER.go.store(go, Release);
        if i & 1 != 0 {
            host!(task_protect(c, task, code_va, 0x4000, XH_PROT_READ | XH_PROT_EXEC));
        } else if load_once(c, a, code_va, x_va, &mut trap, None) != FAULT || trap.far != x_va {
            bad_a += 1;
        }
        let start = now();
        while WORKER.done.load(Acquire) != go && now() - start < info().timebase_hz * 20 {
            cpu_yield();
        }
        let done = WORKER.done.load(Relaxed);
        if done != go || WORKER.kind.load(Relaxed) != XH_TRAP_FAULT || WORKER.far.load(Relaxed) != x_va {
            bad_b += 1;
        }
        if done != go {
            break;
        }
    }
    WORKER.armed.store(0, Release);
    if bad_a != 0 || bad_b != 0 || setup != 0 {
        put("  unmap race: rounds, bad A, bad B, setup: ");
        for (k, v) in [rounds, bad_a, bad_b, setup].into_iter().enumerate() {
            if k != 0 {
                put(" ");
            }
            hex(v);
        }
        put("\n");
    }
    check(
        setup == 0 && bad_a == 0 && bad_b == 0,
        "unmap race: 200 rounds, the thread that did not carry a queued unmap never sees the old page",
    );
    check(host!(thread_destroy(c, b)) == 0, "unmap race: the second thread destroyed");
}

static JIT_ST: Racy<XhUserState> = Racy::new(zeroed_state());

/// WebKit's JIT (MAP_JIT) at the host-call level: a page is writable through one mapping and executable through
/// another of the same physical page (the "separated W^X heaps" WebKit uses on a host with no APRR/SPRR: real hardware
/// here has none). The thread writes an instruction sequence through the read-write alias, does the i-cache
/// maintenance an arm64 JIT must, and branches to the read-execute alias. The page is executed as it was written: the
/// two aliases share the physical page (two task_map mmaps of one RAM offset), and a store through the writable one is
/// seen by an instruction fetch through the executable one. 0067 (fault 10): the hosted layer carries no per-thread
/// W^X (one Linux process cannot give two threads different permissions of one page), so WebKit must not take the
/// per-thread path; this checks the aliasing path it does take.
///
/// JSC's first JIT thunk (JSC::LLInt::createJSGateThunk -> LinkBuffer::finalizeCodeWithoutDisassemblyImpl ->
/// sys_icache_invalidate): code written through the RW alias of a MAP_JIT page, then `dc cvau` + `dsb ish` + `ic
/// ivau` + `dsb ish` + `isb` on the **RX** alias, then a branch to it. resident = true: the RX alias is mapped first.
/// resident = false: it is not (as on the phone, where the hosted pmap maps a page into the task only when it
/// faults), so the alias's first access is the dc cvau. On arm64 hardware that faults as a data abort with ISS.CM and
/// WnR set: the stub must say XH_PROT_READ (the kernel then maps the RX page and the dc cvau is retried). Taken as a
/// write, iOS failed it on the RX entry (phone 2026-11-14: every WebContent process, "byte write Permission fault").
/// qemu-user runs dc cvau / ic ivau as no-ops that never fault, so under qemu the first access is the branch (an
/// exec fault).
fn test_jit_icache(resident: bool) {
    let c = c0();
    let what = if resident { "jit icache (RX alias resident)" } else { "jit icache (RX alias never touched)" };
    let (mut task, mut thread) = (0u64, 0u64);
    if host!(task_create(c, &mut task)) < 0 {
        put("SKIP ");
        put(what);
        put(": task_create\n");
        return;
    }
    let (code_pa, jit_pa) = (page_alloc(), page_alloc());
    let code = code_words(
        code_pa,
        &[
            (0, 0x5281_6c05),  // mov w5,#0xb60
            (1, 0x72ba_5005),  // movk w5,#0xd280,lsl16
            (2, 0xb900_0025),  // str w5,[x1]
            (3, 0x5280_0405),  // mov w5,#0x20
            (4, 0x72ba_8405),  // movk w5,#0xd420,lsl16
            (5, 0xb900_0425),  // str w5,[x1,#4]
            (6, 0xd50b_7b22),  // dc cvau,x2
            (7, 0xd503_3b9f),  // dsb ish
            (8, 0xd50b_7522),  // ic ivau,x2
            (9, 0xd503_3b9f),  // dsb ish
            (10, 0xd503_3fdf), // isb
            (11, 0xd61f_0040), // br x2   (x2: "mov x0,#0x5b; brk #1")
        ],
    );
    host!(text_fixup(c, code.cast(), 12 * 4));
    let (code_va, exec_va, write_va) = (0x1_1000_0000u64, 0x1_9000_0000u64, 0x1_d000_0000u64);
    check(
        host!(task_map(c, task, write_va, 0x4000, XH_PROT_READ | XH_PROT_WRITE | XH_PROT_EXEC, jit_pa)) == 0
            && host!(task_map(c, task, code_va, 0x4000, XH_PROT_READ | XH_PROT_EXEC, code_pa)) == 0
            && (!resident || host!(task_map(c, task, exec_va, 0x4000, XH_PROT_READ | XH_PROT_EXEC, jit_pa)) == 0),
        what,
    );
    if host!(thread_create(c, task, &mut thread)) != 0 {
        check(false, "jit icache: thread_create");
        host!(task_destroy(c, task));
        return;
    }
    let st = state_at(JIT_ST.get(), code_va);
    st.x[1] = write_va;
    st.x[2] = exec_va;
    let mut trap = XhTrap::default();
    let (mut k, mut faults, mut cm_faults, mut cm_as_write, mut exec_faults, mut other) = (-1i64, 0u64, 0, 0, 0, 0);
    for _ in 0..8 {
        let r = run_thread(c, thread, st, &mut trap, take_irqs);
        k = if r == 0 { trap.kind as i64 } else { -1 };
        if k != FAULT {
            break;
        }
        faults += 1;
        if trap.far & !0x3fff != exec_va {
            other += 1;
            break;
        }
        if st.pc == code_va + 6 * 4 || st.pc == code_va + 8 * 4 {
            // the cache maintenance on the RX alias faulted (arm64 hardware): a read, with ISS.CM in Linux's syndrome
            cm_faults += 1;
            if trap.access != XH_PROT_READ || (trap.esr != 0 && trap.esr & (1 << 8) == 0) {
                cm_as_write += 1;
            }
            put("  ");
            put(what);
            put(": cache maintenance at pc ");
            hex(st.pc);
            put(" faulted: access ");
            hex(trap.access as u64);
            put(" esr ");
            hex(trap.esr);
            put("\n");
        } else if st.pc == exec_va {
            exec_faults += 1;
        } else {
            other += 1;
            break;
        }
        // As the kernel does for a read or an execute fault on the RX entry: the page mapped read-execute, then
        // retried.
        if host!(task_map(c, task, exec_va, 0x4000, XH_PROT_READ | XH_PROT_EXEC, jit_pa)) != 0 {
            other += 1;
            break;
        }
    }
    let ran = k == BRK && st.x[0] == 0x5b && st.pc == exec_va + 4;
    if !ran || other != 0 {
        put("  ");
        put(what);
        put(": kind ");
        hex(k as u32 as u64);
        put(" x0 ");
        hex(st.x[0]);
        put(" pc ");
        hex(st.pc);
        put(" far ");
        hex(trap.far);
        put(" faults ");
        hex(faults);
        put("\n");
    }
    check(
        ran && other == 0,
        if resident {
            "jit icache: dc cvau / ic ivau on the resident RX alias, then the code runs (x0 = 0x5b)"
        } else {
            "jit icache: dc cvau / ic ivau as the RX alias's first access, then the code runs (x0 = 0x5b)"
        },
    );
    if resident {
        check(faults == 0, "jit icache: no fault on the resident RX alias");
    } else {
        check(
            cm_as_write == 0,
            "jit icache: a cache maintenance fault on the RX alias is a read (ISS.CM), not a write",
        );
        if cm_faults == 0 {
            put("NOTE jit icache: the cache maintenance did not fault (qemu-user runs dc cvau / ic ivau as no-ops); \
                 the RX alias's first fault was the branch\n");
        }
        check(
            cm_faults + exec_faults >= 1,
            "jit icache: the never-touched RX alias faulted once and was mapped read-execute",
        );
    }
    check(
        host!(thread_destroy(c, thread)) == 0 && host!(task_destroy(c, task)) == 0,
        "jit icache: thread_destroy, task_destroy",
    );
}

pub(crate) fn test_jit() {
    let c = c0();
    let (mut task, mut thread) = (0u64, 0u64);
    let rc = host!(task_create(c, &mut task));
    if rc < 0 {
        put("SKIP jit: task_create returned ");
        hex(rc as u64);
        put("\n");
        return;
    }
    // The writer stub (verified encodings): write "mov x0,#0x5a; brk #1" through x1 (the RW alias), clean and
    // invalidate the two code words for the exec alias in x2, then branch to it.
    let (jcode_pa, jit_pa) = (page_alloc(), page_alloc());
    let jc = code_words(
        jcode_pa,
        &[
            (0, 0x5281_6805),  // mov w5,#0xb40
            (1, 0x72ba_5005),  // movk w5,#0xd280,lsl16
            (2, 0xb900_0025),  // str w5,[x1]
            (3, 0x5280_0405),  // mov w5,#0x20
            (4, 0x72ba_8405),  // movk w5,#0xd420,lsl16
            (5, 0xb900_0425),  // str w5,[x1,#4]
            (6, 0xd50b_7b22),  // dc cvau,x2
            (7, 0x9100_1046),  // add x6,x2,#4
            (8, 0xd50b_7b26),  // dc cvau,x6
            (9, 0xd503_3b9f),  // dsb ish
            (10, 0xd50b_7522), // ic ivau,x2
            (11, 0xd50b_7526), // ic ivau,x6
            (12, 0xd503_3b9f), // dsb ish
            (13, 0xd503_3fdf), // isb
            (14, 0xd61f_0040), // br x2
        ],
    );
    host!(text_fixup(c, jc.cast(), 15 * 4));
    let (jcode_va, exec_va, write_va) = (0x1_0000_0000u64, 0x1_8000_0000u64, 0x1_c000_0000u64);
    // The JIT page in one task_map must be read, write and execute at once (MAP_JIT's W+X reservation: the host must
    // allow a W+X mapping of the RAM memfd).
    check(
        host!(task_map(c, task, write_va, 0x4000, XH_PROT_READ | XH_PROT_WRITE | XH_PROT_EXEC, jit_pa)) == 0,
        "jit: task_map of a W+X page (MAP_JIT)",
    );
    check(
        host!(task_map(c, task, exec_va, 0x4000, XH_PROT_READ | XH_PROT_EXEC, jit_pa)) == 0,
        "jit: the same physical page mapped read-execute at a second address (the W^X alias)",
    );
    check(
        host!(task_map(c, task, jcode_va, 0x4000, XH_PROT_READ | XH_PROT_EXEC, jcode_pa)) == 0,
        "jit: the writer stub mapped",
    );
    if host!(thread_create(c, task, &mut thread)) != 0 {
        check(false, "jit: thread_create");
        host!(task_destroy(c, task));
        return;
    }
    let st = state_at(JIT_ST.get(), jcode_va);
    st.x[1] = write_va;
    st.x[2] = exec_va;
    let mut trap = XhTrap::default();
    let r = run_thread(c, thread, st, &mut trap, take_irqs);
    let k = if r == 0 { trap.kind as i64 } else { -1 };
    let ran = k == BRK && st.x[0] == 0x5a && st.pc == exec_va + 4;
    let jit_word = || read64(kva(jit_pa)) as u32;
    if !ran {
        put("  jit: kind ");
        hex(k as u32 as u64);
        put(" x0 ");
        hex(st.x[0]);
        put(" pc ");
        hex(st.pc);
        put(" jit_pa[0] ");
        hex(jit_word() as u64);
        put("\n");
    }
    check(ran, "jit: code written through the RW alias runs through the RX alias (x0 = 0x5a at the brk)");
    // The kernel's own view of the page sees the thread's JIT store too (shared memory, as copyio would read it).
    check(
        jit_word() == 0xd280_0b40,
        "jit: the thread's store through the RW alias is in the kernel's view of the page",
    );
    check(host!(thread_destroy(c, thread)) == 0, "jit: thread_destroy");
    check(
        host!(task_unmap(c, task, exec_va, 0x4000)) == 0 && host!(task_unmap(c, task, write_va, 0x4000)) == 0,
        "jit: unmap the aliases",
    );
    check(host!(task_destroy(c, task)) == 0, "jit: task_destroy");
    test_jit_icache(true);
    test_jit_icache(false);
}

static TASK_ST: Racy<XhUserState> = Racy::new(zeroed_state());

pub(crate) fn test_tasks() {
    let c = c0();
    let (mut task, mut thread) = (0u64, 0u64);
    let rc = host!(task_create(c, &mut task));
    if rc < 0 {
        put("SKIP tasks: task_create returned ");
        hex(rc as u64);
        put("\n");
        return;
    }
    let trapped = info().features & XH_FEATURE_TASKS != 0;
    let (code_pa, data_pa) = (page_alloc(), page_alloc());
    let code = code_words(
        code_pa,
        &[
            (0, 0xd280_0540), // mov x0, #42
            (1, 0xd280_0030), // mov x16, #1
            (2, 0xd400_1001), // svc #0x80
            (3, 0xd2c0_0041), // mov x1, #0x200000000
            (4, 0xf900_0020), // str x0, [x1]
            (5, 0xd420_0020), // brk #1
            (6, 0x1400_0000), // b .
            (8, 0xf940_0020), // ldr x0, [x1]      (test_unmap_race)
            (9, 0xd420_0020), // brk #1
        ],
    );
    // As the kernel does with a page of signed text before it maps it: on a host with no seccomp the svc becomes the
    // undefined instruction thread_run reports as a system call.
    host!(text_fixup(c, code.cast(), 10 * 4));
    let (code_va, data_va) = (0x1_0000_0000u64, 0x2_0000_0000u64);
    check(
        host!(task_map(c, task, code_va, 0x4000, XH_PROT_READ | XH_PROT_EXEC, code_pa)) == 0,
        "task_map of a code page at 0x100000000",
    );
    check(
        host!(task_map(c, task, info().user_va_limit, 0x4000, XH_PROT_READ, code_pa)) < 0,
        "task_map refuses the stub's region",
    );
    check(host!(thread_create(c, task, &mut thread)) == 0, "thread_create");
    let st = state_at(TASK_ST.get(), if trapped { code_va } else { code_va + 12 });
    st.x[0] = 7;
    st.tsd = 0x123_4500;
    st.v[3].lo = 0x1122_3344_5566_7788;
    let mut trap = XhTrap::default();
    let run = |st: &mut XhUserState, trap: &mut XhTrap| host!(thread_run(c, thread, st, trap)) == 0;
    if trapped {
        check(
            run(st, &mut trap)
                && trap.kind == XH_TRAP_SYSCALL
                && trap.svc == 0x80
                && st.x[16] == 1
                && st.x[0] == 42
                && st.pc == code_va + 12,
            "svc #0x80 arrives as a system-call trap with x16 and x0 intact",
        );
        st.x[0] = 7; // "the system call's return value"
    } else {
        put("SKIP the svc trap: this host has no task layer\n");
    }
    check(
        run(st, &mut trap)
            && trap.kind == XH_TRAP_FAULT
            && trap.far == data_va
            && (trap.access == XH_PROT_WRITE || trap.access == 0)
            && st.pc == code_va + 16,
        "a store to an unmapped page arrives as a fault",
    );
    let kept = st.tsd == 0x123_4500 && st.v[3].lo == 0x1122_3344_5566_7788;
    if !kept {
        put("  tsd, v3: ");
        hex(st.tsd);
        put(" ");
        hex(st.v[3].lo);
        put("\n");
    }
    check(kept, "the thread register and a vector register came back unchanged");
    check(
        host!(task_map(c, task, data_va, 0x4000, XH_PROT_READ | XH_PROT_WRITE, data_pa)) == 0,
        "the kernel maps the page (pmap_enter)",
    );
    check(
        run(st, &mut trap) && trap.kind == XH_TRAP_BRK && st.pc == code_va + 20,
        "the store is retried, then brk traps",
    );
    // the store's fault came just before on this task: Linux's SIGTRAP carries that fault's syndrome as its own
    // (stub), and the kernel would have reported the brk as a data abort
    if trapped {
        check(
            trap.esr == 0 && trap.insn == 0xd420_0020,
            "brk after a fault: no syndrome of that fault, and the brk instruction for the kernel",
        );
    }
    check(read64(kva(data_pa)) == 7, "the task's store is visible in the kernel's view of the page (shared memory)");
    let value = 0xfeed_face_cafe_beefu64;
    let mut back = 0u64;
    let wrote = host!(task_write(c, task, data_va + 8, (&value as *const u64).cast(), 8));
    if wrote == -38 {
        put("SKIP task_write and task_read: this host has no process_vm_writev\n");
    } else {
        check(
            wrote == 8
                && host!(task_read(c, task, data_va + 8, (&mut back as *mut u64).cast(), 8)) == 8
                && back == value
                && read64(kva(data_pa) + 8) == value,
            "task_write and task_read (copyout, copyin)",
        );
    }
    check(host!(task_protect(c, task, data_va, 0x4000, XH_PROT_READ)) == 0, "task_protect");
    // task_map rides on the next release of a thread of the task (map-defer); an order given before that release
    // (here the protect above, below an unmap) must find it done and keep its place after it.
    st.pc = code_va + 16; // str x0, [x1]
    st.x[1] = data_va;
    check(
        run(st, &mut trap) && trap.kind == XH_TRAP_FAULT && trap.far == data_va && st.pc == code_va + 16,
        "a store to the page made read-only faults",
    );
    let other_va = 0x3_0000_0000u64;
    check(
        host!(task_map(c, task, other_va, 0x4000, XH_PROT_READ | XH_PROT_WRITE, data_pa)) == 0
            && host!(task_unmap(c, task, other_va, 0x4000)) == 0,
        "a map and an unmap of another page before the thread runs",
    );
    st.x[1] = other_va;
    check(
        run(st, &mut trap) && trap.kind == XH_TRAP_FAULT && trap.far == other_va,
        "the unmap came after the map: the store faults",
    );
    check(
        host!(task_map(c, task, other_va, 0x4000, XH_PROT_READ | XH_PROT_WRITE, data_pa)) == 0
            && run(st, &mut trap)
            && trap.kind == XH_TRAP_BRK
            && st.pc == code_va + 20
            && read64(kva(data_pa)) == st.x[0],
        "a map given before the run is in place when the thread runs",
    );
    // task_unmap while no thread of the task runs waits in the queue (task.rs): it must be in force when the thread
    // runs.
    check(host!(task_unmap(c, task, other_va, 0x4000)) == 0, "an unmap while the task's thread is stopped");
    st.pc = code_va + 16;
    st.x[1] = other_va;
    check(
        run(st, &mut trap) && trap.kind == XH_TRAP_FAULT && trap.far == other_va,
        "it is in force when the thread runs again: the store faults",
    );
    if trapped {
        test_unmap_queue(task, thread, code_va, data_pa);
        test_unmap_race(task, thread, code_va, data_pa);
    }
    // Preemption: a thread that never traps is stopped when the vCPU's timer fires.
    st.pc = code_va + 24;
    let fires = TIMER_FIRES.load(Relaxed);
    set_irq_masked(c, 1);
    host!(timer_set(c, now() + info().timebase_hz / 50));
    check(
        run(st, &mut trap) && trap.kind == XH_TRAP_INTERRUPT && st.pc == code_va + 24,
        "a looping thread is preempted by the timer",
    );
    take_irqs(c);
    check(TIMER_FIRES.load(Relaxed) == fires + 1, "and the timer interrupt is then taken by the kernel");
    check(host!(thread_destroy(c, thread)) == 0, "thread_destroy");
    check(host!(task_unmap(c, task, data_va, 0x4000)) == 0, "task_unmap");
    check(host!(task_destroy(c, task)) == 0, "task_destroy");
}
