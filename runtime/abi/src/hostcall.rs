// SPDX-License-Identifier: MIT
//! `hostcall.h`: the xnuhost host-call ABI, version 1, the whole contract between a hosted XNU ("LINUXHOST" machine
//! configuration, kernel/) and xnuhost. The C header stays (the kernel's machine layer includes it unchanged); this
//! is its mirror, checked against it by `tests/headers.rs`. docs/booter.md explains the model; the summary:
//!
//! - The kernel is entered on vCPU 0 at its LC_UNIXTHREAD entry with x0 = boot_args (virtual), x1 = the
//!   [`XhVector`], x2 = the [`XhCpu`] of vCPU 0, sp = a 256 KiB boot stack. The window of RAM is already mapped at
//!   boot_args.virtBase; there is no MMU to turn on. The vector is also in the device tree: /chosen/xnuhost "vector".
//! - Every host call takes the calling vCPU's [`XhCpu`] first. The host switches to its own thread register
//!   (TPIDR_EL0), x18 and stack for the call, so the kernel may keep anything in TPIDR_EL0 and x18 while its own code
//!   runs. Host calls follow AAPCS64.
//! - Interrupts are a software flag. The kernel masks them by storing 1 to cpu->irq_masked and unmasks by storing 0
//!   and then, if cpu->irq_pending is not 0, calling its own interrupt path (irq_ack until it returns
//!   [`XH_INTID_NONE`]). While unmasked and running kernel code, an interrupt arrives as a call of
//!   upcalls->irq(cpu, frame) on the interrupted stack with irq_masked = 1; returning from it resumes the interrupted
//!   code. The kernel may block or switch threads inside.
//! - A kernel that moves a thread to another vCPU inside an upcall (preemption in an interrupt) must not return from
//!   that upcall: it ends it by calling frame_resume(the vCPU the thread is on now, frame) itself, after correcting
//!   anything in the frame that names the old vCPU. Such a kernel also makes every host call with irq_masked = 1, so
//!   that no interrupt is delivered on a host call's return path (that path stores to the vCPU the call started on).
//! - A vCPU started by PSCI CPU_ON is entered like vCPU 0: x0 = the PSCI context id, x1 = the vector, x2 = its
//!   [`XhCpu`].
//! - iOS threads run in other Linux processes. thread_run() gives the vCPU to one until it traps.

use core::ffi::c_void;

pub const XH_ABI_VERSION: u32 = 1;

// ---- per-vCPU block (host memory; the kernel reads and writes only the fields marked K) ----------
/// `struct xh_cpu`: 0x80 bytes; host-private data follows.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct XhCpu {
    /// 0x00 vCPU number
    pub index: u32,
    /// 0x04 K: 1 = interrupts masked (the kernel's DAIF.I)
    pub irq_masked: u32,
    /// 0x08: not 0 = the interrupt line is asserted (read by K)
    pub irq_pending: u32,
    /// 0x0c host only
    pub in_host: u32,
    /// 0x10 the kernel's TPIDR_EL0 while a host call runs
    pub kernel_tpidr: u64,
    /// 0x18 the kernel's x18 while a host call runs
    pub kernel_x18: u64,
    /// 0x20 host only
    pub host_tpidr: u64,
    /// 0x28 host only
    pub host_x18: u64,
    /// 0x30 host only
    pub host_sp: u64,
    /// 0x38 K: free for the kernel (its cpu_data pointer)
    pub kernel_data: u64,
    /// 0x40 K: shadow of TPIDR_EL1 for trapped mrs/msr (current thread)
    pub tpidr_el1: u64,
    /// 0x48 K: what the user thread this vCPU runs needs (XH_HINT_*), written by the kernel before thread_run (kernel
    /// 0075); 0 = no hint (an older kernel). Read by thread_run (its uclamp) and by any host code on the vCPU (the GPU
    /// path: the submitting thread's role)
    pub perf_hint: u32,
    /// 0x4c
    pub perf_reserved: u32,
    /// 0x50
    pub reserved: [u64; 6],
}

// perf_hint: bits 0-10 the utilisation clamp floor the thread asks for (0-1024), and its role from XNU's own policy
// (thread_get_perfcontrol_class, the task's effective QoS and darwin-background state: RunningBoard's roles reach it
// there).
/// The kernel wrote this word.
pub const XH_HINT_VALID: u32 = 1 << 16;
/// Background or utility QoS, darwin background: no floor, no ceiling of ours.
pub const XH_HINT_BACKGROUND: u32 = 1 << 17;
/// UI, above-UI or user-initiated class (an app's main thread, its frame workers).
pub const XH_HINT_UI: u32 = 1 << 18;
/// A realtime thread (the render server's, audio).
pub const XH_HINT_REALTIME: u32 = 1 << 19;
/// `XH_HINT_FLOOR(h)`: the utilisation clamp floor of a perf hint.
pub const fn xh_hint_floor(hint: u32) -> u32 {
    hint & 0x7ff
}

// ---- saved register frames ---------------------------------------------------------------------
/// `struct xh_vreg`: one 128-bit vector register.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct XhVreg {
    pub lo: u64,
    pub hi: u64,
}

/// `struct xh_frame`: kernel context interrupted by an interrupt or exception; lives on the interrupted stack.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct XhFrame {
    pub x: [u64; 31],
    pub sp: u64,
    pub pc: u64,
    /// NZCV only is meaningful
    pub pstate: u64,
    pub fpsr: u32,
    pub fpcr: u32,
    pub v: [XhVreg; 32],
    /// the mask state to return to
    pub irq_masked: u32,
    /// XH_FRAME_*
    pub kind: u32,
    /// for XH_FRAME_EXCEPTION
    pub far: u64,
    pub esr: u64,
    /// Appended (present when vector->size covers mmio_emulate). For XH_FRAME_EXCEPTION: the Linux signal that
    /// reported the trap and its si_code: SIGSEGV/SIGBUS (a fault: far and esr are set), SIGILL (an undefined
    /// instruction the host does not emulate), SIGTRAP (brk), SIGFPE. Linux gives no esr for the last three. 0 in an
    /// XH_FRAME_IRQ frame.
    pub signo: u32,
    pub sigcode: u32,
}
pub const XH_FRAME_IRQ: u32 = 1;
pub const XH_FRAME_EXCEPTION: u32 = 2;

/// `struct xh_user_state`: user (iOS thread) register state.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct XhUserState {
    pub x: [u64; 31],
    pub sp: u64,
    pub pc: u64,
    /// NZCV; the host forces EL0t
    pub pstate: u64,
    /// The thread's TSD base. iOS reads it from TPIDRRO_EL0; the host keeps it in TPIDR_EL0 and text_fixup()
    /// rewrites the reads (docs/booter.md). iOS's own use of TPIDR_EL0 (the CPU number in its low 12 bits) is
    /// rewritten too: reads give 0, writes do nothing.
    pub tsd: u64,
    pub fpsr: u32,
    pub fpcr: u32,
    pub v: [XhVreg; 32],
}

/// `struct xh_trap`: why a thread stopped running user code.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct XhTrap {
    /// XH_TRAP_*
    pub kind: u32,
    /// the Linux signal that reported it (diagnostic)
    pub signo: u32,
    /// fault address (FAULT), instruction address (ILLEGAL, BRK)
    pub far: u64,
    /// ESR_EL1 as Linux reported it, 0 if it did not
    pub esr: u64,
    /// SYSCALL: the svc immediate (0x80 for XNU's)
    pub svc: u32,
    /// FAULT: XH_PROT_* bit of the access that faulted, 0 if unknown (a cache maintenance instruction, ESR ISS.CM:
    /// READ, though the hardware sets WnR)
    pub access: u32,
    /// the instruction at the trap, when readable
    pub insn: u32,
    /// EXITED: the Linux wait status of the task process
    pub exit_status: u32,
}
/// svc: number in x16, as XNU's handle_svc expects
pub const XH_TRAP_SYSCALL: u32 = 1;
/// data or instruction abort from user mode
pub const XH_TRAP_FAULT: u32 = 2;
/// undefined instruction
pub const XH_TRAP_ILLEGAL: u32 = 3;
/// brk / breakpoint / single step
pub const XH_TRAP_BRK: u32 = 4;
pub const XH_TRAP_FPE: u32 = 5;
/// an interrupt is pending for this vCPU; state is the interrupted user state
pub const XH_TRAP_INTERRUPT: u32 = 6;
/// the task's Linux process is gone
pub const XH_TRAP_EXITED: u32 = 7;
/// a new thread's first stop, before any user instruction
pub const XH_TRAP_BOOT: u32 = 8;

pub const XH_PROT_READ: u32 = 1;
pub const XH_PROT_WRITE: u32 = 2;
pub const XH_PROT_EXEC: u32 = 4;

// ---- interrupts: the board's GICv3 numbering ----------------------------------------------------
pub const XH_INTID_NONE: u32 = 1023;
/// per-CPU virtual timer (PPI), as ArmCpu takes it
pub const XH_INTID_VTIMER: u32 = 27;
pub const XH_SGI_COUNT: u32 = 16;
/// ArmCpu's software IPIs (armcpu-spi-ipi=1): SPI 1008 + core
pub const XH_IPI_SPI_FIRST: u32 = 1008;
pub const XH_IPI_SPI_COUNT: u32 = 8;
/// irq_take (appended to the vector) sets this bit in the INTID it returns when it did not acknowledge it: a device
/// interrupt, which stays pending for the controller driver's own ICC_IAR1_EL1 path.
pub const XH_INTID_UNTAKEN: u32 = 0x8000_0000;

// ---- halt reasons -------------------------------------------------------------------------------
/// guest shut down: xnuhost exits 0
pub const XH_HALT_OFF: u32 = 0;
/// guest asked for a reboot: exit 3
pub const XH_HALT_RESTART: u32 = 1;
/// kernel panic: exit 4
pub const XH_HALT_PANIC: u32 = 2;
/// guest asks to return to One UI: session/return.request, exit 0
pub const XH_HALT_RETURN: u32 = 3;

/// The halt reasons as a type, for the host's side of `halt`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HaltReason {
    Off,
    Restart,
    Panic,
    Return,
}

impl HaltReason {
    /// The reason a kernel passed, `None` for a number the ABI does not define.
    pub const fn from_raw(reason: u32) -> Option<Self> {
        match reason {
            XH_HALT_OFF => Some(Self::Off),
            XH_HALT_RESTART => Some(Self::Restart),
            XH_HALT_PANIC => Some(Self::Panic),
            XH_HALT_RETURN => Some(Self::Return),
            _ => None,
        }
    }

    pub const fn raw(self) -> u32 {
        match self {
            Self::Off => XH_HALT_OFF,
            Self::Restart => XH_HALT_RESTART,
            Self::Panic => XH_HALT_PANIC,
            Self::Return => XH_HALT_RETURN,
        }
    }
}

// ---- static facts about the host ----------------------------------------------------------------
/// `struct xh_info`.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct XhInfo {
    pub abi: u32,
    pub ncpu: u32,
    /// 4096 on the S24 Ultra; never larger than 16384
    pub host_page_size: u32,
    /// of the host's user address space
    pub user_va_bits: u32,
    /// the kernel's window: physical, where it is mapped, bytes
    pub ram_phys: u64,
    pub ram_virt: u64,
    pub ram_size: u64,
    /// trapped MMIO aperture (the board's devices)
    pub io_phys: u64,
    pub io_virt: u64,
    pub io_size: u64,
    /// iOS tasks may use [min, limit)
    pub user_va_min: u64,
    pub user_va_limit: u64,
    /// CNTFRQ_EL0; CNTVCT_EL0 is readable in the kernel and in tasks
    pub timebase_hz: u64,
    /// XH_FEATURE_*
    pub features: u64,
    /// boot_args video, repeated
    pub video: [u64; 6],
    /// Appended after the first ABI 1 layout; present when vector->size covers kernel_unmap. A reservation (no
    /// access) in xnuhost's own address space for the kernel's mappings outside the RAM window (kernel_map: zones,
    /// kalloc, stacks, IOKit buffers): from the end of the RAM rounded up to 1 GiB, to 0x3c00000000. kva_size is 0 if
    /// it could not be reserved.
    pub kva_base: u64,
    pub kva_size: u64,
}
/// paca/pacg: arm64e runs; one key set for kernel and tasks
pub const XH_FEATURE_PAC: u64 = 1 << 0;
/// the task trap layer works on this host (seccomp, memfd)
pub const XH_FEATURE_TASKS: u64 = 1 << 1;
/// PR_PAC_SET_ENABLED_KEYS: plain arm64 tasks can run
pub const XH_FEATURE_PAC_KEYS_OFF: u64 = 1 << 2;
/// the RAM is held for the whole session (ram-giveback=0): ram_discard is refused (-EPERM); a kernel gives no free
/// pages back (0067)
pub const XH_FEATURE_RAM_FIXED: u64 = 1 << 3;

// ---- calls from the host into the kernel --------------------------------------------------------
/// `struct xh_upcalls`.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct XhUpcalls {
    /// An interrupt while kernel code ran unmasked. Returning resumes `frame`.
    pub irq: Option<unsafe extern "C" fn(cpu: *mut XhCpu, frame: *mut XhFrame)>,
    /// A fault in kernel code that is not a device access (frame->kind = XH_FRAME_EXCEPTION, far, esr). The kernel
    /// may change frame->pc (copyio recovery) and return, or panic. NULL: xnuhost stops.
    pub exception: Option<unsafe extern "C" fn(cpu: *mut XhCpu, frame: *mut XhFrame)>,
    /// A secondary vCPU's first call, on its boot stack, instead of an entry address (cpu_start with entry 0). Does
    /// not return.
    pub cpu_entry: Option<unsafe extern "C" fn(cpu: *mut XhCpu, arg: u64)>,
}

// ---- the vector ---------------------------------------------------------------------------------

/// `struct xh_vector`. Return values: 0 or a positive count on success, a negative errno otherwise. Every entry is
/// a nullable function pointer (`Option`), the same layout as C's.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct XhVector {
    pub abi: u32,
    pub size: u32,
    pub info: *const XhInfo,

    // console, life cycle
    pub console_write: Option<unsafe extern "C" fn(*mut XhCpu, text: *const u8, length: u64)>,
    /// does not return
    pub halt: Option<unsafe extern "C" fn(*mut XhCpu, reason: u32, code: u32)>,
    /// the guest's first frame is on screen
    pub ready: Option<unsafe extern "C" fn(*mut XhCpu)>,
    pub set_upcalls: Option<unsafe extern "C" fn(*mut XhCpu, *const XhUpcalls) -> i64>,
    /// seconds since 1970 (armdt_platform_clock_register)
    pub wall_time: Option<unsafe extern "C" fn(*mut XhCpu) -> u64>,

    // CPUs, interrupts, timer
    pub cpu_start: Option<unsafe extern "C" fn(*mut XhCpu, index: u32, entry: u64, arg: u64) -> i64>,
    /// returns when irq_pending is set
    pub cpu_idle: Option<unsafe extern "C" fn(*mut XhCpu)>,
    /// highest pending INTID, now active; XH_INTID_NONE
    pub irq_ack: Option<unsafe extern "C" fn(*mut XhCpu) -> u32>,
    pub irq_eoi: Option<unsafe extern "C" fn(*mut XhCpu, intid: u32)>,
    pub irq_enable: Option<unsafe extern "C" fn(*mut XhCpu, intid: u32, enable: u32)>,
    pub ipi: Option<unsafe extern "C" fn(*mut XhCpu, target_cpu: u32, sgi: u32)>,
    /// The vCPU's deadline timer: INTID 27 (a level) is up from CNTVCT_EL0 >= deadline until the next timer_set.
    /// UINT64_MAX cancels. It is the same timer as the trapped CNTV_CVAL_EL0/CNTV_CTL_EL0.
    pub timer_set: Option<unsafe extern "C" fn(*mut XhCpu, deadline: u64)>,
    /// Does not return. A frame resumed unmasked while the vCPU's line is up is interrupted again at once
    /// (upcalls->irq on that frame's context), as an exception return with a pending interrupt.
    pub frame_resume: Option<unsafe extern "C" fn(*mut XhCpu, *const XhFrame)>,

    // devices
    /// kernel VA, 0 if outside
    pub io_map: Option<unsafe extern "C" fn(*mut XhCpu, phys: u64, size: u64) -> u64>,
    pub mmio_read: Option<unsafe extern "C" fn(*mut XhCpu, phys: u64, size: u32) -> u64>,
    pub mmio_write: Option<unsafe extern "C" fn(*mut XhCpu, phys: u64, size: u32, value: u64)>,

    // memory
    /// Give free pages back to Linux: a hole punched in the RAM memfd (fallocate PUNCH_HOLE, the size kept). Only
    /// inside the kernel's window [info->ram_phys, +ram_size), host-page aligned, size > 0, else -EINVAL;
    /// fallocate's errno otherwise. Afterwards the range reads as zero in every mapping (the window, every task that
    /// maps it; a task's mapping is not removed, its next access gets the new page) and the host holds no memory for
    /// it until it is touched again (a read through a shared mapping fills it too). Kernel 0058's free-page reporting
    /// calls it for pages on XNU's free queue only (free_report.h); xnuhost counts it for its `memory:` log line.
    /// When info->features has XH_FEATURE_RAM_FIXED (the default since 2026-11-13: the RAM is held for the whole
    /// session) every call is refused with -EPERM and counted; kernel 0067 then does not call it.
    pub ram_discard: Option<unsafe extern "C" fn(*mut XhCpu, phys: u64, size: u64) -> i64>,
    /// User text, exactly once per page of original text, before it is mapped executable: `mrs xN, TPIDRRO_EL0`
    /// becomes `mrs xN, TPIDR_EL0` (the TSD base), `mrs xN, TPIDR_EL0` becomes `mov xN, #0` (iOS's CPU number:
    /// always 0) and `msr TPIDR_EL0, xN` becomes `nop`. A second pass over a rewritten page would turn its TSD reads
    /// into zeros. Returns the rewritten instruction count.
    pub text_fixup: Option<unsafe extern "C" fn(*mut XhCpu, text: *mut c_void, size: u64) -> u32>,

    // iOS tasks (Linux processes) and threads
    pub task_create: Option<unsafe extern "C" fn(*mut XhCpu, task: *mut u64) -> i64>,
    pub task_destroy: Option<unsafe extern "C" fn(*mut XhCpu, task: u64) -> i64>,
    pub task_map: Option<unsafe extern "C" fn(*mut XhCpu, task: u64, va: u64, size: u64, prot: u32, phys: u64) -> i64>,
    pub task_unmap: Option<unsafe extern "C" fn(*mut XhCpu, task: u64, va: u64, size: u64) -> i64>,
    pub task_protect: Option<unsafe extern "C" fn(*mut XhCpu, task: u64, va: u64, size: u64, prot: u32) -> i64>,
    pub task_read: Option<unsafe extern "C" fn(*mut XhCpu, task: u64, va: u64, buffer: *mut c_void, size: u64) -> i64>,
    pub task_write:
        Option<unsafe extern "C" fn(*mut XhCpu, task: u64, va: u64, buffer: *const c_void, size: u64) -> i64>,
    pub thread_create: Option<unsafe extern "C" fn(*mut XhCpu, task: u64, thread: *mut u64) -> i64>,
    pub thread_destroy: Option<unsafe extern "C" fn(*mut XhCpu, thread: u64) -> i64>,
    /// Runs the thread from *state until it traps; *state is then the trapped state. Blocks the vCPU.
    pub thread_run:
        Option<unsafe extern "C" fn(*mut XhCpu, thread: u64, state: *mut XhUserState, trap: *mut XhTrap) -> i64>,
    /// Pointer-authentication keys enabled for the thread from its next run (plain arm64 binaries: 0).
    pub thread_set_pac: Option<unsafe extern "C" fn(*mut XhCpu, thread: u64, enabled: u32) -> i64>,

    /// Appended (check vector->size). The kernel's own address space: RAM pages at kernel virtual addresses inside
    /// [info->kva_base, +kva_size). The host mapping plays the TLB of the kernel's page tables: kernel_map on a fault
    /// the page table allows, kernel_unmap on an invalidation. va, size and phys are host-page aligned. After
    /// kernel_unmap the range faults again (upcalls->exception). Both may be called with interrupts masked.
    pub kernel_map: Option<unsafe extern "C" fn(*mut XhCpu, va: u64, size: u64, prot: u32, phys: u64) -> i64>,
    pub kernel_unmap: Option<unsafe extern "C" fn(*mut XhCpu, va: u64, size: u64) -> i64>,

    /// Appended (check vector->size). A device page mapped at a kernel address by the kernel's own page tables
    /// (IOKit's IOMemoryMap, not io_map) has no host mapping: the access faults into upcalls->exception. When the
    /// page-table entry's physical address is inside [io_phys, +io_size), the kernel calls this instead of
    /// kernel_map: the host emulates the integer load or store at frame->pc against device address `phys` (page
    /// address plus the offset of frame->far in its page) on the frame's registers and advances frame->pc. 0 = done;
    /// -EINVAL: phys is not in the aperture or pc is not kernel text; -ENOTSUP: not a load or store the host emulates
    /// (SIMD, exclusive, atomics): the frame is unchanged. May be called with interrupts masked.
    pub mmio_emulate: Option<unsafe extern "C" fn(*mut XhCpu, frame: *mut XhFrame, phys: u64) -> i64>,

    /// Appended (check vector->size; fix-mach-rpc, 2026-11-15). The interrupt path of a kernel whose controller
    /// driver reads ICC_IAR1_EL1 and writes ICC_EOIR1_EL1 (ArmCpu): each is a trapped instruction, a signal round
    /// trip on the vCPU's thread, three per interrupt and a fourth (GICD_ICPENDR) per software-IPI SPI. irq_take is
    /// irq_ack for the interrupts the kernel handles itself: the highest pending INTID of the calling CPU,
    /// acknowledged (active) and returned when it is the timer (XH_INTID_VTIMER), an SGI or a software-IPI SPI
    /// (XH_IPI_SPI_FIRST + n); another (a device's SPI) is returned with XH_INTID_UNTAKEN set and left pending, for
    /// the driver; XH_INTID_NONE when nothing is pending. End a taken one with irq_eoi. Call masked.
    pub irq_take: Option<unsafe extern "C" fn(*mut XhCpu) -> u32>,
}

/// APIA, APIB, APDA, APDB
pub const XH_PAC_ALL: u32 = 0xf;

// PSCI function numbers the host answers when the kernel issues `hvc #0` (or `smc #0`) directly, as ArmCpu and
// ArmDTPlatform do: CPU_ON starts a vCPU at a physical entry address.
pub const XH_PSCI_VERSION: u32 = 0x8400_0000;
pub const XH_PSCI_CPU_OFF: u32 = 0x8400_0002;
pub const XH_PSCI_CPU_ON: u32 = 0xc400_0003;
pub const XH_PSCI_SYSTEM_OFF: u32 = 0x8400_0008;
pub const XH_PSCI_SYSTEM_RESET: u32 = 0x8400_0009;

const _: () = assert!(core::mem::size_of::<XhCpu>() == 0x80);
