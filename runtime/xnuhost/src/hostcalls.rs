// SPDX-License-Identifier: MIT
//! Port of hostcalls.c: the host-call vector (xnuhost/hostcall.h). Each entry is an assembly thunk (arch.rs) that
//! switches from the kernel's context to the host's and calls the function of the same name here.
//!
//! [`info`] is `g_info`, the static facts the kernel reads through the vector; main fills it before the first vCPU
//! starts ([`update_info`]).

use std::cell::UnsafeCell;
use std::sync::Mutex;
use xnuhost_abi::XhInfo;

/// `g_info`: written by main before the vCPUs start, then only read (the kernel holds a pointer to it).
struct Info(UnsafeCell<XhInfo>);
// SAFETY: written only through update_info (under WRITE, before the kernel runs), read everywhere after.
unsafe impl Sync for Info {}

static INFO: Info = Info(UnsafeCell::new(XhInfo {
    abi: 0,
    ncpu: 0,
    host_page_size: 0,
    user_va_bits: 0,
    ram_phys: 0,
    ram_virt: 0,
    ram_size: 0,
    io_phys: 0,
    io_virt: 0,
    io_size: 0,
    user_va_min: 0,
    user_va_limit: 0,
    timebase_hz: 0,
    features: 0,
    video: [0; 6],
    kva_base: 0,
    kva_size: 0,
}));
static WRITE: Mutex<()> = Mutex::new(());

/// The host's facts as the kernel sees them.
pub fn info() -> &'static XhInfo {
    // SAFETY: see Info: no write happens while the kernel (or anything else) reads.
    unsafe { &*INFO.0.get() }
}

/// Changes `g_info`. Only before the first vCPU starts (main's set-up): the kernel and the vCPU threads read it
/// without a lock afterwards.
pub fn update_info(f: impl FnOnce(&mut XhInfo)) {
    let _w = WRITE.lock().unwrap_or_else(|e| e.into_inner());
    // SAFETY: one writer (WRITE), and by the rule above no reader yet.
    f(unsafe { &mut *INFO.0.get() })
}

pub use calls::*;

#[cfg(all(xh_machine, not(test)))]
mod calls {
    use super::info;
    use crate::cpu::{self, Vcpu, UPCALLS};
    use crate::{discard, gic, mem, session, task};
    use std::ffi::c_void;
    use std::sync::OnceLock;
    use xh_core::logi;
    use xh_core::regs::Regs;
    use xnuhost_abi::*;

    /// The vCPU of the block the kernel passed.
    fn v<'a>(cpu: *mut XhCpu) -> &'a Vcpu {
        // SAFETY: every host call's first argument is the calling vCPU's block (hostcall.h).
        unsafe { Vcpu::from_cpu(cpu) }
    }

    fn index(cpu: *mut XhCpu) -> usize {
        v(cpu).index()
    }

    pub(crate) unsafe extern "C" fn console_write(_cpu: *mut XhCpu, text: *const u8, length: u64) {
        if text.is_null() {
            return;
        }
        // SAFETY: the kernel passes `length` bytes of its console text.
        xh_core::log::console(unsafe { std::slice::from_raw_parts(text, length as usize) });
    }

    pub(crate) unsafe extern "C" fn halt(cpu: *mut XhCpu, reason: u32, code: u32) {
        // A panic's halt (the kernel's hosted_halt: code 0 panic(), 1 a start-up check, 2 a fatal kernel fault, 3 a
        // panic while panicking). The session ends from here whatever the other vCPUs do (the debugger's wait loops
        // spin on, and a blocked main loop is the stop watch's: session.rs).
        if reason == XH_HALT_PANIC {
            const KINDS: [&str; 4] = ["panic()", "a start-up check", "a fatal kernel fault", "a panic while panicking"];
            logi!(
                "guest panic: the kernel halted on cpu{} (code {}, {}): ending the session",
                index(cpu),
                code,
                KINDS.get(code as usize).copied().unwrap_or("?")
            );
        }
        session::stop(reason, code, &format!("the guest halted on cpu{}", index(cpu)));
        loop {
            // SAFETY: waits for a signal (the stop ends the process).
            unsafe { libc::pause() };
        }
    }

    pub(crate) unsafe extern "C" fn ready(_cpu: *mut XhCpu) {
        session::guest_ready();
    }

    pub(crate) unsafe extern "C" fn set_upcalls(_cpu: *mut XhCpu, upcalls: *const XhUpcalls) -> i64 {
        // SAFETY: the kernel passes its upcalls or null.
        match unsafe { upcalls.as_ref() } {
            Some(u) if u.irq.is_some() => {
                UPCALLS.install(u);
                0
            }
            _ => -(libc::EINVAL as i64),
        }
    }

    pub(crate) unsafe extern "C" fn wall_time(_cpu: *mut XhCpu) -> u64 {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
    }

    pub(crate) unsafe extern "C" fn cpu_start(_cpu: *mut XhCpu, index: u32, entry: u64, arg: u64) -> i64 {
        match cpu::start(index as usize, entry, arg, 0, 0) {
            Ok(()) => 0,
            Err(cpu::StartError::Already) => -(libc::EALREADY as i64),
            Err(cpu::StartError::Invalid) => -(libc::EINVAL as i64),
        }
    }

    pub(crate) unsafe extern "C" fn cpu_idle(cpu: *mut XhCpu) {
        cpu::idle(v(cpu));
    }

    pub(crate) unsafe extern "C" fn irq_ack(cpu: *mut XhCpu) -> u32 {
        let id = gic::ack(index(cpu));
        cpu::line(index(cpu), false); // recomputed: more may be pending
        id
    }

    pub(crate) unsafe extern "C" fn irq_take(cpu: *mut XhCpu) -> u32 {
        let id = gic::take(index(cpu));
        if id & XH_INTID_UNTAKEN == 0 {
            cpu::line(index(cpu), false); // recomputed: more may be pending
        }
        id
    }

    pub(crate) unsafe extern "C" fn irq_eoi(cpu: *mut XhCpu, intid: u32) {
        gic::eoi(index(cpu), intid);
        cpu::line(index(cpu), false);
    }

    pub(crate) unsafe extern "C" fn irq_enable(cpu: *mut XhCpu, intid: u32, enable: u32) {
        gic::enable(index(cpu), intid, enable != 0);
        cpu::line(index(cpu), false);
    }

    pub(crate) unsafe extern "C" fn ipi(_cpu: *mut XhCpu, target: u32, sgi: u32) {
        gic::send_sgi(target as usize, sgi);
    }

    pub(crate) unsafe extern "C" fn timer_set(cpu: *mut XhCpu, deadline: u64) {
        // One timer, two ways in: keep the register view (CNTV_CVAL_EL0, ENABLE) in step, so that a later trapped
        // write of CNTV_CTL_EL0 (ArmCpu's enableCPUTimeBase) keeps this deadline.
        let v = v(cpu);
        v.sys.set_timer(deadline);
        cpu::timer_set(v, deadline);
    }

    pub(crate) unsafe extern "C" fn frame_resume(cpu: *mut XhCpu, frame: *const XhFrame) {
        // SAFETY: the kernel passes a complete frame.
        cpu::frame_resume(v(cpu), unsafe { &*frame })
    }

    pub(crate) unsafe extern "C" fn io_map(_cpu: *mut XhCpu, phys: u64, size: u64) -> u64 {
        let m = mem::state();
        if phys >= m.io_phys && phys - m.io_phys <= m.io_size && size <= m.io_size - (phys - m.io_phys) {
            return m.io_host as u64 + (phys - m.io_phys);
        }
        // RAM outside the kernel's window (the boot framebuffer) is ordinary memory in the aperture.
        mem::ram(phys, size).map_or(0, |p| p.as_ptr() as u64)
    }

    pub(crate) unsafe extern "C" fn mmio_read(_cpu: *mut XhCpu, phys: u64, size: u32) -> u64 {
        xh_core::mmio::bus().read(phys, size)
    }

    pub(crate) unsafe extern "C" fn mmio_write(cpu: *mut XhCpu, phys: u64, size: u32, value: u64) {
        xh_core::mmio::bus().write(phys, size, value);
        cpu::line(index(cpu), false);
    }

    pub(crate) unsafe extern "C" fn ram_discard(_cpu: *mut XhCpu, phys: u64, size: u64) -> i64 {
        // The pages go back to Linux and read as zero afterwards, in every mapping of them (discard.rs); refused while
        // the RAM is held fixed for the session (ramfix.rs).
        let (i, m) = (info(), mem::state());
        discard::ram_discard_host(
            i.features & XH_FEATURE_RAM_FIXED != 0,
            m.fd,
            m.fd_phys,
            i.ram_phys,
            i.ram_size,
            phys,
            size,
            u64::from(i.host_page_size),
        )
    }

    pub(crate) unsafe extern "C" fn text_fixup(_cpu: *mut XhCpu, text: *mut c_void, size: u64) -> u32 {
        if text.is_null() || text as usize & 3 != 0 {
            return 0;
        }
        // SAFETY: the kernel passes `size` bytes of user text it owns, 4-aligned (checked).
        xh_core::insn::text_fixup(unsafe { std::slice::from_raw_parts_mut(text as *mut u32, (size / 4) as usize) })
    }

    pub(crate) unsafe extern "C" fn task_create(_cpu: *mut XhCpu, out: *mut u64) -> i64 {
        match task::create() {
            Ok(handle) => {
                // SAFETY: the kernel passes where the handle goes.
                unsafe { out.write(handle) };
                0
            }
            Err(e) => e,
        }
    }

    pub(crate) unsafe extern "C" fn task_destroy(_cpu: *mut XhCpu, task: u64) -> i64 {
        task::destroy(task)
    }

    pub(crate) unsafe extern "C" fn task_map(
        _cpu: *mut XhCpu,
        task: u64,
        va: u64,
        size: u64,
        prot: u32,
        phys: u64,
    ) -> i64 {
        task::map(task, va, size, prot, phys)
    }

    pub(crate) unsafe extern "C" fn task_unmap(_cpu: *mut XhCpu, task: u64, va: u64, size: u64) -> i64 {
        task::unmap(task, va, size)
    }

    pub(crate) unsafe extern "C" fn task_protect(_cpu: *mut XhCpu, task: u64, va: u64, size: u64, prot: u32) -> i64 {
        task::protect(task, va, size, prot)
    }

    pub(crate) unsafe extern "C" fn task_read(
        _cpu: *mut XhCpu,
        task: u64,
        va: u64,
        buffer: *mut c_void,
        size: u64,
    ) -> i64 {
        task::read(task, va, buffer, size)
    }

    pub(crate) unsafe extern "C" fn task_write(
        _cpu: *mut XhCpu,
        task: u64,
        va: u64,
        buffer: *const c_void,
        size: u64,
    ) -> i64 {
        task::write(task, va, buffer, size)
    }

    pub(crate) unsafe extern "C" fn thread_create(_cpu: *mut XhCpu, task: u64, out: *mut u64) -> i64 {
        match task::thread_create(task) {
            Ok(handle) => {
                // SAFETY: the kernel passes where the handle goes.
                unsafe { out.write(handle) };
                0
            }
            Err(e) => e,
        }
    }

    pub(crate) unsafe extern "C" fn thread_destroy(_cpu: *mut XhCpu, thread: u64) -> i64 {
        task::thread_destroy(thread)
    }

    pub(crate) unsafe extern "C" fn thread_run(
        cpu: *mut XhCpu,
        thread: u64,
        state: *mut XhUserState,
        trap: *mut XhTrap,
    ) -> i64 {
        // SAFETY: the kernel passes the thread's state and where the trap goes, both its own.
        task::thread_run(v(cpu), thread, unsafe { &mut *state }, unsafe { &mut *trap })
    }

    pub(crate) unsafe extern "C" fn thread_set_pac(_cpu: *mut XhCpu, thread: u64, enabled: u32) -> i64 {
        task::thread_set_pac(thread, enabled)
    }

    pub(crate) unsafe extern "C" fn kernel_map(_cpu: *mut XhCpu, va: u64, size: u64, prot: u32, phys: u64) -> i64 {
        mem::kernel_map(va, size, prot, phys)
    }

    pub(crate) unsafe extern "C" fn kernel_unmap(_cpu: *mut XhCpu, va: u64, size: u64) -> i64 {
        mem::kernel_unmap(va, size)
    }

    /// A load or store on a device page the kernel mapped itself (IOMemoryMap): the kernel found the physical address
    /// in its page table and hands the fault back.
    pub(crate) unsafe extern "C" fn mmio_emulate(cpu: *mut XhCpu, frame: *mut XhFrame, phys: u64) -> i64 {
        let m = mem::state();
        // SAFETY: the kernel passes its frame or null.
        let Some(frame) = (unsafe { frame.as_mut() }) else { return -(libc::EINVAL as i64) };
        if phys < m.io_phys || phys - m.io_phys >= m.io_size {
            return -(libc::EINVAL as i64);
        }
        // The instruction is kernel text: in the RAM window (the kernel collection is loaded there).
        let i = info();
        let insn = mem::ram(frame.pc.wrapping_sub(i.ram_virt).wrapping_add(i.ram_phys), 4);
        let Some(insn) = insn.filter(|_| frame.pc & 3 == 0 && frame.pc >= i.ram_virt) else {
            return -(libc::EINVAL as i64);
        };
        // SAFETY: four bytes of RAM, 4-aligned (pc is).
        let insn = unsafe { *(insn.as_ptr() as *const u32) };
        let mut regs = Regs { x: frame.x, sp: frame.sp, pc: frame.pc, pstate: frame.pstate };
        if !xh_core::mmio::bus().emulate(&mut regs, insn, phys) {
            return -(libc::ENOTSUP as i64);
        }
        frame.x = regs.x;
        frame.sp = regs.sp;
        frame.pc = regs.pc;
        v(cpu).stat.mmio.add(1);
        cpu::line(index(cpu), false); // a device write may have changed this CPU's line
        0
    }

    mod thunks {
        use std::ffi::c_void;
        use xnuhost_abi::*;
        extern "C" {
            pub fn xh_hc_console_write(cpu: *mut XhCpu, text: *const u8, length: u64);
            pub fn xh_hc_halt(cpu: *mut XhCpu, reason: u32, code: u32);
            pub fn xh_hc_ready(cpu: *mut XhCpu);
            pub fn xh_hc_set_upcalls(cpu: *mut XhCpu, upcalls: *const XhUpcalls) -> i64;
            pub fn xh_hc_wall_time(cpu: *mut XhCpu) -> u64;
            pub fn xh_hc_cpu_start(cpu: *mut XhCpu, index: u32, entry: u64, arg: u64) -> i64;
            pub fn xh_hc_cpu_idle(cpu: *mut XhCpu);
            pub fn xh_hc_irq_ack(cpu: *mut XhCpu) -> u32;
            pub fn xh_hc_irq_eoi(cpu: *mut XhCpu, intid: u32);
            pub fn xh_hc_irq_enable(cpu: *mut XhCpu, intid: u32, enable: u32);
            pub fn xh_hc_ipi(cpu: *mut XhCpu, target: u32, sgi: u32);
            pub fn xh_hc_timer_set(cpu: *mut XhCpu, deadline: u64);
            pub fn xh_hc_frame_resume(cpu: *mut XhCpu, frame: *const XhFrame);
            pub fn xh_hc_io_map(cpu: *mut XhCpu, phys: u64, size: u64) -> u64;
            pub fn xh_hc_mmio_read(cpu: *mut XhCpu, phys: u64, size: u32) -> u64;
            pub fn xh_hc_mmio_write(cpu: *mut XhCpu, phys: u64, size: u32, value: u64);
            pub fn xh_hc_ram_discard(cpu: *mut XhCpu, phys: u64, size: u64) -> i64;
            pub fn xh_hc_text_fixup(cpu: *mut XhCpu, text: *mut c_void, size: u64) -> u32;
            pub fn xh_hc_task_create(cpu: *mut XhCpu, task: *mut u64) -> i64;
            pub fn xh_hc_task_destroy(cpu: *mut XhCpu, task: u64) -> i64;
            pub fn xh_hc_task_map(cpu: *mut XhCpu, task: u64, va: u64, size: u64, prot: u32, phys: u64) -> i64;
            pub fn xh_hc_task_unmap(cpu: *mut XhCpu, task: u64, va: u64, size: u64) -> i64;
            pub fn xh_hc_task_protect(cpu: *mut XhCpu, task: u64, va: u64, size: u64, prot: u32) -> i64;
            pub fn xh_hc_task_read(cpu: *mut XhCpu, task: u64, va: u64, buffer: *mut c_void, size: u64) -> i64;
            pub fn xh_hc_task_write(cpu: *mut XhCpu, task: u64, va: u64, buffer: *const c_void, size: u64) -> i64;
            pub fn xh_hc_thread_create(cpu: *mut XhCpu, task: u64, thread: *mut u64) -> i64;
            pub fn xh_hc_thread_destroy(cpu: *mut XhCpu, thread: u64) -> i64;
            pub fn xh_hc_thread_run(cpu: *mut XhCpu, thread: u64, state: *mut XhUserState, trap: *mut XhTrap) -> i64;
            pub fn xh_hc_thread_set_pac(cpu: *mut XhCpu, thread: u64, enabled: u32) -> i64;
            pub fn xh_hc_kernel_map(cpu: *mut XhCpu, va: u64, size: u64, prot: u32, phys: u64) -> i64;
            pub fn xh_hc_kernel_unmap(cpu: *mut XhCpu, va: u64, size: u64) -> i64;
            pub fn xh_hc_mmio_emulate(cpu: *mut XhCpu, frame: *mut XhFrame, phys: u64) -> i64;
            pub fn xh_hc_irq_take(cpu: *mut XhCpu) -> u32;
        }
    }

    struct Vector(XhVector);
    // SAFETY: the vector is built once and never written again; its info pointer is the static INFO.
    unsafe impl Sync for Vector {}
    // SAFETY: as above.
    unsafe impl Send for Vector {}

    /// The vector the kernel is given (`xh_vector`), `None` where the kernel cannot run.
    pub fn vector() -> Option<&'static XhVector> {
        static V: OnceLock<Vector> = OnceLock::new();
        use thunks::*;
        Some(
            &V.get_or_init(|| {
                Vector(XhVector {
                    abi: XH_ABI_VERSION,
                    size: std::mem::size_of::<XhVector>() as u32,
                    info: info() as *const XhInfo,
                    console_write: Some(xh_hc_console_write),
                    halt: Some(xh_hc_halt),
                    ready: Some(xh_hc_ready),
                    set_upcalls: Some(xh_hc_set_upcalls),
                    wall_time: Some(xh_hc_wall_time),
                    cpu_start: Some(xh_hc_cpu_start),
                    cpu_idle: Some(xh_hc_cpu_idle),
                    irq_ack: Some(xh_hc_irq_ack),
                    irq_eoi: Some(xh_hc_irq_eoi),
                    irq_enable: Some(xh_hc_irq_enable),
                    ipi: Some(xh_hc_ipi),
                    timer_set: Some(xh_hc_timer_set),
                    frame_resume: Some(xh_hc_frame_resume),
                    io_map: Some(xh_hc_io_map),
                    mmio_read: Some(xh_hc_mmio_read),
                    mmio_write: Some(xh_hc_mmio_write),
                    ram_discard: Some(xh_hc_ram_discard),
                    text_fixup: Some(xh_hc_text_fixup),
                    task_create: Some(xh_hc_task_create),
                    task_destroy: Some(xh_hc_task_destroy),
                    task_map: Some(xh_hc_task_map),
                    task_unmap: Some(xh_hc_task_unmap),
                    task_protect: Some(xh_hc_task_protect),
                    task_read: Some(xh_hc_task_read),
                    task_write: Some(xh_hc_task_write),
                    thread_create: Some(xh_hc_thread_create),
                    thread_destroy: Some(xh_hc_thread_destroy),
                    thread_run: Some(xh_hc_thread_run),
                    thread_set_pac: Some(xh_hc_thread_set_pac),
                    kernel_map: Some(xh_hc_kernel_map),
                    kernel_unmap: Some(xh_hc_kernel_unmap),
                    mmio_emulate: Some(xh_hc_mmio_emulate),
                    irq_take: Some(xh_hc_irq_take),
                })
            })
            .0,
        )
    }
}

#[cfg(any(not(xh_machine), test))]
mod calls {
    /// The vector the kernel is given (`xh_vector`), `None` where the kernel cannot run.
    pub fn vector() -> Option<&'static xnuhost_abi::XhVector> {
        None
    }
}
