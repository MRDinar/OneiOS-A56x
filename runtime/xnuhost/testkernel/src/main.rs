// SPDX-License-Identifier: MIT
//! A stand-in for the hosted XNU (tests/testkernel.c in Rust): a freestanding AArch64 program that xnuhost loads and
//! enters exactly as it will the kernel collection, and that exercises every part of the host-call ABI
//! (xnuhost/hostcall.h, xnuhost-abi) the way XNU's machine layer will. It prints one line per check and halts.
//!
//! The program has no fixups: it runs where xnuhost mapped the window (virt_base), not at its link address, so no
//! constant may hold a pointer (a statically initialised function pointer or string table would hold its link address;
//! XNU rebases its own chained fixups after entry). Code reaches code and data PC-relative (adrp); the upcalls are
//! filled in at run time. It never touches x18 (build.sh checks the code): the host's code that runs on a vCPU's
//! thread (its signal handlers) may use it, as the hosted kernel, built for iOS, leaves it alone.
//!
//! The boot-args it reads: testkernel-restart, testkernel-parallel[=SCALE], testkernel-rpc[=SCALE] with
//! testkernel-rpc-mode=N, testkernel-bench[=SCALE] (the Makefile's check-arm64, bench-arm64, parallel-arm64, rpc-arm64),
//! testkernel-hold (idle for good: check-arm64.sh signals xnuhost from outside).
#![no_std]
#![no_main]

mod adt;
mod bench;
mod tasks;

use core::arch::{asm, global_asm};
use core::cell::UnsafeCell;
use core::mem::{offset_of, size_of};
use core::ptr::{addr_of, addr_of_mut, null_mut, read_volatile, write_volatile};
use core::sync::atomic::{AtomicPtr, AtomicU32, AtomicU64, Ordering::Relaxed};
use xnuhost_abi::*;

global_asm!(".section .text.start,\"ax\"", ".globl _start", "_start:", "  b kernel_main");

extern "C" {
    /// The image's first byte (testkernel.ld): where the kernel runs.
    static __base: u8;
}

/// pexpert's boot_args, Revision 2 Version 2, as the loader writes it.
#[repr(C)]
pub(crate) struct BootArgs {
    pub revision: u16,
    pub version: u16,
    pub pad0: u32,
    pub virt_base: u64,
    pub phys_base: u64,
    pub mem_size: u64,
    pub top_of_kernel_data: u64,
    pub video: [u64; 6],
    pub machine_type: u32,
    pub pad1: u32,
    pub device_tree: u64,
    pub device_tree_length: u32,
    pub command_line: [u8; 1024],
    pub pad2: u32,
    pub boot_flags: u64,
    pub mem_size_actual: u64,
}

/// Memory shared between vCPUs as the C test kernel's statics are: each test hands it over by the atomics around it.
pub(crate) struct Racy<T>(UnsafeCell<T>);

// SAFETY: every use hands the value from one vCPU to another through an atomic (or the host call that starts a
// vCPU), as the C program's statics.
unsafe impl<T> Sync for Racy<T> {}

impl<T> Racy<T> {
    pub(crate) const fn new(value: T) -> Self {
        Racy(UnsafeCell::new(value))
    }

    pub(crate) fn get(&self) -> *mut T {
        self.0.get()
    }
}

static H: AtomicPtr<XhVector> = AtomicPtr::new(null_mut());
static C0: AtomicPtr<XhCpu> = AtomicPtr::new(null_mut());
static BA: AtomicPtr<BootArgs> = AtomicPtr::new(null_mut());
static FAILURES: AtomicU32 = AtomicU32::new(0);
/// physical bump allocator above topOfKernelData
static NEXT_PAGE: AtomicU64 = AtomicU64::new(0);

pub(crate) static IRQ_COUNT: AtomicU32 = AtomicU32::new(0);
pub(crate) static LAST_INTID: AtomicU32 = AtomicU32::new(0);
pub(crate) static TIMER_FIRES: AtomicU32 = AtomicU32::new(0);
pub(crate) static SGI_SEEN: AtomicU32 = AtomicU32::new(0);
pub(crate) static REARM: AtomicU32 = AtomicU32::new(0);
static SECONDARY_SEEN: AtomicU64 = AtomicU64::new(0);
static PSCI_SEEN: AtomicU64 = AtomicU64::new(0);
static PSCI_OK: AtomicU64 = AtomicU64::new(0);

/// The vector xnuhost entered the kernel with.
pub(crate) fn h() -> &'static XhVector {
    // SAFETY: set once at entry, before anything reads it; xnuhost keeps the vector for the whole run.
    unsafe { &*H.load(Relaxed) }
}

pub(crate) fn info() -> &'static XhInfo {
    // SAFETY: the vector's info block lives as long as the vector.
    unsafe { &*h().info }
}

pub(crate) fn c0() -> *mut XhCpu {
    C0.load(Relaxed)
}

pub(crate) fn ba() -> &'static BootArgs {
    // SAFETY: set once at entry; the loader's boot_args stay in the window.
    unsafe { &*BA.load(Relaxed) }
}

/// A host call the vector must have (the test kernel checks the vector's size before it uses an appended one).
pub(crate) fn need<F>(f: Option<F>) -> F {
    match f {
        Some(f) => f,
        None => loop {
            // SAFETY: waits for an interrupt: a vector without the call cannot go on.
            unsafe { asm!("wfi", options(nomem, nostack)) };
        },
    }
}

/// A host call's function pointer, called with its arguments as a tuple (so that `host!` evaluates them outside its
/// unsafe block).
pub(crate) trait HostCall {
    type Args;
    type Out;
    /// # Safety
    /// A host call of the vector xnuhost entered the kernel with, with the arguments hostcall.h gives it.
    unsafe fn call(self, args: Self::Args) -> Self::Out;
}

macro_rules! host_call_arity {
    ($($a:ident $i:tt),+) => {
        impl<R, $($a),+> HostCall for unsafe extern "C" fn($($a),+) -> R {
            type Args = ($($a,)+);
            type Out = R;
            unsafe fn call(self, args: Self::Args) -> R {
                // SAFETY: the caller's (HostCall::call).
                unsafe { self($(args.$i),+) }
            }
        }
    };
}
host_call_arity!(A 0);
host_call_arity!(A 0, B 1);
host_call_arity!(A 0, B 1, C 2);
host_call_arity!(A 0, B 1, C 2, D 3);
host_call_arity!(A 0, B 1, C 2, D 3, E 4);
host_call_arity!(A 0, B 1, C 2, D 3, E 4, F 5);

pub(crate) fn host_call<F: HostCall>(f: F, args: F::Args) -> F::Out {
    // SAFETY: every host!() passes the arguments hostcall.h gives the call: vCPU blocks the host gave, the test's own
    // pages and states.
    unsafe { f.call(args) }
}

/// `host!(name(args...))`: the host call `name` of the vector.
#[macro_export]
macro_rules! host {
    ($call:ident($($arg:expr),* $(,)?)) => {
        $crate::host_call($crate::need($crate::h().$call), ($($arg,)*))
    };
}

// ---- a vCPU's block: the words the host writes too -------------------------------------------
pub(crate) fn irq_pending(cpu: *mut XhCpu) -> bool {
    // SAFETY: the vCPU's block, which the host keeps for the run.
    unsafe { read_volatile(addr_of!((*cpu).irq_pending)) != 0 }
}

pub(crate) fn irq_masked(cpu: *mut XhCpu) -> u32 {
    // SAFETY: as irq_pending.
    unsafe { read_volatile(addr_of!((*cpu).irq_masked)) }
}

pub(crate) fn set_irq_masked(cpu: *mut XhCpu, masked: u32) {
    // SAFETY: as irq_pending; irq_masked is the kernel's to write.
    unsafe { write_volatile(addr_of_mut!((*cpu).irq_masked), masked) }
}

pub(crate) fn cpu_index(cpu: *mut XhCpu) -> u32 {
    // SAFETY: as irq_pending.
    unsafe { read_volatile(addr_of!((*cpu).index)) }
}

// ---- the console and the checks ------------------------------------------------------------------
pub(crate) fn put(s: &str) {
    host!(console_write(c0(), s.as_ptr(), s.len() as u64));
}

pub(crate) fn hex(v: u64) {
    let mut b = *b"0x0000000000000000";
    for i in 0..16 {
        b[2 + i] = b"0123456789abcdef"[((v >> (60 - 4 * i)) & 15) as usize];
    }
    // SAFETY: ASCII digits.
    put(unsafe { core::str::from_utf8_unchecked(&b) });
}

pub(crate) fn dec(mut v: u64) {
    let mut b = [0u8; 20];
    let mut i = 20;
    loop {
        i -= 1;
        b[i] = b'0' + (v % 10) as u8;
        v /= 10;
        if v == 0 {
            break;
        }
    }
    // SAFETY: ASCII digits.
    put(unsafe { core::str::from_utf8_unchecked(&b[i..]) });
}

pub(crate) fn check(ok: bool, what: &str) {
    put(if ok { "PASS " } else { "FAIL " });
    put(what);
    put("\n");
    if !ok {
        FAILURES.fetch_add(1, Relaxed);
    }
}

// ---- memory --------------------------------------------------------------------------------------
/// Zeroes `n` bytes at `p` (a byte loop the compiler does not turn into a memset call).
pub(crate) fn zero<T>(p: *mut T, n: usize) {
    let p = p as *mut u8;
    for i in 0..n {
        // SAFETY: the caller's object of n bytes.
        unsafe { write_volatile(p.add(i), 0) };
    }
}

pub(crate) fn same(a: *const u8, b: &[u8]) -> bool {
    // SAFETY: the caller's object holds b.len() bytes.
    b.iter().enumerate().all(|(i, y)| unsafe { read_volatile(a.add(i)) } == *y)
}

pub(crate) fn kva(pa: u64) -> u64 {
    pa.wrapping_sub(ba().phys_base).wrapping_add(ba().virt_base)
}

pub(crate) fn kpa(va: u64) -> u64 {
    va.wrapping_sub(ba().virt_base).wrapping_add(ba().phys_base)
}

pub(crate) fn page_alloc() -> u64 {
    let pa = NEXT_PAGE.fetch_add(0x4000, Relaxed);
    zero(kva(pa) as *mut u8, 0x4000);
    pa
}

/// The kernel's view of a page of RAM.
pub(crate) fn at<T>(pa: u64) -> *mut T {
    kva(pa) as *mut T
}

pub(crate) fn read64(va: u64) -> u64 {
    // SAFETY: an address the test mapped (the window or a kernel mapping).
    unsafe { read_volatile(va as *const u64) }
}

pub(crate) fn write64(va: u64, v: u64) {
    // SAFETY: as read64.
    unsafe { write_volatile(va as *mut u64, v) }
}

pub(crate) fn now() -> u64 {
    let v: u64;
    // SAFETY: reads the virtual counter.
    unsafe { asm!("mrs {}, cntvct_el0", out(reg) v, options(nomem, nostack, preserves_flags)) };
    v
}

pub(crate) fn cpu_yield() {
    // SAFETY: a hint.
    unsafe { asm!("yield", options(nomem, nostack, preserves_flags)) };
}

// ---- interrupts ---------------------------------------------------------------------------------
pub(crate) extern "C" fn irq_upcall(cpu: *mut XhCpu, _frame: *mut XhFrame) {
    loop {
        let id = host!(irq_ack(cpu));
        if id == XH_INTID_NONE {
            break;
        }
        IRQ_COUNT.fetch_add(1, Relaxed);
        LAST_INTID.store(id, Relaxed);
        if id == XH_INTID_VTIMER {
            TIMER_FIRES.fetch_add(1, Relaxed);
            if REARM.load(Relaxed) != 0 {
                REARM.fetch_sub(1, Relaxed);
                host!(timer_set(cpu, now() + info().timebase_hz / 1000));
            } else {
                host!(timer_set(cpu, u64::MAX));
            }
        } else if id < XH_SGI_COUNT {
            SGI_SEEN.fetch_or(1 << id, Relaxed);
        }
        host!(irq_eoi(cpu, id));
    }
}

pub(crate) fn take_irqs(cpu: *mut XhCpu) {
    irq_upcall(cpu, null_mut());
}

/// Runs the thread until it traps with something other than an interrupt (each interrupt taken by `irq`): the host
/// call's result.
pub(crate) fn run_thread(
    cpu: *mut XhCpu,
    thread: u64,
    st: *mut XhUserState,
    trap: &mut XhTrap,
    irq: fn(*mut XhCpu),
) -> i64 {
    loop {
        let rc = host!(thread_run(cpu, thread, st, trap));
        if rc != 0 || trap.kind != XH_TRAP_INTERRUPT {
            return rc;
        }
        irq(cpu);
    }
}

extern "C" fn secondary_entry(cpu: *mut XhCpu, arg: u64) {
    SECONDARY_SEEN.store(arg + cpu_index(cpu) as u64, Relaxed);
    host!(irq_enable(cpu, 6, 1));
    host!(irq_enable(cpu, 7, 1)); // test_xcall's cross-call
    host!(ipi(cpu, 0, 5));
    tasks::worker_loop(cpu)
}

extern "C" fn psci_entry(context: u64, vector: *const XhVector, cpu: *mut XhCpu) -> ! {
    // entered like vCPU 0
    PSCI_OK.store((core::ptr::eq(vector, h()) && cpu_index(cpu) == 2) as u64, Relaxed);
    PSCI_SEEN.store(context, Relaxed);
    loop {
        // SAFETY: waits for an interrupt.
        unsafe { asm!("wfi", options(nomem, nostack)) };
    }
}

#[inline(never)]
fn work(n: u64, f: &mut f64) -> u64 {
    let (mut a, mut b, mut c, mut d) = (1u64, 2u64, 3u64, 5u64);
    let mut x = 1.0f64;
    for i in 1..=n {
        a = a.wrapping_add(i.wrapping_mul(i) ^ (b >> 3));
        b = b.wrapping_mul(31).wrapping_add(a);
        c ^= a.wrapping_add(d << 1);
        d = d.wrapping_add(c >> 5);
        x = x * 1.0000001 + (i & 7) as f64;
    }
    *f = x;
    a ^ b ^ c ^ d
}

// ---- virtio block, the registers NativeVirtioBlock programs ---------------------------------------
fn reg_read(r: *mut u32, off: usize) -> u32 {
    // SAFETY: a register of the device window io_map gave (a trapped load).
    unsafe { read_volatile(r.add(off / 4)) }
}

fn reg_write(r: *mut u32, off: usize, v: u32) {
    // SAFETY: as reg_read (a trapped store).
    unsafe { write_volatile(r.add(off / 4), v) }
}

#[repr(C)]
struct Desc {
    addr: u64,
    len: u32,
    flags: u16,
    next: u16,
}

#[repr(C)]
struct BlkReq {
    kind: u32,
    reserved: u32,
    sector: u64,
}

fn blk_read(r: *mut u32, queue_pa: u64, sector: u64, data: *mut u8, status: *mut u8) -> bool {
    let desc: *mut Desc = at(queue_pa);
    let avail: *mut u16 = at(queue_pa + 0x1000);
    let used: *mut u16 = at(queue_pa + 0x2000);
    let req: *mut BlkReq = at(queue_pa + 0x3000);
    // SAFETY: the queue's pages, allocated by the test; the device reads and writes them while QueueNotify traps.
    unsafe {
        write_volatile(req, BlkReq { kind: 0, reserved: 0, sector });
        write_volatile(desc, Desc { addr: kpa(req as u64), len: 16, flags: 1, next: 1 });
        write_volatile(desc.add(1), Desc { addr: kpa(data as u64), len: 512, flags: 3, next: 2 });
        write_volatile(desc.add(2), Desc { addr: kpa(status as u64), len: 1, flags: 2, next: 0 });
        let before = read_volatile(used.add(1));
        let idx = read_volatile(avail.add(1));
        write_volatile(avail.add(2 + (idx % 8) as usize), 0);
        write_volatile(avail.add(1), idx.wrapping_add(1));
        reg_write(r, 0x50, 0); // QueueNotify
        read_volatile(used.add(1)) == before.wrapping_add(1)
    }
}

fn test_devices() {
    let c = c0();
    // The console's PL011 through the trapped aperture.
    let uart = host!(io_map(c, 0x6000, 0x1000));
    check(uart == 0, "io_map refuses an address outside the aperture");
    let gicd = host!(io_map(c, 0x0800_0000, 0x10000));
    check(gicd != 0 && (reg_read(gicd as *mut u32, 0xffe8) >> 4 & 15) == 3, "GICD_PIDR2 by a trapped load says GICv3");
    let r = host!(io_map(c, 0x0a00_0000, 0x1000)) as *mut u32;
    check(
        !r.is_null() && reg_read(r, 0) == 0x7472_6976 && reg_read(r, 4) == 2 && reg_read(r, 8) == 2,
        "virtio-mmio block device at the first window (trapped loads)",
    );
    check(host!(mmio_read(c, 0x0a00_0000, 4)) == 0x7472_6976, "the same register by the mmio_read host call");
    // Feature negotiation and queue setup as NativeVirtioBlock::configureQueue does them.
    reg_write(r, 0x70, 0);
    reg_write(r, 0x70, 1);
    reg_write(r, 0x70, 3);
    reg_write(r, 0x14, 0);
    let low = reg_read(r, 0x10);
    reg_write(r, 0x14, 1);
    let high = reg_read(r, 0x10);
    check(low & (1 << 5) != 0 && high & 1 != 0, "the image is offered read-only (F_RO) and VERSION_1");
    reg_write(r, 0x24, 0);
    reg_write(r, 0x20, 1 << 5);
    reg_write(r, 0x24, 1);
    reg_write(r, 0x20, 1);
    reg_write(r, 0x70, 11);
    check(reg_read(r, 0x70) & 8 != 0, "FEATURES_OK accepted");
    reg_write(r, 0x30, 0);
    let q = page_alloc();
    check(reg_read(r, 0x44) == 0 && reg_read(r, 0x34) >= 8, "queue 0 is free and large enough");
    reg_write(r, 0x38, 8);
    for (off, addr) in [(0x80, q), (0x90, q + 0x1000), (0xa0, q + 0x2000)] {
        reg_write(r, off, addr as u32);
        reg_write(r, off + 4, (addr >> 32) as u32);
    }
    reg_write(r, 0x44, 1);
    let capacity = reg_read(r, 0x100) as u64 | (reg_read(r, 0x104) as u64) << 32;
    check(capacity == 2048, "capacity from the configuration space");
    reg_write(r, 0x70, 15);
    let data: *mut u8 = at(page_alloc());
    // SAFETY: the status byte is in the data's page.
    let status = unsafe { data.add(0x1000) };
    // SAFETY: as above.
    unsafe { write_volatile(status, 0xff) };
    let done = blk_read(r, q, 77, data, status);
    // SAFETY: the data page.
    let (n, byte100, st) = unsafe {
        (
            u32::from_le_bytes([0, 1, 2, 3].map(|i| read_volatile(data.add(8 + i)))),
            read_volatile(data.add(100)),
            read_volatile(status),
        )
    };
    check(
        done && st == 0 && same(data, b"ONEIOSTD") && n == 77 && byte100 == (77 ^ 0x5a),
        "sector 77 read through the virtqueue",
    );
    check(reg_read(r, 0x60) & 1 != 0, "the device raised its interrupt status");
    reg_write(r, 0x64, 1);
}

/// The boot argument `word` at position `c` of the command line: the index after it when it is there.
fn word_at(line: &[u8], c: usize, word: &[u8]) -> Option<usize> {
    line[c..].starts_with(word).then_some(c + word.len())
}

/// The byte at `i`, 0 past the command line's end.
fn byte(line: &[u8], i: usize) -> u8 {
    line.get(i).copied().unwrap_or(0)
}

/// `=1`..`=9` after a boot argument: its scale, else 1.
fn scale_at(line: &[u8], i: usize) -> u32 {
    match (byte(line, i), byte(line, i + 1)) {
        (b'=', d @ b'1'..=b'9') => (d - b'0') as u32,
        _ => 1,
    }
}

fn halt(cpu: *mut XhCpu, reason: u32, code: u32) -> ! {
    host!(halt(cpu, reason, code));
    loop {
        cpu_yield();
    }
}

/// # Safety
/// Entered by xnuhost as hostcall.h says: x0 = boot_args, x1 = the vector, x2 = vCPU 0's block.
#[no_mangle]
unsafe extern "C" fn kernel_main(args: *const BootArgs, vector: *const XhVector, cpu: *mut XhCpu) -> ! {
    H.store(vector as *mut XhVector, Relaxed);
    C0.store(cpu, Relaxed);
    BA.store(args as *mut BootArgs, Relaxed);
    put("TESTKERNEL: entered\n");
    let a = ba();
    let len = a.command_line.iter().position(|b| *b == 0).unwrap_or(a.command_line.len());
    let line = &a.command_line[..len];
    let first_page = (a.top_of_kernel_data + 0x3fff) & !0x3fff;
    // boot-args "testkernel-restart": ask for a restart at once, every time (xnuhost starts again in place, then gives
    // up: `make check-arm64` counts the starts).
    if (0..len).any(|c| word_at(line, c, b"testkernel-restart").is_some()) {
        put("TESTKERNEL: restart\n");
        halt(cpu, XH_HALT_RESTART, 0);
    }
    // boot-args "testkernel-hold": idle for good, for a check that signals xnuhost from outside (check-arm64.sh, the
    // last words: its line, then the death of that signal). The check's own time limit ends a hold nothing ended.
    if (0..len).any(|c| word_at(line, c, b"testkernel-hold").is_some()) {
        put("TESTKERNEL: holding\n");
        loop {
            host!(timer_set(cpu, now() + info().timebase_hz));
            host!(cpu_idle(cpu));
        }
    }
    // boot-args "testkernel-parallel[=SCALE]": only the parallel test (any number of vCPUs), then off.
    if let Some(i) = (0..len).find_map(|c| word_at(line, c, b"testkernel-parallel")) {
        NEXT_PAGE.store(first_page, Relaxed);
        bench::parallel_bench(cpu, scale_at(line, i));
        halt(cpu, XH_HALT_OFF, 0);
    }
    // boot-args "testkernel-rpc[=SCALE] testkernel-rpc-mode=N": only the Mach RPC round trip (2 or more vCPUs), then
    // off.
    let (mut rpc_on, mut rpc_scale, mut rpc_mode) = (false, 1, 1);
    for c in 0..len {
        let Some(i) = word_at(line, c, b"testkernel-rpc") else { continue };
        if let Some(k) = word_at(line, i, b"-mode=") {
            if let d @ b'0'..=b'2' = byte(line, k) {
                rpc_mode = (d - b'0') as u32;
            }
            continue;
        }
        if byte(line, i) == b'=' && matches!(byte(line, i + 1), b'1'..=b'9') {
            rpc_scale = scale_at(line, i);
        }
        if matches!(byte(line, i), b'=' | b' ' | 0) {
            rpc_on = true;
        }
    }
    if rpc_on {
        NEXT_PAGE.store(first_page, Relaxed);
        bench::rpc_bench(cpu, rpc_scale, rpc_mode);
        halt(cpu, XH_HALT_OFF, 0);
    }

    // 1. What the loader handed over.
    let v = h();
    let inf = info();
    check(a.revision == 2 && a.version == 2, "boot_args revision and version");
    check(a.virt_base == addr_of!(__base) as u64, "boot_args.virtBase is where the kernel runs (no MMU switch)");
    check(a.mem_size == a.mem_size_actual && a.top_of_kernel_data > a.phys_base, "memory fields");
    check(
        v.abi == XH_ABI_VERSION && v.size as usize == size_of::<XhVector>() && inf.abi == XH_ABI_VERSION,
        "host-call vector ABI",
    );
    check(inf.ram_virt == a.virt_base && inf.ram_phys == a.phys_base && inf.ncpu == 3, "xh_info agrees with boot_args");
    let tree = a.device_tree as *const u8;
    let name = adt::prop(tree, "name");
    check(name.is_some_and(|(p, _)| same(p, b"device-tree\0")), "the device tree's root");
    let chosen = adt::child(tree, "chosen");
    let host = chosen.and_then(|c| adt::child(c, "xnuhost"));
    let vec_prop = host.and_then(|x| adt::prop(x, "vector"));
    check(vec_prop.is_some_and(|(p, _)| adt::u64_at(p) == vector as u64), "/chosen/xnuhost vector");
    // The RAM's size as XNU reads it in three places (ram=auto sizes all of them: docs/booter.md 1).
    let dram = chosen.and_then(|c| adt::prop(c, "dram-size"));
    check(
        dram.is_some_and(|(p, l)| l == 8 && adt::u64_at(p) == a.mem_size)
            && inf.ram_size == a.mem_size
            && a.mem_size & 0x3fff == 0,
        "the RAM's size agrees: boot_args.memSize, /chosen dram-size, xh_info.ram_size",
    );
    put("TESTKERNEL: RAM ");
    dec(a.mem_size >> 20);
    put(" MiB\n");
    let cpus = adt::child(tree, "cpus");
    check(
        cpus.is_some_and(|c| adt::child(c, "cpu2").is_some() && adt::child(c, "cpu3").is_none()),
        "/cpus has the three CPUs",
    );
    let io = adt::child(tree, "arm-io");
    check(
        io.is_some_and(|io| {
            ["intc", "virtio-mmio-0", "data-store", "input-0", "virtio-gpu", "uart0"]
                .into_iter()
                .all(|n| adt::child(io, n).is_some())
        }),
        "/arm-io has the board's devices",
    );
    NEXT_PAGE.store(first_page, Relaxed);

    // 2. Instructions that are undefined at EL0, emulated.
    let value: u64 = 0x1122_3344_5566_7788;
    let (back, el): (u64, u64);
    // SAFETY: EL1 system registers the host emulates (trapped at EL0).
    unsafe {
        asm!("msr tpidr_el1, {}", in(reg) value, options(nostack));
        asm!("mrs {}, tpidr_el1", out(reg) back, options(nostack));
        asm!("mrs {}, CurrentEL", out(reg) el, options(nostack));
    }
    // SAFETY: the vCPU's block.
    let shadow = unsafe { read_volatile(addr_of!((*cpu).tpidr_el1)) };
    check(back == value && shadow == value, "TPIDR_EL1 write and read (XNU's current thread)");
    check(el == 4, "CurrentEL reads as EL1");
    check(irq_masked(cpu) == 1, "the boot CPU starts with interrupts masked");
    // SAFETY: the interrupt mask, which the host emulates as cpu->irq_masked.
    unsafe { asm!("msr daifclr, #2", options(nostack)) };
    let unmasked = irq_masked(cpu) == 0;
    // SAFETY: as above.
    unsafe { asm!("msr daifset, #2", options(nostack)) };
    check(unmasked && irq_masked(cpu) == 1, "msr daifclr/daifset drive cpu->irq_masked");
    let back: u64;
    // SAFETY: an EL1 register the host shadows; tlbi, which it accepts.
    unsafe {
        asm!("msr vbar_el1, {}", in(reg) value, options(nostack));
        asm!("mrs {}, vbar_el1", out(reg) back, options(nostack));
        asm!("tlbi vmalle1", options(nostack));
    }
    check(back == value, "another EL1 register reads back what was written (shadow)");
    check(true, "tlbi is accepted");

    // 3. Interrupts: the timer by host call, taken from idle. Filled at run time: this program has no fixups, so a
    // statically initialised function pointer would hold its link address (XNU rebases its own chained fixups after
    // entry).
    static UPCALLS: Racy<XhUpcalls> = Racy::new(XhUpcalls { irq: None, exception: None, cpu_entry: None });
    // SAFETY: vCPU 0 alone writes it, before the host reads it.
    unsafe {
        let u = UPCALLS.get();
        write_volatile(addr_of_mut!((*u).irq), Some(irq_upcall));
        write_volatile(addr_of_mut!((*u).cpu_entry), Some(secondary_entry));
    }
    check(host!(set_upcalls(cpu, UPCALLS.get())) == 0, "set_upcalls");
    host!(irq_enable(cpu, XH_INTID_VTIMER, 1));
    host!(irq_enable(cpu, 5, 1));
    let hz = inf.timebase_hz;
    let idle_until = |done: &dyn Fn() -> bool, limit: u64| {
        let start = now();
        while !done() && now() - start < limit {
            host!(cpu_idle(cpu));
            if irq_pending(cpu) {
                take_irqs(cpu);
            }
        }
    };
    let start = now();
    host!(timer_set(cpu, start + hz / 100));
    idle_until(&|| TIMER_FIRES.load(Relaxed) != 0, hz * 5);
    let elapsed = now() - start;
    let fired = TIMER_FIRES.load(Relaxed) == 1 && LAST_INTID.load(Relaxed) == XH_INTID_VTIMER && elapsed >= hz / 100;
    if !fired {
        put("  fires, intid, elapsed, hz: ");
        hex(TIMER_FIRES.load(Relaxed) as u64);
        put(" ");
        hex(LAST_INTID.load(Relaxed) as u64);
        put(" ");
        hex(elapsed);
        put(" ");
        hex(hz);
        put("\n");
    }
    check(fired, "the timer fires after its deadline and wakes cpu_idle");
    check(host!(irq_ack(cpu)) == XH_INTID_NONE && !irq_pending(cpu), "the line is low again after end-of-interrupt");
    host!(ipi(cpu, 0, 5));
    let taken = host!(irq_take(cpu));
    let again = host!(irq_take(cpu));
    host!(irq_eoi(cpu, 5));
    check(
        taken == 5 && again == XH_INTID_NONE && host!(irq_take(cpu)) == XH_INTID_NONE && !irq_pending(cpu),
        "irq_take takes an SGI by host call (fix-mach-rpc), the line low again after its end-of-interrupt",
    );

    // 4. Interrupts taken while kernel code runs unmasked: five timer ticks during a computation.
    let (mut f0, mut f1) = (0.0, 0.0);
    let expect = work(3_000_000, &mut f0);
    REARM.store(4, Relaxed);
    TIMER_FIRES.store(0, Relaxed);
    host!(timer_set(cpu, now() + hz / 1000));
    set_irq_masked(cpu, 0);
    let got = work(core::hint::black_box(3_000_000), &mut f1);
    let start = now();
    while TIMER_FIRES.load(Relaxed) < 5 && now() - start < hz * 5 {
        cpu_yield();
    }
    set_irq_masked(cpu, 1);
    check(TIMER_FIRES.load(Relaxed) == 5, "five timer interrupts were delivered into running kernel code");
    check(got == expect && f0 == f1, "and the interrupted computation's integer and floating-point state survived");

    // 4b. A frame resumed unmasked while the line is up is interrupted again at once (an exception return with a
    // pending interrupt): the timer is made due masked, then the host is asked to resume a frame of this very
    // context, unmasked.
    TIMER_FIRES.store(0, Relaxed);
    host!(timer_set(cpu, now())); // due now; masked, so only the line rises
    let start = now();
    while !irq_pending(cpu) && now() - start < hz {
        cpu_yield();
    }
    static RF: Racy<XhFrame> = Racy::new(XhFrame {
        x: [0; 31],
        sp: 0,
        pc: 0,
        pstate: 0,
        fpsr: 0,
        fpcr: 0,
        v: [XhVreg { lo: 0, hi: 0 }; 32],
        irq_masked: 0,
        kind: 0,
        far: 0,
        esr: 0,
        signo: 0,
        sigcode: 0,
    });
    let rf = RF.get();
    zero(rf, size_of::<XhFrame>());
    let resume = need(v.frame_resume);
    // SAFETY: the frame holds this context (pc after the call, sp, the callee-saved registers, x18 as it is); the
    // host resumes it at label 1 with every other register as the zeroed frame says, which the clobbers declare.
    unsafe {
        asm!(
            "adr x9, 1f", "str x9, [x10, #256]", "mov x9, sp", "str x9, [x10, #248]", "str x29, [x10, #232]",
            "str x19, [x10, #152]", "str x20, [x10, #160]", "str x21, [x10, #168]", "str x22, [x10, #176]",
            "str x23, [x10, #184]", "str x24, [x10, #192]", "str x25, [x10, #200]", "str x26, [x10, #208]",
            "str x27, [x10, #216]", "str x28, [x10, #224]", "str x18, [x10, #144]",
            "mov x0, x11", "mov x1, x10", "blr x12", "1:",
            // (the inputs in registers of their own: x0, x1 and x9 are the scratch the clobbers cover)
            in("x10") rf, in("x11") cpu, in("x12") resume,
            out("v8") _, out("v9") _, out("v10") _, out("v11") _, out("v12") _, out("v13") _, out("v14") _,
            out("v15") _, clobber_abi("C"),
        );
    }
    let resumed = TIMER_FIRES.load(Relaxed); // the upcall ran before the first instruction after the resume
    set_irq_masked(cpu, 1);
    check(resumed == 1, "frame_resume of an unmasked frame with the line up takes the interrupt at once");

    // 5. The timer through the registers ArmCpu writes.
    TIMER_FIRES.store(0, Relaxed);
    let deadline = now() + hz / 200;
    // SAFETY: the virtual timer's registers, which the host emulates.
    unsafe {
        asm!("msr cntv_cval_el0, {}", in(reg) deadline, options(nostack));
        asm!("msr cntv_ctl_el0, {}", in(reg) 1u64, options(nostack));
    }
    idle_until(&|| TIMER_FIRES.load(Relaxed) != 0, hz * 5);
    check(TIMER_FIRES.load(Relaxed) == 1, "CNTV_CVAL_EL0/CNTV_CTL_EL0 arm the same timer");
    let iar: u64;
    // SAFETY: ICC_IAR1_EL1, which the host emulates.
    unsafe { asm!("mrs {}, S3_0_C12_C12_0", out(reg) iar, options(nostack)) };
    check(iar == XH_INTID_NONE as u64, "ICC_IAR1_EL1 reads no pending interrupt");

    // 6. Other CPUs: by host call, and by PSCI CPU_ON as ArmCpu calls it.
    check(host!(cpu_start(cpu, 1, 0, 0x1000)) == 0, "cpu_start of CPU 1");
    idle_until(&|| SGI_SEEN.load(Relaxed) & (1 << 5) != 0, hz * 5);
    check(
        SECONDARY_SEEN.load(Relaxed) == 0x1001 && SGI_SEEN.load(Relaxed) & (1 << 5) != 0,
        "CPU 1 ran its entry and its SGI reached CPU 0",
    );
    check(host!(cpu_start(cpu, 1, 0, 0)) < 0, "a CPU cannot be started twice");
    let psci_result: u64;
    // SAFETY: PSCI CPU_ON by hvc, as ArmCpu issues it; the host starts vCPU 2 at psci_entry's physical address.
    unsafe {
        asm!("hvc #0", inlateout("x0") XH_PSCI_CPU_ON as u64 => psci_result, in("x1") 2u64,
             in("x2") kpa(psci_entry as *const () as u64), in("x3") 0xabcdefu64, options(nostack));
    }
    let start = now();
    while PSCI_SEEN.load(Relaxed) == 0 && now() - start < hz * 5 {
        cpu_yield();
    }
    check(
        psci_result == 0 && PSCI_SEEN.load(Relaxed) == 0xabcdef,
        "PSCI CPU_ON (hvc) starts CPU 2 at a physical entry address",
    );
    check(PSCI_OK.load(Relaxed) != 0, "and gives it the vector in x1 and its xh_cpu in x2");

    // 6b. The kernel's own address space: a RAM page mapped at a kernel virtual address outside the window.
    check(
        v.size as usize >= offset_of!(XhVector, kernel_unmap) + 8
            && inf.kva_size >= 1 << 30
            && inf.kva_base >= inf.ram_virt + inf.ram_size
            && inf.kva_base + inf.kva_size == 0x3c_0000_0000,
        "the kernel address space reservation",
    );
    let kpage = page_alloc();
    let kv = inf.kva_base + 0x1000_0000;
    check(host!(kernel_map(cpu, kv, 0x4000, XH_PROT_READ | XH_PROT_WRITE, kpage)) == 0, "kernel_map");
    write64(kv, 0x600d_cafe);
    check(read64(kva(kpage)) == 0x600d_cafe, "the mapping and the window show the same page");
    check(
        host!(kernel_map(cpu, inf.ram_virt, 0x4000, XH_PROT_READ, kpage)) < 0
            && host!(kernel_map(cpu, kv, 0x4000, XH_PROT_READ, 0x1000)) < 0,
        "kernel_map refuses addresses outside the reservation and outside RAM",
    );
    check(
        host!(kernel_unmap(cpu, kv, 0x4000)) == 0
            && host!(kernel_map(cpu, kv + 0x4000, 0x4000, XH_PROT_READ, kpage)) == 0
            && read64(kv + 0x4000) == 0x600d_cafe,
        "kernel_unmap, and the page mapped again elsewhere",
    );

    // 6c. A device page the kernel mapped itself: the fault's frame handed to mmio_emulate (what the exception upcall
    // does when its page-table entry points into the I/O aperture).
    static MCODE: Racy<[u32; 2]> = Racy::new([0; 2]);
    let mcode = MCODE.get() as *mut u32;
    // SAFETY: the code words live in the kernel's image (its text for the host's decoder).
    unsafe {
        write_volatile(mcode, 0xb940_0023); // ldr w3, [x1]
        write_volatile(mcode.add(1), 0xb800_4422); // str w2, [x1], #4
    }
    let mpc = mcode as u64;
    zero(rf, size_of::<XhFrame>());
    // SAFETY: the frame is the test's own (no vCPU runs it).
    let mf = unsafe { &mut *rf };
    mf.pc = mpc;
    mf.x[3] = !0;
    mf.x[1] = 0x1000;
    check(
        v.size as usize >= offset_of!(XhVector, mmio_emulate) + 8
            && host!(mmio_emulate(cpu, mf, 0x0800_0000 + 0xffe8)) == 0
            && (mf.x[3] >> 4 & 15) == 3
            && mf.x[3] >> 32 == 0
            && mf.pc == mpc + 4,
        "mmio_emulate: a load from GICD_PIDR2 on a frame, pc advanced",
    );
    mf.x[2] = 1 << 9; // GICD_ISENABLER1: enable SPI 41
    check(
        host!(mmio_emulate(cpu, mf, 0x0800_0000 + 0x104)) == 0
            && mf.x[1] == 0x1004
            && mf.pc == mpc + 8
            && host!(mmio_read(cpu, 0x0800_0000 + 0x104, 4)) & (1 << 9) != 0,
        "mmio_emulate: a store with writeback reaches the device",
    );
    host!(mmio_write(cpu, 0x0800_0000 + 0x184, 4, 1 << 9));
    mf.pc = mpc;
    check(
        host!(mmio_emulate(cpu, mf, 0x1000)) < 0 && mf.pc == mpc,
        "mmio_emulate refuses an address outside the aperture",
    );

    // 7. Devices and tasks.
    test_devices();
    let mut text = [0xd53b_d063u32, 0xd53b_d040]; // mrs x3, tpidrro_el0; mrs x0, tpidr_el0
    check(
        host!(text_fixup(cpu, text.as_mut_ptr().cast(), 8)) == 2 && text[0] == 0xd53b_d043 && text[1] == 0xd280_0000,
        "text_fixup: TPIDRRO_EL0 reads become TPIDR_EL0 reads, TPIDR_EL0 reads (the CPU number) become 0",
    );
    let scratch = page_alloc();
    write64(kva(scratch), 5);
    if inf.features & XH_FEATURE_RAM_FIXED != 0 {
        // The RAM held for the session (xnuhost's default since 2026-11-13, kernel 0067): nothing goes back.
        check(
            host!(ram_discard(cpu, scratch, 0x4000)) == -1
                && read64(kva(scratch)) == 5
                && host!(ram_discard(cpu, scratch + 0x800, 0x4000)) == -1
                && read64(kva(scratch)) == 5,
            "ram_discard is refused while the RAM is held fixed (-EPERM), and the page keeps its data",
        );
    } else {
        check(
            host!(ram_discard(cpu, scratch, 0x4000)) == 0 && read64(kva(scratch)) == 0,
            "ram_discard gives pages back (they read as zero)",
        );
        // A run of three pages and its neighbours: only the run reads zero; refused: unaligned, empty, outside the RAM.
        let run = page_alloc();
        for _ in 0..4 {
            page_alloc();
        }
        for i in 0..5 {
            write64(kva(run + i * 0x4000), 0x70 + i);
        }
        let zeroed = host!(ram_discard(cpu, run + 0x4000, 3 * 0x4000)) == 0
            && (1..4).all(|i| read64(kva(run + i * 0x4000)) == 0);
        check(
            zeroed && read64(kva(run)) == 0x70 && read64(kva(run + 4 * 0x4000)) == 0x74,
            "ram_discard of a run of pages: the run reads zero, the pages beside it keep their data",
        );
        check(
            host!(ram_discard(cpu, run + 0x800, 0x4000)) == -22
                && host!(ram_discard(cpu, run, 0x800)) == -22
                && host!(ram_discard(cpu, run, 0)) == -22
                && host!(ram_discard(cpu, inf.ram_phys - 0x4000, 0x4000)) == -22
                && host!(ram_discard(cpu, inf.ram_phys + inf.ram_size + (1 << 30), 0x4000)) == -22
                && read64(kva(run)) == 0x70,
            "ram_discard refuses an unaligned range, an empty one and one outside the RAM (-EINVAL), and changes nothing",
        );
    }
    check(host!(wall_time(cpu)) > 1_700_000_000, "wall_time");
    tasks::test_tasks();
    tasks::test_jit();
    // boot-args "testkernel-bench" (`make bench-arm64`): the trap path's costs, after the checks.
    if let Some(i) = (0..len).find_map(|c| word_at(line, c, b"testkernel-bench")) {
        bench::bench(scale_at(line, i));
    }

    let failures = FAILURES.load(Relaxed);
    put(if failures != 0 { "TESTKERNEL: FAILED\n" } else { "TESTKERNEL: all passed\n" });
    halt(cpu, if failures != 0 { XH_HALT_PANIC } else { XH_HALT_OFF }, failures)
}

/// A panic is a failed test kernel (no code path should reach one): said, then the panic halt.
#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    put("TESTKERNEL: FAILED (panic)\n");
    halt(c0(), XH_HALT_PANIC, FAILURES.load(Relaxed) + 1)
}
