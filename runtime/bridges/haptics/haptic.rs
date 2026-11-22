// SPDX-License-Identifier: MIT
//! The haptics bridge (C: haptics/haptic.c): iOS vibration requests onto the Galaxy S24 Ultra's actuator.
//!
//! The actuator is an LRA driven by a Cirrus CS40L26A (I2C 0x40) under Samsung's sec_vibrator_inputff:
//! /dev/input/event8 "sec_vibrator_inputff", EV_FF bits FF_PERIODIC, FF_SINE, FF_CUSTOM, FF_GAIN and no FF_RUMBLE
//! (read on the owner's phone; docs/hardware.md). The driver's FF_SINE path programs the chip's buzz generator: period
//! 4..10 ms (100..250 Hz), magnitude 0..255, length in ms (cs40l26_sine_upload, drivers/vibrator/cs/cs40l26/cs40l26.c in
//! Samsung's kernel source). FF_GAIN takes 0..10000 (Samsung's intensity scale, gain / 100 = percent).
//!
//! One effect is kept and updated for every request. A worker thread does the ioctls (each is I2C traffic), so a
//! request from a vCPU never waits; the newest request wins.
//!
//! Two backends (hw-backend-haptics): samsung, the evdev device above (unchanged); treble, the vibrator HAL
//! android.hardware.vibrator.IVibrator/default through servicemanager: on(ms), setAmplitude (when the HAL has amplitude
//! control), off(). The same worker, mapping and counters drive both.

use crate::hw::{self, Backend, Bridge};
use std::os::fd::{AsRawFd, OwnedFd};
use std::sync::atomic::{AtomicI8, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use xh_core::sys::strerror;

pub const PERIOD_MIN_MS: u32 = 4; // CS40L26_BUZZGEN_PER_MIN
pub const PERIOD_MAX_MS: u32 = 10; // CS40L26_BUZZGEN_PER_MAX
/// 166 Hz: a guess at the LRA's resonance until f0 is read on the phone (attribute f0_measured of the class
/// sec_vib_inputff); whole milliseconds only.
pub const RESONANCE_MS: u32 = 6;
pub const LEVEL_MAX: u32 = 255; // CS40L26_BUZZGEN_LEVEL_MAX
/// Below this an LRA does not move enough to feel.
pub const LEVEL_FLOOR: u32 = 40;
pub const LENGTH_MAX_MS: u32 = 10000;

/// The mapping: iOS intensity (0..1000) and frequency (0 = resonance) to the effect's period (ms) and magnitude.
pub fn map(intensity: u32, frequency_hz: u32) -> (u32, u32) {
    let intensity = intensity.min(1000);
    let period = (1000 + frequency_hz / 2).checked_div(frequency_hz).unwrap_or(RESONANCE_MS);
    let period = period.clamp(PERIOD_MIN_MS, PERIOD_MAX_MS);
    // Any intensity above zero is felt: the floor up to full scale, linear.
    let magnitude = if intensity != 0 { LEVEL_FLOOR + (LEVEL_MAX - LEVEL_FLOOR) * intensity / 1000 } else { 0 };
    (period, magnitude)
}

/// An explicit semantic impact (one single tap, not a 20/50 ms fragment of a long vibration). Only offered after
/// opt-in and a successful native HAL preset inventory; LIGHT/MEDIUM mappings are experimental, only HEAVY has a
/// tactile S26 acceptance.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Impact {
    Light = 1,
    Medium = 2,
    Heavy = 3,
}

impl Impact {
    pub fn from_kind(kind: u32) -> Option<Impact> {
        match kind {
            1 => Some(Impact::Light),
            2 => Some(Impact::Medium),
            3 => Some(Impact::Heavy),
            _ => None,
        }
    }
}

/// A positive errno.
pub type Errno = i32;

/// What drives the actuator. Every call runs on the bridge's worker thread; dropping it closes it.
pub trait HapticBackend: Send {
    /// Uploads (or updates, `id` >= 0) the one effect: a sine of `period_ms` and `magnitude` (0..255) for `length_ms`.
    /// The effect id.
    fn upload(&mut self, id: i32, length_ms: u32, period_ms: u32, magnitude: u32) -> Result<i32, Errno>;
    fn play(&mut self, id: i32, on: bool) -> Result<(), Errno>;
    /// 0..10000 (Samsung's scale). Default: nothing to set.
    fn gain(&mut self, _gain: u32) -> Result<(), Errno> {
        Ok(())
    }
    /// Whether [`HapticBackend::impact`] does anything.
    fn has_impact(&self) -> bool {
        false
    }
    /// An EXPLICIT single impact: its duration_ms (1..1000).
    fn impact(&mut self, _kind: Impact) -> Result<u32, Errno> {
        Err(libc::EOPNOTSUPP)
    }
}

/// What reached this side: requests from the guest (vibrate, stop, and those a newer one replaced before the worker
/// started them) and what the device then did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HapticStats {
    pub vib: u64,
    pub stop: u64,
    pub replaced: u64,
    pub played: u64,
    pub stopped: u64,
    pub failed: u64,
    pub impacts: u64,
}

// Native impacts (docs/hardware.md 5): -1, ONEIOS_HAPTIC_NATIVE_IMPACTS decides (bridgectl, a manual run); 0 or 1,
// xnuhost's haptics-native-impacts setting does (set_native_impacts, before the bridge opens: no setenv in xnuhost).
static NATIVE_IMPACTS: AtomicI8 = AtomicI8::new(-1);

/// Whether a bridge opened after this asks for native impacts: Some (xnuhost's haptics-native-impacts), or None (the
/// default): the environment's ONEIOS_HAPTIC_NATIVE_IMPACTS=1 decides, as for bridgectl.
pub fn set_native_impacts(on: Option<bool>) {
    NATIVE_IMPACTS.store(on.map_or(-1, i8::from), Ordering::Release);
}

pub(crate) fn native_impacts_wanted() -> bool {
    match NATIVE_IMPACTS.load(Ordering::Acquire) {
        -1 => std::env::var("ONEIOS_HAPTIC_NATIVE_IMPACTS").is_ok_and(|v| v == "1"),
        set => set == 1,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Request {
    Vib,
    Stop,
    Impact,
}

struct State {
    quit: bool,
    pending: bool,
    busy: bool,
    on_ms: u32,
    intensity: u32,
    frequency_hz: u32,
    impact_kind: u32,
    request: Request,
    stats: HapticStats,
}

struct Shared {
    state: Mutex<State>,
    wake: Condvar,
    idle: Condvar,
    /// what plays: the device node, or "the stand-in actuator"
    name: String,
    /// HAL preset inventory + explicit opt-in
    impact_enabled: bool,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn report(&self) {
        let s = self.lock().stats;
        crate::br_info!(
            "haptics: counters: {} requests from the guest ({} vibrate, {} stop; {} replaced before they started), {} \
             effects played, {} stopped, {} failed on {} ({} discrete impacts)",
            s.vib + s.stop + s.impacts,
            s.vib,
            s.stop,
            s.replaced,
            s.played,
            s.stopped,
            s.failed,
            self.name,
            s.impacts
        );
    }
}

/// The worker's own state: the device's side.
struct Device {
    backend: Box<dyn HapticBackend>,
    /// -1 until the first upload
    effect: i32,
    playing: bool,
    active: Option<Request>,
    /// bound off(): never cancel an already-finished native tick
    impact_until: Instant,
    failures: u32,
}

impl Device {
    fn effect_id(&self) -> i32 {
        self.effect.max(0)
    }

    /// Stops what plays; Ok(true) when the device was told.
    fn stop(&mut self) -> Result<bool, Errno> {
        self.backend.play(self.effect_id(), false)?;
        self.playing = false;
        self.active = None;
        Ok(true)
    }
}

fn worker(shared: Arc<Shared>, mut dev: Device) {
    let mut st = shared.lock();
    loop {
        while !st.pending && !st.quit {
            st.busy = false;
            shared.idle.notify_all();
            st = shared.wake.wait(st).unwrap_or_else(PoisonError::into_inner);
        }
        if st.quit && !st.pending {
            break;
        }
        let (on_ms, intensity, frequency, request, kind) =
            (st.on_ms, st.intensity, st.frequency_hz, st.request, st.impact_kind);
        st.pending = false;
        st.busy = true;
        drop(st);

        let (period, magnitude) = map(intensity, frequency);
        let mut duration = 0;
        let mut step = "stop";
        let mut effect_id = dev.effect;
        let (mut played, mut stopped) = (false, false);
        // HAL preset effects finish by themselves. A later guest STOP must never issue off() after the vendor's
        // reported duration, because One UI could already be playing a different vibration.
        if dev.playing && dev.active == Some(Request::Impact) && Instant::now() >= dev.impact_until {
            dev.playing = false;
            dev.active = None;
        }
        let length = on_ms.min(LENGTH_MAX_MS);
        let rc: Result<(), Errno> = if request == Request::Stop
            || (request == Request::Vib && (on_ms == 0 || magnitude == 0))
        {
            let rc =
                if dev.playing { dev.backend.play(dev.effect_id(), false).map(|()| stopped = true) } else { Ok(()) };
            dev.playing = false;
            dev.active = None;
            rc
        } else if request == Request::Impact {
            step = "native impact";
            (|| {
                let kind = Impact::from_kind(kind).filter(|_| shared.impact_enabled && dev.backend.has_impact());
                let kind = kind.ok_or(libc::EOPNOTSUPP)?;
                // A continuous buzz is explicitly stopped before the new discrete tap.
                if dev.playing && dev.active == Some(Request::Vib) {
                    stopped = dev.stop()?;
                }
                duration = dev.backend.impact(kind)?;
                if !(1..=1000).contains(&duration) {
                    return Err(libc::EPROTO);
                }
                dev.playing = true;
                played = true;
                dev.active = Some(Request::Impact);
                dev.impact_until = Instant::now() + Duration::from_millis(duration.into());
                Ok(())
            })()
        } else {
            step = "effect upload";
            (|| {
                // Switching from a currently running preset back to a continuous waveform cancels that preset;
                // otherwise no spurious off() call.
                if dev.playing && dev.active == Some(Request::Impact) {
                    stopped = dev.stop()?;
                }
                let id = dev.backend.upload(dev.effect, length, period, magnitude)?;
                dev.effect = id;
                effect_id = id;
                step = "effect play";
                let rc = dev.backend.play(id, true);
                dev.playing = rc.is_ok();
                played = dev.playing;
                dev.active = played.then_some(Request::Vib);
                rc
            })()
        };
        if let Err(e) = rc {
            if dev.failures < 8 {
                crate::br_error!(
                    "haptics: {step} failed: {} ({on_ms} ms, intensity {intensity}, impact {kind}, period {period} ms, \
                     level {magnitude}, effect id {effect_id})",
                    strerror(e)
                );
            }
            dev.failures += 1;
        }
        let n = {
            let mut st = shared.lock();
            st.stats.played += u64::from(played);
            st.stats.stopped += u64::from(stopped);
            st.stats.failed += u64::from(rc.is_err());
            st.stats.played
        };
        let notable = n <= 8 || n.is_power_of_two();
        if played && notable {
            if request == Request::Impact {
                crate::br_info!(
                    "haptics: native impact {kind} played on {}: HAL duration {duration} ms (effect {n})",
                    shared.name
                );
            } else {
                crate::br_info!(
                    "haptics: effect {n} played on {}: {length} ms, period {period} ms, level {magnitude} (intensity \
                     {intensity}, {frequency} Hz asked)",
                    shared.name
                );
            }
        }
        if played && n > 8 && n.is_power_of_two() {
            shared.report();
        }
        st = shared.lock();
    }
    st.busy = false;
    shared.idle.notify_all();
    drop(st);
    drop(dev); // the backend closes (the evdev device's effects are erased, the HAL's off() when it is owed)
}

/// The haptic bridge: a worker over one backend. Dropping it logs the counter line and stops the motor.
pub struct Haptic {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for Haptic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Haptic").field("name", &self.shared.name).finish_non_exhaustive()
    }
}

impl Haptic {
    /// The bridge on a caller's backend (tests, another actuator). Custom/fake backends opt in to impacts explicitly
    /// (ONEIOS_HAPTIC_NATIVE_IMPACTS or set_native_impacts); None when the worker cannot start.
    pub fn open_backend(backend: Box<dyn HapticBackend>) -> Option<Haptic> {
        let impacts = backend.has_impact() && native_impacts_wanted();
        Haptic::start(backend, "the back end", impacts)
    }

    fn start(mut backend: Box<dyn HapticBackend>, name: &str, impact_enabled: bool) -> Option<Haptic> {
        if let Err(e) = backend.gain(10000) {
            crate::br_error!("haptics: initial gain setting failed: {}", strerror(e));
        }
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                quit: false,
                pending: false,
                busy: false,
                on_ms: 0,
                intensity: 0,
                frequency_hz: 0,
                impact_kind: 0,
                request: Request::Stop,
                stats: HapticStats::default(),
            }),
            wake: Condvar::new(),
            idle: Condvar::new(),
            name: name.to_owned(),
            impact_enabled,
        });
        let dev =
            Device { backend, effect: -1, playing: false, active: None, impact_until: Instant::now(), failures: 0 };
        let for_worker = Arc::clone(&shared);
        let thread = std::thread::Builder::new().name("haptics".into()).spawn(move || worker(for_worker, dev)).ok()?;
        Some(Haptic { shared, thread: Some(thread) })
    }

    /// Finds the force-feedback device under `dev_dir` (None: /dev/input) and starts the worker; the backend as
    /// hw-backend-haptics says. None when there is none or it cannot be opened (the reason is logged).
    pub fn open(dev_dir: Option<&str>) -> Option<Haptic> {
        Haptic::open_as(dev_dir, Backend::Unset, None)
    }

    /// The same with the backend chosen (Samsung: the evdev device; Treble: the vibrator HAL on `binder`, None:
    /// ONEIOS_BINDER or /dev/binder; Unset: hw-backend-haptics).
    pub fn open_as(dev_dir: Option<&str>, backend: Backend, binder: Option<&str>) -> Option<Haptic> {
        if hw::select(Bridge::Haptics, backend) == Backend::Treble {
            match super::vibrator::open_hal(binder) {
                Ok(h) => return Some(h),
                Err(why) if !hw::missing(Bridge::Haptics, &why) => return None,
                Err(_) => {}
            }
        }
        open_evdev(dev_dir)
    }

    /// The stand-in actuator (haptics=sim, a PC boot): every request is taken and counted, nothing vibrates.
    pub fn open_sim() -> Option<Haptic> {
        let h = Haptic::start(Box::new(SimBackend), "the stand-in actuator", false)?;
        crate::br_info!(
            "haptics: on: a stand-in actuator (haptics=sim): requests are counted and logged, nothing vibrates"
        );
        Some(h)
    }

    /// A backend's bridge with its name and impact choice (the vibrator HAL's open).
    pub(crate) fn open_named(backend: Box<dyn HapticBackend>, name: &str, impact_enabled: bool) -> Option<Haptic> {
        Haptic::start(backend, name, impact_enabled)
    }

    fn ask(&self, set: impl FnOnce(&mut State)) {
        let mut st = self.shared.lock();
        if st.pending {
            st.stats.replaced += 1; // the newest wins: the one not started is never played
        }
        set(&mut st);
        st.pending = true;
        self.shared.wake.notify_one();
    }

    /// Vibrate from now for on_ms (0 = stop) at intensity 0..1000 and frequency_hz (0 = resonance). Never blocks: the
    /// newest request replaces one that has not started.
    pub fn vib(&self, on_ms: u32, intensity: u32, frequency_hz: u32) {
        self.ask(|st| {
            if on_ms != 0 {
                st.stats.vib += 1;
            } else {
                st.stats.stop += 1;
            }
            (st.on_ms, st.intensity, st.frequency_hz) = (on_ms, intensity, frequency_hz);
            st.request = if on_ms != 0 && intensity != 0 { Request::Vib } else { Request::Stop };
        });
    }

    pub fn stop(&self) {
        self.vib(0, 0, 0);
    }

    pub fn impacts_available(&self) -> bool {
        self.shared.impact_enabled
    }

    /// An explicit semantic impact (1 light, 2 medium, 3 heavy) on the same queue as vib and stop. Err(EOPNOTSUPP)
    /// when impacts are not offered or the kind is not one.
    pub fn impact(&self, kind: u32) -> Result<(), Errno> {
        if !self.impacts_available() || Impact::from_kind(kind).is_none() {
            return Err(libc::EOPNOTSUPP);
        }
        self.ask(|st| {
            st.stats.impacts += 1;
            st.impact_kind = kind;
            st.on_ms = 0;
            st.request = Request::Impact;
        });
        Ok(())
    }

    /// Waits until the worker has carried out everything asked so far (tests, shutdown).
    pub fn flush(&self) {
        let mut st = self.shared.lock();
        while st.pending || st.busy {
            st = self.shared.idle.wait(st).unwrap_or_else(PoisonError::into_inner);
        }
    }

    pub fn stats(&self) -> HapticStats {
        self.shared.lock().stats
    }

    /// Logs the "haptics: counters:" line.
    pub fn report(&self) {
        self.shared.report();
    }

    /// What plays: the device node, "the vibrator HAL", "the stand-in actuator", "the back end".
    pub fn name(&self) -> &str {
        &self.shared.name
    }
}

impl Drop for Haptic {
    fn drop(&mut self) {
        self.report(); // the session's figures, before the closing stop counts as one
        self.stop(); // the motor never outlives the bridge
        {
            let mut st = self.shared.lock();
            st.quit = true;
            self.shared.wake.notify_one();
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

// ---- the stand-in actuator (haptics=sim: a PC boot, which has no vibrator) --------------------------------
// It takes every upload and play like the evdev device and keeps nothing but the counters, so the guest's whole path
// (its Actuator device, oneios_haptic.c, the bridge channel) runs and the log shows what would have played.
struct SimBackend;

impl HapticBackend for SimBackend {
    fn upload(&mut self, id: i32, _length_ms: u32, _period_ms: u32, _magnitude: u32) -> Result<i32, Errno> {
        Ok(id.max(0))
    }
    fn play(&mut self, _id: i32, _on: bool) -> Result<(), Errno> {
        Ok(())
    }
}

// ---- the evdev force-feedback back end ----------------------------------------------------------
const EV_FF: u16 = 0x15;
const FF_PERIODIC: u16 = 0x51;
const FF_SINE: u16 = 0x5a;
const FF_GAIN: u16 = 0x60;
const FF_MAX: usize = 0x7f;

/// struct ff_effect (linux/input.h) with its union as struct ff_periodic_effect: 48 bytes on a 64-bit kernel.
#[repr(C)]
#[derive(Default)]
struct FfEffect {
    kind: u16,
    id: i16,
    direction: u16,
    trigger_button: u16,
    trigger_interval: u16,
    replay_length: u16,
    replay_delay: u16,
    _pad: u16,
    waveform: u16,
    period: u16,
    magnitude: i16,
    offset: i16,
    phase: u16,
    envelope: [u16; 4],
    custom_len: u32,
    custom_data: usize,
}

const fn ioc(dir: u64, nr: u64, size: usize) -> u64 {
    dir << 30 | (size as u64) << 16 | (b'E' as u64) << 8 | nr
}
const IOC_WRITE: u64 = 1;
const IOC_READ: u64 = 2;
const fn eviocgbit(ev: u16, len: usize) -> u64 {
    ioc(IOC_READ, 0x20 + ev as u64, len)
}
const fn eviocgname(len: usize) -> u64 {
    ioc(IOC_READ, 0x06, len)
}
const EVIOCSFF: u64 = ioc(IOC_WRITE, 0x80, std::mem::size_of::<FfEffect>());

/// ioctl(fd, request, arg) -> -1 and errno, or its result.
fn ioctl<T>(fd: &OwnedFd, request: u64, arg: *mut T) -> Result<i32, Errno> {
    // SAFETY: the callers pass a request whose size matches the buffer `arg` points to.
    let rc = unsafe { libc::ioctl(fd.as_raw_fd(), request as _, arg) };
    if rc < 0 {
        Err(xh_core::sys::errno())
    } else {
        Ok(rc)
    }
}

fn open_rw(path: &str) -> Result<OwnedFd, Errno> {
    std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .map(OwnedFd::from)
        .map_err(|e| e.raw_os_error().unwrap_or(libc::EIO))
}

fn bit(bits: &[u8], n: u16) -> bool {
    bits[usize::from(n) / 8] >> (n % 8) & 1 != 0
}

fn has_periodic(fd: &OwnedFd) -> bool {
    let mut bits = [0u8; FF_MAX / 8 + 1];
    ioctl(fd, eviocgbit(EV_FF, bits.len()), bits.as_mut_ptr()).is_ok() && bit(&bits, FF_PERIODIC) && bit(&bits, FF_SINE)
}

fn device_name(fd: &OwnedFd) -> String {
    let mut name = [0u8; 80];
    let _ = ioctl(fd, eviocgname(name.len()), name.as_mut_ptr());
    let end = name.iter().position(|&b| b == 0).unwrap_or(name.len());
    String::from_utf8_lossy(&name[..end]).into_owned()
}

/// The evdev backend's device search: the node path of the first device whose EV_FF bits have FF_PERIODIC and
/// FF_SINE, Samsung's sec_vibrator_inputff first.
pub fn find(dev_dir: Option<&str>) -> Option<String> {
    let dev_dir = dev_dir.unwrap_or("/dev/input");
    let mut best = None;
    for entry in std::fs::read_dir(dev_dir).ok()?.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.starts_with("event") {
            continue;
        }
        let node = format!("{dev_dir}/{name}");
        let Ok(fd) = open_rw(&node) else { continue };
        if !has_periodic(&fd) {
            continue;
        }
        // Samsung's wrapper first; any other periodic device otherwise.
        if device_name(&fd) == "sec_vibrator_inputff" {
            return Some(node);
        }
        best.get_or_insert(node);
    }
    best
}

struct EvdevBackend {
    fd: OwnedFd, // closing it erases the file's effects
}

impl EvdevBackend {
    fn event(&self, code: u16, value: i32) -> Result<(), Errno> {
        // SAFETY: an all-zero input_event is a valid one.
        let mut ev: libc::input_event = unsafe { std::mem::zeroed() };
        ev.type_ = EV_FF;
        ev.code = code;
        ev.value = value;
        let size = std::mem::size_of::<libc::input_event>();
        // SAFETY: `ev` is a valid input_event of `size` bytes.
        let n = unsafe { libc::write(self.fd.as_raw_fd(), (&ev as *const libc::input_event).cast(), size) };
        if n == size as isize {
            Ok(())
        } else {
            Err(xh_core::sys::errno())
        }
    }
}

impl HapticBackend for EvdevBackend {
    fn upload(&mut self, id: i32, length_ms: u32, period_ms: u32, magnitude: u32) -> Result<i32, Errno> {
        let mut effect = FfEffect {
            kind: FF_PERIODIC,
            id: id as i16,
            replay_length: length_ms as u16,
            waveform: FF_SINE,
            period: period_ms as u16,
            magnitude: magnitude as i16,
            ..FfEffect::default()
        };
        ioctl(&self.fd, EVIOCSFF, &mut effect)?;
        Ok(effect.id.into())
    }
    fn play(&mut self, id: i32, on: bool) -> Result<(), Errno> {
        self.event(id as u16, i32::from(on))
    }
    fn gain(&mut self, gain: u32) -> Result<(), Errno> {
        self.event(FF_GAIN, gain as i32)
    }
}

fn open_evdev(dev_dir: Option<&str>) -> Option<Haptic> {
    let Some(node) = find(dev_dir) else {
        crate::br_error!(
            "haptics: off: no force-feedback device with FF_PERIODIC + FF_SINE under {}",
            dev_dir.unwrap_or("/dev/input")
        );
        return None;
    };
    let fd = match open_rw(&node) {
        Ok(fd) => fd,
        Err(e) => {
            crate::br_error!("haptics: off: {node}: {}", strerror(e));
            return None;
        }
    };
    let h = Haptic::start(Box::new(EvdevBackend { fd }), &node, false)?;
    crate::br_info!(
        "haptics: on: {node} (sine effects, period {PERIOD_MIN_MS}..{PERIOD_MAX_MS} ms, level {LEVEL_FLOOR}..{LEVEL_MAX})"
    );
    Some(h)
}

#[cfg(test)]
pub(crate) fn ff_effect_size() -> usize {
    std::mem::size_of::<FfEffect>()
}
