// SPDX-License-Identifier: MIT
//! Port of main.c: the session (docs/booter.md, oneos/interface.md): the machine's assembly from the config, the main
//! loop and its timers, the signals, the stop and the return.
//!
//! What the other modules call: [`stop`], [`stopping`], [`mono_ms`] (any thread), [`guest_ready`] (the kernel's
//! ready host call) and [`guest_frame`] (virtio-gpu, a frame of the guest's own). [`main`] is the program.

mod board;
mod bridges;
mod host;
mod memory;

use std::fs::OpenOptions;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use xh_core::text::truncate;
use xh_core::time::monotonic_ms;
use xh_core::{loge, logi};
use xnuhost_abi::{XH_HALT_OFF, XH_HALT_PANIC, XH_HALT_RESTART, XH_HALT_RETURN};

use crate::blk::{self, Blk};
use crate::config::{self, Config, READY_CONTENT_WAIT};
use crate::imageset::{DiskIdentity, ImageSet};
use crate::ready::{ReadyState, ReadyWhy};
use crate::ret::{ReturnDecision, ReturnState};
use crate::usb::{self, PropSource, UsbWatch};
use crate::{cpu, display, input, input_script, mem, perf, sysreg, task};

/// The exit codes (docs/booter.md, oneos/interface.md 5).
pub const EXIT_GUEST_OFF: i32 = 0;
pub const EXIT_SETUP: i32 = 2;
pub const EXIT_RESTART: i32 = 3;
pub const EXIT_PANIC: i32 = 4;

/// The finished config of the running session (set once, when the kernel is placed).
static CONFIG: OnceLock<Config> = OnceLock::new();

/// The session's config; None before the kernel was placed (the tests, a tool).
pub fn config() -> Option<&'static Config> {
    CONFIG.get()
}

// ---- the stop ------------------------------------------------------------------------------------------------------

/// Why the machine stops: the first request wins.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StopRequest {
    pub reason: u32,
    pub code: u32,
    pub why: String,
}

static STOP_REQUESTED: AtomicBool = AtomicBool::new(false);
static STOP: Mutex<Option<StopRequest>> = Mutex::new(None);

/// xh_stop: asks the machine to stop (any thread; returns). Only the first request is kept.
pub fn stop(reason: u32, code: u32, why: &str) {
    let mut s = STOP.lock().unwrap_or_else(|e| e.into_inner());
    if s.is_none() {
        // what C kept of it: 95 bytes
        *s = Some(StopRequest { reason, code, why: truncate(why, 95).to_owned() });
        STOP_REQUESTED.store(true, Ordering::Release);
    }
}

/// xh_stopping: a stop was asked.
pub fn stopping() -> bool {
    STOP_REQUESTED.load(Ordering::Acquire)
}

/// The stop that was asked, if any.
pub fn stop_request() -> Option<StopRequest> {
    STOP.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// CLOCK_MONOTONIC in whole seconds (the C's tv_sec comparisons of the periodic lines).
fn mono_s() -> u64 {
    monotonic_ms() / 1000
}

// ---- the session files (oneos/interface.md 3) ----------------------------------------------------------------------

/// The session folder, "" for none: set at the start.
static SESSION_DIR: OnceLock<String> = OnceLock::new();
/// ready-content-wait, set at the start.
static READY_WAIT: AtomicU32 = AtomicU32::new(READY_CONTENT_WAIT);

/// Writes `<session>/<name>` (0600, replaced whole); nothing without a session folder.
fn session_file(name: &str, content: &str) {
    let Some(dir) = SESSION_DIR.get().filter(|d| !d.is_empty()) else { return };
    let path = format!("{dir}/{name}");
    if path.len() >= 600 {
        return;
    }
    let file = OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&path);
    match file {
        Ok(mut f) => {
            let _ = f.write_all(content.as_bytes());
        }
        Err(e) => loge!("session: {}: {}", path, xh_core::sys::error_text(&e)),
    }
}

/// A whole file of at most `limit` bytes (None: unreadable, empty or larger).
fn read_file(path: &str, limit: usize) -> Option<Vec<u8>> {
    let f = std::fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    if len == 0 || len > limit as u64 {
        return None;
    }
    let mut data = Vec::with_capacity(len as usize);
    std::io::Read::read_to_end(&mut &f, &mut data).ok()?;
    (data.len() as u64 == len).then_some(data)
}

// ---- the guest is up -----------------------------------------------------------------------------------------------
// session/ready (the switch's dead-man timer stops; the OneOS boot screen hands over to the display helper) and the
// `guest ready: first frame` line, which the fork's boot stages and oneios-tool look for. When, is ready.rs's rule: the
// first frame of the guest's own with content, or ready-content-wait seconds after its first one. The device's thread
// (guest_frame), the main loop's tick and the ready host call all come through here.

static READY: Mutex<ReadyState> = Mutex::new(ReadyState { first_ms: 0, frames: 0, ready: false });
static GUEST_READY: AtomicBool = AtomicBool::new(false);
/// a frame of the guest's own has been shown: the console's copy stops
static GUEST_DRAWING: AtomicBool = AtomicBool::new(false);

fn ready_content_wait() -> u32 {
    READY_WAIT.load(Ordering::Relaxed)
}

/// Whether the guest is up (session/ready written).
pub fn is_guest_ready() -> bool {
    GUEST_READY.load(Ordering::Acquire)
}

/// Whether a frame of the guest's own was shown.
pub fn guest_drawing() -> bool {
    GUEST_DRAWING.load(Ordering::Acquire)
}

fn ready_now(rule: &ReadyState, why: ReadyWhy, now: u64) {
    if !why.now() || GUEST_READY.swap(true, Ordering::AcqRel) {
        return;
    }
    session_file("ready", "ready\n");
    let after = now.saturating_sub(rule.first_ms) as f64 / 1000.0;
    let frames = rule.frames;
    match why {
        ReadyWhy::Content if frames <= 1 => logi!("guest ready: first frame (it has content)"),
        ReadyWhy::Content => logi!(
            "guest ready: first frame with content, {:.1} s and {} frames after the guest's first (black) frame",
            after,
            frames - 1
        ),
        ReadyWhy::BlackTimeout => logi!(
            "guest ready: first frame, all black: no frame with content in {} s after the guest's first ({} frames); ready all the same (ready-content-wait)",
            ready_content_wait(),
            frames
        ),
        ReadyWhy::AnyFrame => logi!("guest ready: first frame (ready-content-wait=0: content not waited for)"),
        _ => logi!("guest ready: first frame (the kernel's ready call)"),
    }
}

/// xh_guest_ready: the kernel's ready host call.
pub fn guest_ready() {
    let mut rule = READY.lock().unwrap_or_else(|e| e.into_inner());
    let why = rule.on_call();
    ready_now(&rule, why, monotonic_ms());
}

/// xh_guest_frame: a frame of the guest's own was shown (virtio_gpu calls it before the frame goes to the display
/// helper, so that session/ready is there first); `lit`: pixels_lit of the display's frame. True once the guest is
/// ready, after which it need not be called again.
pub fn guest_frame(lit: bool) -> bool {
    let now = monotonic_ms();
    let wait = ready_content_wait();
    let mut rule = READY.lock().unwrap_or_else(|e| e.into_inner());
    let first = rule.frames == 0 && !rule.ready;
    let why = rule.on_frame(lit, wait, now);
    if first {
        GUEST_DRAWING.store(true, Ordering::Release);
        // The marker of the rule before 2026-11-15 (ready at this frame), kept for the timings.
        if !lit && why == ReadyWhy::Not {
            logi!(
                "display: the guest's first frame of its own: all black; session/ready waits for one with content (up to {} s)",
                wait
            );
        }
    }
    ready_now(&rule, why, now);
    rule.ready
}

/// The main loop's tick of the ready rule: session/ready for an iOS that only draws black.
fn ready_tick() {
    if is_guest_ready() {
        return;
    }
    let now = monotonic_ms();
    let mut rule = READY.lock().unwrap_or_else(|e| e.into_inner());
    let why = rule.on_tick(ready_content_wait(), now);
    ready_now(&rule, why, now);
}

// ---- the session ---------------------------------------------------------------------------------------------------

/// A setup step failed; its line is logged.
#[derive(Debug)]
pub struct BootFailed;

/// What the session holds from the boot to the stop.
#[derive(Default)]
struct Session {
    image_set: Option<ImageSet>,
    disk_identity: Vec<DiskIdentity>,
    seed_sha256: String,
    disks: Vec<Arc<Blk>>,
    /// the guest's display mode: width, height, refresh rate
    fb: (u32, u32, u32),
    boot_fb_pa: u64,
    /// the host's pen was given to the guest (its IC is woken with the touch IC)
    pen_ic: bool,
    memory: memory::MemoryWatch,
    bridges: bridges::Bridges,
    usb: UsbWatch,
    usb_source: Option<Box<dyn PropSource + Send>>,
    usb_last: Option<u64>,
    /// the console's last B,G,R,X frame, for a half-float display
    console_seen: Vec<u8>,
}

impl Session {
    /// Every usb-log seconds (3): the phone's USB gadget, logged at the start and whenever it changed (usb.rs). From
    /// the main thread, which is not a realtime one: three property serials and one named sysfs file a poll. Read only.
    fn usb_tick(&mut self, config: &Config, now: bool, when: &str) {
        if config.usb_log == 0 {
            return;
        }
        let t = mono_s();
        if !now && self.usb_last.is_some_and(|last| t - last < config.usb_log as u64) {
            return;
        }
        self.usb_last = Some(t);
        let src = self.usb_source.as_deref_mut().map(|s| s as &mut dyn PropSource);
        if self.usb.poll(src) || now {
            logi!("{}{}", self.usb.line, when);
        }
    }

    /// Until the guest's display driver presents a frame of its own, show what its kernel console draws (not until
    /// ready: between its first frame and ready the frames are the guest's, and the console must not be copied over
    /// them).
    fn console_tick(&mut self) {
        use xh_core::pixels::{bgrx8_to_rgba16f, BGRX8, RGBA16F};
        if is_guest_ready() || guest_drawing() {
            return;
        }
        let (w, h) = (self.fb.0 as usize, self.fb.1 as usize);
        let Some(d) = display::get() else { return };
        let Some(fb) = mem::ram(self.boot_fb_pa, (w * h * 4) as u64) else { return };
        // SAFETY: the boot framebuffer is guest RAM of w * h * 4 bytes (mem::ram); the kernel's console writes it while
        // this reads it, which can only tear the copy (the next tick copies it again).
        let fb = unsafe { std::slice::from_raw_parts(fb.as_ptr(), w * h * 4) };
        // SAFETY: until the guest shows a frame of its own (checked above) the console's copy is the frame's only
        // writer: the GPU device writes it only for the guest's frames.
        let pixels = unsafe { d.bytes_mut() };
        let stride = d.stride as usize;
        if d.format == BGRX8 {
            let n = (stride * d.height as usize).min(fb.len()).min(pixels.len());
            if pixels[..n] != fb[..n] {
                pixels[..n].copy_from_slice(&fb[..n]);
                display::flush(0, 0, d.width, d.height);
            }
        } else if d.format == RGBA16F && d.width as usize == w && d.height as usize == h {
            // A half-float frame (display-format=rgba16f): the console's B,G,R,X bytes are compared with the last ones
            // seen and decoded to linear light when they changed.
            if self.console_seen.len() != fb.len() {
                self.console_seen = vec![0; fb.len()];
            }
            if self.console_seen != fb {
                self.console_seen.copy_from_slice(fb);
                let len = pixels.len();
                for y in 0..h {
                    let row = &mut pixels[(y * stride).min(len)..(y * stride + w * 8).min(len)];
                    bgrx8_to_rgba16f(row, &self.console_seen[y * w * 4..(y + 1) * w * 4]);
                }
                display::flush(0, 0, d.width, d.height);
            }
        }
    }
}

/// A guest restart (PSCI SYSTEM_RESET, XH_HALT_RESTART: iOS restarting itself, or the kernel starting the Data volume
/// over for a new seed) is not the end of the session: for the OneOS fork xnuhost exiting means "back to One UI".
/// xnuhost starts itself again in place, same pid, cgroup and namespace: exec ends every vCPU thread, the tasks went
/// with task::shutdown, and every descriptor but the log is closed (the RAM memfd would otherwise stay alive).
/// XNUHOST_RESTARTS counts them: a guest that restarts 8 times in a row without ever getting ready is left, with exit
/// 3. Returns only when it could not start again.
fn guest_restart(argv: &[String]) {
    use std::os::unix::process::CommandExt;
    let counted = std::env::var("XNUHOST_RESTARTS").ok();
    let mut n = counted.map_or(0, |c| xh_core::parse::leading_i64(&c, 10).map_or(0, |(v, _)| v));
    if is_guest_ready() {
        n = 0;
    }
    if n >= 8 {
        loge!("the guest restarted {} times without getting ready: not starting it again", n);
        return;
    }
    std::env::set_var("XNUHOST_RESTARTS", (n + 1).to_string());
    logi!("the guest asked for a restart: starting again ({})", n + 1);
    let log = xh_core::log::fd();
    for fd in (3..4096).filter(|&fd| fd != log) {
        // SAFETY: closing descriptors before an exec of this program; nothing of ours uses them past this point.
        unsafe { libc::close(fd) };
    }
    // XNUHOST_RUNNER: the emulator xnuhost itself runs under ("qemu-aarch64 -cpu cortex-a710"), which a PC with no
    // binfmt entry needs in front of the program; unset on the phone.
    let runner = std::env::var("XNUHOST_RUNNER").unwrap_or_default();
    let words: Vec<&str> = runner.split(' ').filter(|w| !w.is_empty()).take(8).collect();
    if !words.is_empty() && words.len() + argv.len() < 63 {
        let _ = std::process::Command::new(words[0]).args(&words[1..]).args(argv).exec();
    }
    let e = std::process::Command::new("/proc/self/exe").arg0(&argv[0]).args(&argv[1..]).exec();
    loge!("restart: exec /proc/self/exe: {}", xh_core::sys::error_text(&e));
}

/// The return signals and SIGQUIT.
fn stop_signals() -> libc::sigset_t {
    // SAFETY: sigemptyset/sigaddset on a local set.
    unsafe {
        let mut s: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut s);
        for sig in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP, libc::SIGQUIT] {
            libc::sigaddset(&mut s, sig);
        }
        s
    }
}

fn hostcalls_vector() -> u64 {
    crate::hostcalls::vector().map_or(0, |v| v as *const _ as u64)
}

/// The program (main.c's main): the exit code.
pub fn main() -> i32 {
    let argv: Vec<String> = std::env::args_os().map(|a| a.to_string_lossy().into_owned()).collect();
    #[cfg(all(target_os = "android", target_arch = "aarch64"))]
    {
        // Before any thread exists: bionic gives each new thread of a program with target SDK >= 31 its own APIA key.
        // The kernel's vCPUs and the tasks need one key set (host.rs, shared_by_threads).
        extern "C" {
            fn android_set_application_target_sdk_version(target: u32);
        }
        // SAFETY: bionic's call, made while the process has one thread.
        unsafe { android_set_application_target_sdk_version(30) };
    }
    let mut config = Config::new();
    if let Err(e) = config.args(&argv).and_then(|()| config.finish_with(&crate::ramfix::can_lock)) {
        e.print();
        return EXIT_SETUP;
    }
    xh_core::log::open(None, config.log_level);
    let _ = SESSION_DIR.set(config.session.clone());
    READY_WAIT.store(config.ready_content_wait, Ordering::Relaxed);
    logi!("xnuhost: session ABI {}, mode {}, pid {}", config.session_abi, config.mode, std::process::id());
    xh_core::virtio::set_log(true); // virtio's health lines (docs/booter.md 2.9b)
                                    // The host phone (onebridges::hw, config.rs): what was found, from which property, and each default it changed.
                                    // Its environment defaults (the S26 Ultra's ONEIOS_BRIGHT_MAX) are set here, while this process has one thread; a
                                    // variable the session already set wins.
    if !config.host_line.is_empty() {
        logi!("{}", config.host_line);
    }
    if let Some(h) = config.host {
        onebridges::hw::host_env_apply(h.0);
    }
    host::host_facts();
    let mut s = Session { usb: UsbWatch::new(""), usb_source: usb::bionic(), ..Session::default() };
    if config.selftest {
        return host::selftest(&config);
    }
    memory::ram_choice_log(&config);
    s.usb_tick(&config, true, "");

    // Order matters: the RAM memfd and the task factory come first, while this process is still single-threaded and
    // has nothing of the guest mapped.
    mem::map_limit();
    if mem::create(config.ram_bytes).is_err() {
        return EXIT_SETUP;
    }
    if config.tasks && task::init(&config).is_err() {
        return EXIT_SETUP;
    }

    // Signals: the main thread waits for the stop signals; every thread started from here on inherits them blocked.
    // vCPU threads open the ones they take themselves.
    let mut blocked = stop_signals();
    // SAFETY: the real-time signals the vCPUs use, added to a local set; this thread's mask changed; SIGPIPE ignored (a
    // helper's socket that closed is an error return, not a death).
    unsafe {
        for sig in libc::SIGRTMIN()..=libc::SIGRTMIN() + 3 {
            libc::sigaddset(&mut blocked, sig);
        }
        libc::pthread_sigmask(libc::SIG_BLOCK, &blocked, std::ptr::null_mut());
        libc::signal(libc::SIGPIPE, libc::SIG_IGN);
    }
    if !config.session.is_empty() {
        session_file("xnuhost.pid", &format!("{}\n", std::process::id()));
    }
    let Ok(fb) = display::init(&config) else { return EXIT_SETUP };
    s.fb = fb;
    // The display driver's scale and refresh rate go on the kernel's command line (a value given there stays).
    if config::display_boot_args(
        &mut config.boot_args,
        config::BOOT_ARGS_SIZE,
        config.scale,
        s.fb.2,
        config.display_format,
    )
    .is_err()
    {
        loge!("no room in the boot arguments for the display's switches");
        return EXIT_SETUP;
    }
    // 24A446's bluetoothd with the virtual machine hidden takes its chip path and dies (SIGSEGV at 0x350) in a loop
    // that launchd then throttles for 1200 s (phone and PC, 2026-11-14; docs/hardware.md 4): said, not changed. [I] Its
    // chip factory finds no chip for transport-encoding 1 outside a VM (only transport 9 or a product of its table gives
    // one), and 0x350 is a field of the chip object it then reads through a null pointer.
    if config::boot_args_hides_vmm_from(&config.boot_args, "bluetoothd") {
        loge!("boot arguments: owner-hide-vmm names bluetoothd: it crashes in a loop that way (docs/hardware.md 4); take it out");
    }
    // bt-transport-override=N replaces the transport-encoding bluetoothd reads. Since 2026-11-23 the guest's Bluetooth
    // driver gives it 10 (in a VM its power init passes only with 10) and the kernel adds BT_HCI_TRANSPORT=H4:POSIX to
    // its environment (hosted 0087), which keeps its HCI on our transport: so 10 changes nothing, any other N makes its
    // power init fail (PowerState 3, Bluetooth off). Without the kernel's variable (oneios-bt-hci=0, or a kernel before
    // 0087) 10 is BTVC, a simulated controller in bluetoothd's own process (main 3233fed, 2026-11-15: exit(1) every 56 s).
    match config::boot_args_bt_transport(&config.boot_args) {
        Some(10) => logi!("boot arguments: bt-transport-override=10 is what the guest's driver gives bluetoothd already; with oneios-bt-hci=0 (no BT_HCI_TRANSPORT) it is bluetoothd's own simulated controller (docs/hardware.md 4)"),
        Some(n) => loge!("boot arguments: bt-transport-override={}: bluetoothd's power init passes only with 10 in a VM, so Bluetooth stays off (docs/hardware.md 4); take it out", n),
        None => {}
    }
    perf::init(config.perf_log);
    let placed = match s.boot(&mut config) {
        Ok(p) => p,
        Err(BootFailed) => {
            task::shutdown();
            return EXIT_SETUP;
        }
    };
    let config: &'static Config = CONFIG.get_or_init(|| config);
    let block = cpu::vcpu(0).cpu.as_ptr() as u64;
    if cpu::start(0, placed.entry_va, placed.args_va, hostcalls_vector(), block).is_err() {
        task::shutdown();
        return EXIT_SETUP;
    }
    run(&mut s, config, &argv)
}

/// The main loop, the stop, the exit code.
fn run(s: &mut Session, config: &'static Config, argv: &[String]) -> i32 {
    let waited = stop_signals();
    // The return's graceful shutdown (docs/booter.md "The return"): the first stop signal, when iOS is up and
    // return-graceful-ms > 0, asks iOS to power off and keeps the vCPUs running up to an absolute deadline; then the
    // stop below runs as always. A panic (it sets stop directly, not through a signal), a second signal, the guest
    // halting itself, or the deadline all leave the wait at once. The way back never depends on any of this: the
    // deadline is ours, and the fork's SIGKILL is the backstop under everything.
    let mut ret = ReturnState::new();
    while !stopping() {
        let tick = libc::timespec { tv_sec: 0, tv_nsec: 100_000_000 };
        // SAFETY: waits up to 100 ms for one of the blocked stop signals.
        let signo = unsafe { libc::sigtimedwait(&waited, std::ptr::null_mut(), &tick) };
        if signo == libc::SIGQUIT {
            // debugging: where each vCPU is
            cpu::dump_all();
            task::dump();
            sysreg::stats_log();
            perf::log(true);
            continue;
        }
        if signo > 0 {
            match ret.on_signal(config.return_graceful_ms, is_guest_ready(), monotonic_ms()) {
                ReturnDecision::Graceful => {
                    let asked = bridges::request_shutdown();
                    logi!(
                        "return: graceful shutdown asked{}; iOS has up to {} ms",
                        if asked { "" } else { " (no bridge; nothing to ask, will time out)" },
                        config.return_graceful_ms
                    );
                }
                ReturnDecision::StopSecond => {
                    logi!("return: second request while shutting down; stopping now");
                    stop(XH_HALT_OFF, 0, "second return request");
                }
                ReturnDecision::StopNow => stop(XH_HALT_OFF, 0, &format!("signal {signo}")),
            }
        }
        if ret.expired(monotonic_ms()) {
            logi!("return: graceful shutdown timed out after {} ms; stopping", ret.elapsed_ms(monotonic_ms()));
            stop(XH_HALT_OFF, 0, "graceful shutdown timed out");
        }
        perf::tick(); // "perf:" lines every perf-log seconds
        blk::tick(); // "disk NAME: 30 s:" lines (blk.rs)
        s.memory.host_memory_tick(config, false); // "memory: host MemAvailable" every host-mem-log seconds
        s.memory.swap_watch(config); // "memory: the host swaps iOS's RAM" when it does (nothing while all is locked)
        s.memory.ram_valve_tick(); // the lock's valve, once a second while locked
        s.memory.huge_tick(config); // "memory: 2 MiB pages (the prefault done): ..." once
        s.usb_tick(config, false, ""); // "usb: state ..." when the USB gadget changed
        xh_core::virtio::tick(); // "virtio: 60 s: ..." and a queue that holds chains (xh_core::virtio)
        task::tick(); // "tasks: 60 s: ..." made, ended, died on their own (task.rs)
        s.bridges.haptic_tick(); // "haptics: 60 s: ..." when iOS used the motor
        ready_tick(); // session/ready for an iOS that only draws black (ready-content-wait)
        s.console_tick();
    }
    let mut request = stop_request().unwrap_or_default();
    // A return was asked: a guest-initiated restart in the window is iOS finishing its shutdown, not a reboot to re-run
    // in place. Take it as the return (exit 0), so the fork's watcher goes back to One UI instead of xnuhost re-exec'ing.
    if ret.active && request.reason == XH_HALT_RESTART {
        logi!("return: the guest restarted during the graceful shutdown; taking it as the return");
        request.reason = XH_HALT_OFF;
    }
    // The guest shut itself down inside the window (its kernel's PSCI SYSTEM_OFF or halt set the stop): say how long it
    // took. A timeout or a second request logged its own line above; a panic took the panic path (reason PANIC).
    if ret.active
        && request.reason != XH_HALT_PANIC
        && request.why != "graceful shutdown timed out"
        && request.why != "second return request"
    {
        logi!("return: iOS shut down in {} ms", ret.elapsed_ms(monotonic_ms()));
    }
    logi!("stopping: {} (reason {}, code {})", request.why, request.reason, request.code);
    s.memory.host_memory_tick(config, true);
    if mem::state().huge {
        s.memory.huge_report(" at the stop");
    }
    s.usb_tick(config, true, " (at the stop)");
    for (i, v) in cpu::vcpus().iter().enumerate().take(cpu::ncpu()) {
        logi!(
            "cpu{}: {}, {} interrupts ({} timer, {} IPI acknowledged, {} re-raised), {} device accesses, {} emulated instructions, {} task traps",
            i,
            if v.started.load(Ordering::Relaxed) { "started" } else { "never started" },
            v.stat.irqs.get(),
            v.stat.timer.get(),
            v.stat.sgi.get(),
            v.stat.reraised.get(),
            v.stat.mmio.get(),
            v.stat.sysreg.get(),
            v.stat.traps.get()
        );
    }
    sysreg::stats_log(); // which trapped instructions kept the vCPUs busy
    perf::log(true); // what the trap path cost, step by step (perf.rs)
    blk::report_all(); // what each disk did, its flushes and their time (blk.rs)
                       // Debugging aid: XNUHOST_DUMP=FILE writes the first 64 MiB of the kernel's window at the stop (XNUHOST_DUMP_MB=N for
                       // more: a kernel collection's data is past 64 MiB).
    if let Ok(path) = std::env::var("XNUHOST_DUMP") {
        let mb = std::env::var("XNUHOST_DUMP_MB").ok().and_then(|t| xh_core::parse::leading_u64(&t, 0).map(|(v, _)| v));
        let m = mem::state();
        if !m.host.is_null() {
            let n = m.size.min(mb.unwrap_or(64) << 20) as usize;
            // SAFETY: the window is mapped for the process's life; the vCPUs are stopped or stopping, a torn byte in a
            // debugging dump is acceptable.
            let bytes = unsafe { std::slice::from_raw_parts(m.host, n) };
            let _ = std::fs::write(path, bytes);
        }
    }
    task::shutdown();
    s.bridges.close();
    input_script::stop();
    input::stop();
    display::close();
    // SAFETY: sync has no preconditions: the Data store's pages.
    unsafe { libc::sync() };
    // The RAM goes back to the host at once with this process (its last mapping and descriptor: the tasks and the
    // factory are gone already); a kill does the same. Nothing to undo: mlock ends with the mapping.
    s.memory.stop();
    if request.reason == XH_HALT_RETURN {
        session_file("return.request", "the guest asked to return\n");
    }
    if request.reason == XH_HALT_RESTART {
        guest_restart(argv); // returns only if it could not
    }
    let code = if request.reason == XH_HALT_RESTART {
        EXIT_RESTART
    } else if request.reason == XH_HALT_PANIC || request.code != 0 {
        EXIT_PANIC
    } else {
        EXIT_GUEST_OFF
    };
    logi!("exit {}", code);
    code
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stop_keeps_the_first() {
        let long = "x".repeat(200);
        stop(XH_HALT_PANIC, 7, &long);
        stop(XH_HALT_OFF, 0, "later");
        let r = stop_request().unwrap();
        assert!(stopping() && r.reason == XH_HALT_PANIC && r.code == 7 && r.why.len() == 95);
    }
}
