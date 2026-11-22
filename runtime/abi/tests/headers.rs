// SPDX-License-Identifier: MIT
//! The Rust mirrors against the C headers they mirror: every constant, every struct's size and every field's offset
//! and size, and every constant of a header mirrored (a define added to a header and not to the Rust fails here).
//! Also the kernel's copies of the free-report headers (kernel patches 0058 and 0066 add them): line for line ours.

mod cheader;

use cheader::Header;
use std::collections::BTreeSet;
use std::mem::{offset_of, size_of};
use std::path::{Path, PathBuf};
use xnuhost_abi::*;

fn runtime() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
}

fn hostcall_h() -> Header {
    Header::read(&runtime().join("xnuhost/include/xnuhost/hostcall.h"), &[])
}

fn stub_abi_h(hostcall: &Header) -> Header {
    Header::read(&runtime().join("xnuhost/stub/stub_abi.h"), &[hostcall])
}

fn field_size<T, F>(_: fn(&T) -> &F) -> usize {
    size_of::<F>()
}

/// A struct's size and its fields' names, offsets and sizes, in order, against the C header's.
macro_rules! check_struct {
    ($h:expr, $c:literal, $t:ty, [$($f:ident),* $(,)?]) => {{
        let c = $h.layout($c);
        let rust = vec![$(cheader::Field {
            name: stringify!($f).to_string(),
            offset: offset_of!($t, $f),
            size: field_size(|s: &$t| &s.$f),
        }),*];
        assert_eq!(rust, c.fields, "struct {} against {}", $c, stringify!($t));
        assert_eq!(size_of::<$t>(), c.size, "sizeof(struct {})", $c);
    }};
}

/// Each constant's value against the header's, and the header's every constant among them.
macro_rules! check_consts {
    ($h:expr, [$($name:ident),* $(,)?]) => {{
        let h: &Header = &$h;
        let mut seen = BTreeSet::new();
        $(
            let c = *h.defines.get(stringify!($name)).unwrap_or_else(|| panic!("{} is not in the header", stringify!($name)));
            assert_eq!($name as u64, c, "{}", stringify!($name));
            seen.insert(stringify!($name).to_string());
        )*
        let unmirrored: Vec<_> = h.defines.keys().filter(|k| !seen.contains(*k)).collect();
        assert!(unmirrored.is_empty(), "constants of the header without a Rust mirror: {unmirrored:?}");
    }};
}

#[test]
fn hostcall_constants() {
    let h = hostcall_h();
    check_consts!(
        h,
        [
            XH_ABI_VERSION,
            XH_HINT_VALID,
            XH_HINT_BACKGROUND,
            XH_HINT_UI,
            XH_HINT_REALTIME,
            XH_FRAME_IRQ,
            XH_FRAME_EXCEPTION,
            XH_TRAP_SYSCALL,
            XH_TRAP_FAULT,
            XH_TRAP_ILLEGAL,
            XH_TRAP_BRK,
            XH_TRAP_FPE,
            XH_TRAP_INTERRUPT,
            XH_TRAP_EXITED,
            XH_TRAP_BOOT,
            XH_PROT_READ,
            XH_PROT_WRITE,
            XH_PROT_EXEC,
            XH_INTID_NONE,
            XH_INTID_VTIMER,
            XH_SGI_COUNT,
            XH_IPI_SPI_FIRST,
            XH_IPI_SPI_COUNT,
            XH_INTID_UNTAKEN,
            XH_HALT_OFF,
            XH_HALT_RESTART,
            XH_HALT_PANIC,
            XH_HALT_RETURN,
            XH_FEATURE_PAC,
            XH_FEATURE_TASKS,
            XH_FEATURE_PAC_KEYS_OFF,
            XH_FEATURE_RAM_FIXED,
            XH_PAC_ALL,
            XH_PSCI_VERSION,
            XH_PSCI_CPU_OFF,
            XH_PSCI_CPU_ON,
            XH_PSCI_SYSTEM_OFF,
            XH_PSCI_SYSTEM_RESET,
        ]
    );
    assert_eq!(h.macros.keys().collect::<Vec<_>>(), ["XH_HINT_FLOOR"]);
    for hint in [0, 1, 0x7ff, 0x800, XH_HINT_VALID | XH_HINT_UI | 512, u32::MAX] {
        assert_eq!(xh_hint_floor(hint) as u64, h.apply("XH_HINT_FLOOR", hint as u64), "XH_HINT_FLOOR({hint:#x})");
    }
    for reason in 0..5 {
        assert_eq!(HaltReason::from_raw(reason).map(HaltReason::raw), (reason <= XH_HALT_RETURN).then_some(reason));
    }
}

#[test]
fn hostcall_layouts() {
    let h = hostcall_h();
    #[rustfmt::skip]
    check_struct!(h, "xh_cpu", XhCpu, [index, irq_masked, irq_pending, in_host, kernel_tpidr, kernel_x18, host_tpidr,
        host_x18, host_sp, kernel_data, tpidr_el1, perf_hint, perf_reserved, reserved]);
    check_struct!(h, "xh_vreg", XhVreg, [lo, hi]);
    #[rustfmt::skip]
    check_struct!(h, "xh_frame", XhFrame, [x, sp, pc, pstate, fpsr, fpcr, v, irq_masked, kind, far, esr, signo,
        sigcode]);
    check_struct!(h, "xh_user_state", XhUserState, [x, sp, pc, pstate, tsd, fpsr, fpcr, v]);
    check_struct!(h, "xh_trap", XhTrap, [kind, signo, far, esr, svc, access, insn, exit_status]);
    #[rustfmt::skip]
    check_struct!(h, "xh_info", XhInfo, [abi, ncpu, host_page_size, user_va_bits, ram_phys, ram_virt, ram_size,
        io_phys, io_virt, io_size, user_va_min, user_va_limit, timebase_hz, features, video, kva_base, kva_size]);
    check_struct!(h, "xh_upcalls", XhUpcalls, [irq, exception, cpu_entry]);
    #[rustfmt::skip]
    check_struct!(h, "xh_vector", XhVector, [abi, size, info, console_write, halt, ready, set_upcalls, wall_time,
        cpu_start, cpu_idle, irq_ack, irq_eoi, irq_enable, ipi, timer_set, frame_resume, io_map, mmio_read,
        mmio_write, ram_discard, text_fixup, task_create, task_destroy, task_map, task_unmap, task_protect, task_read,
        task_write, thread_create, thread_destroy, thread_run, thread_set_pac, kernel_map, kernel_unmap,
        mmio_emulate, irq_take]);
    let mirrored: BTreeSet<&str> =
        ["xh_cpu", "xh_vreg", "xh_frame", "xh_user_state", "xh_trap", "xh_info", "xh_upcalls", "xh_vector"].into();
    assert_eq!(h.structs.keys().map(String::as_str).collect::<BTreeSet<_>>(), mirrored);
}

#[test]
fn stub_abi_constants() {
    let hostcall = hostcall_h();
    let h = stub_abi_h(&hostcall);
    check_consts!(
        h,
        [
            XS_REGION_SIZE,
            XS_TEXT_MAX,
            XS_CONFIG_OFF,
            XS_BOOTSTACK_OFF,
            XS_BOOTSTACK_SIZE,
            XS_SERVICE_OFF,
            XS_THREADS_OFF,
            XS_AREA_SIZE,
            XS_SLOT_SIZE,
            XS_SIGSTACK_OFF,
            XS_SIGSTACK_SIZE,
            XS_MAX_THREADS,
            XS_FILTER_MAX,
            XS_RUN,
            XS_EXIT,
            XS_OPS_OFF,
            XS_THREAD_OPS,
            XS_OP_MAP,
            XS_OP_UNMAP,
            XS_OP_PROTECT,
            XS_OP_CLONE,
            XS_OP_EXIT,
            XS_OPS_MAX,
            XS_MAP_POPULATE,
            XS_ESR_EC_SHIFT,
            XS_ESR_WNR,
            XS_ESR_CM,
        ]
    );
    assert!(h.macros.is_empty());
}

#[test]
fn stub_abi_layouts() {
    let hostcall = hostcall_h();
    let h = stub_abi_h(&hostcall);
    check_struct!(h, "xs_filter_insn", XsFilterInsn, [code, jt, jf, k]);
    #[rustfmt::skip]
    check_struct!(h, "xs_config", XsConfig, [base, ram_fd, trap_fd, service_slot, kick_signal, uid, filter_length,
        filter]);
    check_struct!(h, "xs_op", XsOp, [op, prot, va, size, offset, result]);
    #[rustfmt::skip]
    check_struct!(h, "xs_trap_page", XsTrapPage, [to_task, to_host, tid, command, pac_set, pac_enabled, booted,
        reserved, host_sleeping, task_sleeping, slept, nops, ops_done, reserved2, spin_ticks, t_enter, t_woken, trap,
        state]);
    check_struct!(h, "xs_service_page", XsServicePage, [request, done, ready, count, pid, setup_error, ops]);
    let own: BTreeSet<&str> =
        h.structs.keys().filter(|k| !hostcall.structs.contains_key(*k)).map(String::as_str).collect();
    assert_eq!(own, ["xs_filter_insn", "xs_config", "xs_op", "xs_trap_page", "xs_service_page"].into());
}

#[test]
fn free_report_headers_define_nothing_else() {
    for name in ["free_report.h", "free_report_runs.h"] {
        let h = Header::read(&runtime().join("xnuhost/include/xnuhost").join(name), &[]);
        assert!(h.defines.is_empty() && h.macros.is_empty() && h.structs.is_empty(), "{name}");
    }
}

/// The lines a patch adds to `file`, without the leading '+'.
fn added_lines(patch: &str, file: &str) -> Vec<String> {
    let mut lines = Vec::new();
    let mut inside = false;
    for line in patch.lines() {
        if line.starts_with("diff --git") {
            if inside {
                break;
            }
            inside = line.ends_with(&format!("b/{file}"));
            continue;
        }
        if inside && line.starts_with('+') && !line.starts_with("+++") {
            lines.push(line[1..].to_string());
        }
    }
    lines
}

// The kernel's copies (osfmk/arm64/hosted/free_report.h from 0058, free_report_runs.h from 0066) are our headers,
// line for line.
#[test]
fn free_report_headers_are_the_kernels() {
    for (patch, header) in
        [("0058-free-page-report.patch", "free_report.h"), ("0066-free-page-runs.patch", "free_report_runs.h")]
    {
        let patch_path = runtime().join("../kernel/patches/oneios-hosted").join(patch);
        let patch_text =
            std::fs::read_to_string(&patch_path).unwrap_or_else(|e| panic!("{}: {e}", patch_path.display()));
        let ours = std::fs::read_to_string(runtime().join("xnuhost/include/xnuhost").join(header)).unwrap();
        let theirs = added_lines(&patch_text, &format!("osfmk/arm64/hosted/{header}"));
        assert!(theirs.len() > 20, "{patch} adds {header}");
        assert_eq!(theirs, ours.lines().collect::<Vec<_>>(), "{patch}'s {header} against ours");
    }
}
