// SPDX-License-Identifier: MIT
//! machine-smoke: the vCPU machinery alone, without a kernel image or the session (check-arm64.sh smoke). One vCPU
//! enters a few instructions of "kernel" (below, in this binary's text) that go through every seam of arch.rs and
//! cpu.rs once:
//!   1. a trapped `mrs x3, tpidr_el1` (SIGILL, sysreg emulation) reads the shadow the host set;
//!   2. console_write and wall_time host calls (the thunks: the host's TPIDR_EL0, x18 and stack);
//!   3. two byte stores to the UART in the I/O aperture (SIGSEGV, MMIO emulation): a "guest" line;
//!   4. set_upcalls, irq_enable(27), unmask, timer_set(1): the timer is due at once, so the thunk's return path
//!      delivers the interrupt (kctx.rs) to the upcall, which acks, cancels and EOIs it;
//!   5. timer_set(now + about 15 ms) and a spin: the timer's signal diverts the loop into the upcall (divert, the
//!      trampoline) and frame_resume (rt_sigreturn) puts the loop back;
//!   6. wfi for good (SIGILL, the idle loop).
//!
//! Prints "machine-smoke: passed" and exits 0, or what failed and exits 1. Times are qemu's under qemu-aarch64.

#[cfg(all(target_arch = "aarch64", any(target_os = "linux", target_os = "android")))]
mod smoke {
    use std::mem::offset_of;
    use std::sync::atomic::{AtomicU64, Ordering::*};
    use std::time::{Duration, Instant};
    use xnuhost::{cpu, gic, hostcalls, mem, uart};
    use xnuhost_abi::{XhCpu, XhVector, XH_INTID_VTIMER};

    /// What the kernel and the host share (u64 slots).
    #[repr(C)]
    struct Shared {
        tpidr_el1: AtomicU64,    // 0: what the trapped mrs read
        wall: AtomicU64,         // 8: wall_time
        uart: AtomicU64,         // 16: the UART's address (in)
        done: AtomicU64,         // 24: 1 when step 5 is over
        upcalls: [AtomicU64; 3], // 32: struct xh_upcalls (in)
        irqs: AtomicU64,         // 56: interrupt upcalls taken
        intid: AtomicU64,        // 64: the last acknowledged INTID
        set_upcalls: AtomicU64,  // 72: set_upcalls' result
        vector: AtomicU64,       // 80: the vector, for the upcall
    }

    core::arch::global_asm!(
        ".text",
        ".globl machine_smoke_kernel",
        ".p2align 2",
        "machine_smoke_kernel:", // x0 = &Shared, x1 = the vector, x2 = the vCPU
        "mov x19, x0",
        "mov x20, x1",
        "mov x21, x2",
        "str x19, [x21, #{kdata}]",
        "str x20, [x19, #80]",
        "mrs x3, tpidr_el1", // 1
        "str x3, [x19]",
        "ldr x9, [x20, #{console_write}]", // 2
        "mov x0, x21",
        "adr x1, 9f",
        "mov x2, #20",
        "blr x9",
        "ldr x9, [x20, #{wall_time}]",
        "mov x0, x21",
        "blr x9",
        "str x0, [x19, #8]",
        "ldr x4, [x19, #16]", // 3
        "mov w5, #0x55",
        "strb w5, [x4]",
        "mov w5, #10",
        "strb w5, [x4]",
        "ldr x9, [x20, #{set_upcalls}]", // 4
        "mov x0, x21",
        "add x1, x19, #32",
        "blr x9",
        "str x0, [x19, #72]",
        "ldr x9, [x20, #{irq_enable}]",
        "mov x0, x21",
        "mov w1, #{vtimer}",
        "mov w2, #1",
        "blr x9",
        "str wzr, [x21, #{masked}]",
        "ldr x9, [x20, #{timer_set}]",
        "mov x0, x21",
        "mov x1, #1",
        "blr x9",
        "mrs x3, cntvct_el0", // 5
        "mrs x4, cntfrq_el0",
        "lsr x4, x4, #6",
        "add x1, x3, x4",
        "ldr x9, [x20, #{timer_set}]",
        "mov x0, x21",
        "blr x9",
        "1:",
        "ldr x3, [x19, #56]",
        "cmp x3, #2",
        "b.lo 1b",
        "mov x3, #1",
        "str x3, [x19, #24]",
        "2:", // 6
        "wfi",
        "b 2b",
        "9:",
        ".ascii \"machine-smoke: hi\\n\\n\\n\"",
        ".p2align 2",
        // the interrupt upcall: x0 = the vCPU, x1 = the frame
        ".globl machine_smoke_irq",
        "machine_smoke_irq:",
        "stp x29, x30, [sp, #-32]!",
        "mov x29, sp",
        "stp x19, x20, [sp, #16]",
        "mov x19, x0",
        "ldr x20, [x19, #{kdata}]",
        "ldr x9, [x20, #80]",
        "ldr x9, [x9, #{irq_ack}]",
        "mov x0, x19",
        "blr x9",
        "str x0, [x20, #64]",
        "ldr x9, [x20, #80]",
        "ldr x9, [x9, #{timer_set}]",
        "mov x0, x19",
        "mov x1, #-1",
        "blr x9",
        "ldr x9, [x20, #80]",
        "ldr x9, [x9, #{irq_eoi}]",
        "mov x0, x19",
        "ldr x1, [x20, #64]",
        "blr x9",
        "ldr x3, [x20, #56]",
        "add x3, x3, #1",
        "str x3, [x20, #56]",
        "ldp x19, x20, [sp, #16]",
        "ldp x29, x30, [sp], #32",
        "ret",
        kdata = const offset_of!(XhCpu, kernel_data),
        masked = const offset_of!(XhCpu, irq_masked),
        console_write = const offset_of!(XhVector, console_write),
        wall_time = const offset_of!(XhVector, wall_time),
        set_upcalls = const offset_of!(XhVector, set_upcalls),
        irq_enable = const offset_of!(XhVector, irq_enable),
        irq_ack = const offset_of!(XhVector, irq_ack),
        irq_eoi = const offset_of!(XhVector, irq_eoi),
        timer_set = const offset_of!(XhVector, timer_set),
        vtimer = const XH_INTID_VTIMER,
    );
    extern "C" {
        fn machine_smoke_kernel();
        fn machine_smoke_irq();
    }

    pub fn run() -> Result<(), String> {
        const IO: u64 = 0x900_0000;
        hostcalls::update_info(|i| {
            i.abi = xnuhost_abi::XH_ABI_VERSION;
            i.ncpu = 1;
            // SAFETY: no arguments.
            i.host_page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as u32;
            i.timebase_hz = cpu::counter_hz();
        });
        gic::init(1, 0x800_0000, 0x80a_0000);
        mem::io(IO, 0x1_0000).map_err(|e| format!("the I/O aperture: {e}"))?;
        uart::init(IO);
        cpu::init(1).map_err(|e| format!("cpu::init: {e}"))?;
        let v = cpu::vcpu(0);
        v.cpu.tpidr_el1.store(0x1234_5678_9abc, Relaxed);
        let shared: &'static Shared = Box::leak(Box::new(Shared {
            tpidr_el1: AtomicU64::new(0),
            wall: AtomicU64::new(0),
            uart: AtomicU64::new(mem::state().io_host as u64),
            done: AtomicU64::new(0),
            upcalls: [AtomicU64::new(machine_smoke_irq as *const () as u64), AtomicU64::new(0), AtomicU64::new(0)],
            irqs: AtomicU64::new(0),
            intid: AtomicU64::new(0),
            set_upcalls: AtomicU64::new(u64::MAX),
            vector: AtomicU64::new(0),
        }));
        let vector = hostcalls::vector().ok_or("no vector on this target")?;
        cpu::start(
            0,
            machine_smoke_kernel as *const () as u64,
            shared as *const Shared as u64,
            vector as *const XhVector as u64,
            v.cpu.as_ptr() as u64,
        )
        .map_err(|e| format!("cpu::start: {e:?}"))?;
        let began = Instant::now();
        while shared.done.load(Acquire) == 0 && began.elapsed() < Duration::from_secs(20) {
            std::thread::sleep(Duration::from_millis(2));
        }
        let took = began.elapsed();
        let (tpidr, wall, irqs, intid, set) = (
            shared.tpidr_el1.load(Acquire),
            shared.wall.load(Acquire),
            shared.irqs.load(Acquire),
            shared.intid.load(Acquire),
            shared.set_upcalls.load(Acquire),
        );
        println!(
            "machine-smoke: tpidr_el1 0x{tpidr:x}, wall_time {wall}, set_upcalls {}, {irqs} interrupt upcalls (last INTID {intid}), \
             {} emulated, {} device accesses, {} diverted, {:.1} ms",
            set as i64,
            v.stat.sysreg.get(),
            v.stat.mmio.get(),
            v.stat.irqs.get(),
            took.as_secs_f64() * 1e3
        );
        let mut failed = Vec::new();
        if shared.done.load(Acquire) == 0 {
            failed.push("the kernel did not finish in 20 s");
        }
        if tpidr != 0x1234_5678_9abc {
            failed.push("the trapped mrs tpidr_el1");
        }
        if wall < 1_700_000_000 {
            failed.push("wall_time");
        }
        if set != 0 {
            failed.push("set_upcalls");
        }
        if irqs < 2 || intid != u64::from(XH_INTID_VTIMER) {
            failed.push("the timer's interrupt upcalls");
        }
        if v.stat.mmio.get() < 2 {
            failed.push("the UART stores (MMIO emulation)");
        }
        if v.stat.irqs.get() < 1 {
            failed.push("the timer signal's divert");
        }
        if failed.is_empty() {
            Ok(())
        } else {
            Err(failed.join(", "))
        }
    }
}

fn main() {
    #[cfg(all(target_arch = "aarch64", any(target_os = "linux", target_os = "android")))]
    match smoke::run() {
        Ok(()) => {
            println!("machine-smoke: passed");
            std::process::exit(0)
        }
        Err(e) => {
            println!("machine-smoke: FAILED: {e}");
            std::process::exit(1)
        }
    }
    #[cfg(not(all(target_arch = "aarch64", any(target_os = "linux", target_os = "android"))))]
    println!("machine-smoke: needs AArch64 Linux or Android (or qemu-aarch64): skipped");
}
