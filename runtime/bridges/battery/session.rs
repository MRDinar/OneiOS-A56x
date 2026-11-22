// SPDX-License-Identifier: MIT
//! The battery bridge's session (battery.c's second half): the worker that reads while iOS listens, and the treble
//! backend's health HAL.

use super::{read_raw, Raw, Sample, Units, MAX_SUPPLIES, POLL_MS};
use crate::hal::{self, HealthInfo};
use crate::hw::{self, Backend, Bridge};
use crate::{br_error, br_info};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// The listener; runs on the worker thread, without the bridge's locks, for each change.
pub type Listener = Arc<dyn Fn(&Sample) + Send + Sync>;

/// How the bridge opens. [`Options::from_env`] is what xnuhost opens with (C: `br_battery_open(NULL)`).
#[derive(Clone, Debug, Default)]
pub struct Options {
    /// None: /sys/class/power_supply
    pub dir: Option<String>,
    /// Comma list; None: usb,ac,wireless,pogo (ONEIOS_BATTERY_SUPPLIES)
    pub supplies: Option<String>,
    /// 0: POLL_MS
    pub poll_ms: u32,
    /// 0: DEFAULT_MAH (ONEIOS_BATTERY_MAH)
    pub default_mah: u32,
    /// 'u' or 'm' fixes current_now's unit; None learns it (ONEIOS_BATTERY_CURRENT_UNIT)
    pub current_unit: Option<char>,
    /// Samsung, Treble; Unset: the process's hw-backend
    pub backend: Backend,
    /// Treble: the binder device; None: ONEIOS_BINDER, else /dev/binder
    pub binder: Option<String>,
}

impl Options {
    /// The phone's paths and ONEIOS_BATTERY_SUPPLIES, ONEIOS_BATTERY_MAH, ONEIOS_BATTERY_CURRENT_UNIT.
    pub fn from_env() -> Options {
        let var = |name| std::env::var(name).ok();
        Options {
            supplies: var("ONEIOS_BATTERY_SUPPLIES"),
            // C's strtoul(text, NULL, 0)
            default_mah: var("ONEIOS_BATTERY_MAH").map_or(0, |t| super::parse_c_prefix(&t) as u32),
            current_unit: var("ONEIOS_BATTERY_CURRENT_UNIT").and_then(|t| t.chars().next()),
            ..Options::default()
        }
    }
}

/// Where the samples come from.
enum Source {
    /// samsung: the power_supply nodes.
    Nodes { dir: String, supplies: Vec<String> },
    /// treble: the health HAL.
    Hal(hal::Hal),
}

impl Source {
    /// One reading: the nodes, or one getHealthInfo call (healthd's HAL reads the same nodes on its side). Err: the
    /// HAL's errno, or 0 for nodes that cannot be read.
    fn read(&self) -> Result<Raw, i32> {
        match self {
            Source::Nodes { dir, supplies } => read_raw(dir, supplies).ok_or(0),
            Source::Hal(h) => {
                h.call(hal::HEALTH_GET_INFO, &hal::parcel(hal::HEALTH), HealthInfo::decode).map(|i| from_health(&i))
            }
        }
    }
}

/// android.hardware.health.IHealth/default's HealthInfo as the nodes would give it: the level, the status's text,
/// present, tenths of a degree, mV, uA (Battery::open fixes the unit), uAh, the cycles, plugged = any charger online.
pub fn from_health(h: &HealthInfo) -> Raw {
    const NAMES: [&str; 6] = ["", "Unknown", "Charging", "Discharging", "Not charging", "Full"];
    let positive = |v: i32| (v > 0).then_some(i64::from(v));
    Raw {
        status: (if (1..=5).contains(&h.status) { NAMES[h.status as usize] } else { "Unknown" }).to_owned(),
        capacity: Some(h.level.into()),
        present: Some(h.present.into()),
        temp: Some(h.temp_deci_c.into()),
        voltage: positive(h.voltage_mv),
        current: Some(h.current_ua.into()),
        charge_full: positive(h.full_uah),
        charge_full_design: positive(h.design_uah),
        cycles: (h.cycles >= 0).then_some(h.cycles.into()),
        plugged: h.ac || h.usb || h.wireless || h.dock,
    }
}

struct State {
    quit: bool,
    read_failed: bool,
    units: Units,
    listener: Option<Listener>,
    sent: Sample,
    sent_at: Instant,
}

struct Shared {
    source: Source,
    poll: Duration,
    default_mah: u32,
    state: Mutex<State>,
    wake: Condvar,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Shared {
    fn dir(&self) -> &str {
        match &self.source {
            Source::Nodes { dir, .. } => dir,
            Source::Hal(_) => "",
        }
    }

    /// With the lock held. Keeps the last good sample when the nodes cannot be read.
    fn sample_now(&self, s: &mut State) -> Result<Sample, Sample> {
        match self.source.read() {
            Ok(raw) => {
                s.read_failed = false;
                Ok(raw.map(&mut s.units, self.default_mah))
            }
            Err(rc) => {
                if !s.read_failed {
                    if matches!(self.source, Source::Hal(_)) {
                        br_error!(
                            "battery: the health HAL's getHealthInfo failed ({}): iOS keeps the last level",
                            hal::error_text(rc)
                        );
                    } else {
                        br_error!("battery: {}/battery/capacity cannot be read: iOS keeps the last level", self.dir());
                    }
                }
                s.read_failed = true;
                Err(s.sent)
            }
        }
    }

    fn worker(&self) {
        let mut s = lock(&self.state);
        while !s.quit {
            if s.listener.is_none() {
                s = self.wake.wait(s).unwrap_or_else(PoisonError::into_inner);
                continue;
            }
            s = self.wake.wait_timeout(s, self.poll).unwrap_or_else(PoisonError::into_inner).0;
            if s.quit || s.listener.is_none() {
                continue;
            }
            let Ok(sample) = self.sample_now(&mut s) else { continue };
            let since = u32::try_from(s.sent_at.elapsed().as_millis()).unwrap_or(u32::MAX);
            if !s.sent.changed(&sample, since) {
                continue;
            }
            s.sent = sample;
            s.sent_at = Instant::now();
            let listener = s.listener.clone();
            drop(s);
            if let Some(f) = listener {
                f(&sample); // takes the bridge's lock, which listen()'s caller may hold
            }
            s = lock(&self.state);
        }
    }
}

/// An open bridge; dropping it (or [`Battery::close`]) stops the worker.
pub struct Battery {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

impl Battery {
    /// None (with a log line) when there is no battery to read.
    pub fn open(o: &Options) -> Option<Battery> {
        let dir = o.dir.clone().unwrap_or_else(|| "/sys/class/power_supply".into());
        let supplies: Vec<String> = o
            .supplies
            .as_deref()
            .unwrap_or("usb,ac,wireless,pogo")
            .split(',')
            .filter(|s| !s.is_empty())
            .take(MAX_SUPPLIES)
            .map(str::to_owned)
            .collect();
        let poll_ms = if o.poll_ms != 0 { o.poll_ms } else { POLL_MS };
        let fixed = matches!(o.current_unit, Some('u' | 'm'));
        let mut units = Units { current_fixed: fixed, current_ua: o.current_unit == Some('u') };

        let mut found = None;
        if hw::select(Bridge::Battery, o.backend) == Backend::Treble {
            let why = match hal::Hal::open(o.binder.as_deref(), hal::HEALTH_INSTANCE, hal::HEALTH) {
                Ok(h) => {
                    let source = Source::Hal(h);
                    match source.read() {
                        Ok(raw) => {
                            found = Some((source, raw));
                            None
                        }
                        Err(rc) => Some(format!("{}: getHealthInfo: {}", hal::HEALTH_INSTANCE, hal::error_text(rc))),
                    }
                }
                Err(why) => Some(why),
            };
            if let Some(why) = why {
                if !hw::missing(Bridge::Battery, &why) {
                    return None;
                }
            }
        }
        if found.is_some() {
            units = Units { current_fixed: true, current_ua: true }; // HealthInfo's current is in microamps by definition
        }
        let (source, raw) = match found {
            Some(f) => f,
            None => {
                let Some(raw) = read_raw(&dir, &supplies) else {
                    br_info!("battery: off: no readable {dir}/battery/capacity");
                    return None;
                };
                (Source::Nodes { dir: dir.clone(), supplies }, raw)
            }
        };
        let default_mah = o.default_mah;
        let sent = raw.map(&mut units, default_mah);
        let hal_version = match &source {
            Source::Hal(h) => Some(h.version()),
            Source::Nodes { .. } => None,
        };
        let shared = Arc::new(Shared {
            source,
            poll: Duration::from_millis(poll_ms.into()),
            default_mah,
            state: Mutex::new(State {
                quit: false,
                read_failed: false,
                units,
                listener: None,
                sent,
                sent_at: Instant::now(),
            }),
            wake: Condvar::new(),
        });
        let worker = Arc::clone(&shared);
        let Ok(thread) = thread::Builder::new().name("battery".into()).spawn(move || worker.worker()) else {
            br_error!("battery: off: no thread");
            return None;
        };
        let plugged = if sent.plugged { "plugged in" } else { "on battery" };
        match hal_version {
            Some(version) => br_info!(
                "battery: on: the health HAL {} (version {version}), status \"{}\", {}%, {plugged}; asked every {poll_ms} \
                 ms while iOS listens",
                hal::HEALTH_INSTANCE,
                raw.status,
                sent.percent
            ),
            None => br_info!(
                "battery: on: {dir}, status \"{}\", {}%, {plugged}; read every {poll_ms} ms while iOS listens",
                raw.status,
                sent.percent
            ),
        }
        Some(Battery { shared, thread: Some(thread) })
    }

    /// Reads the nodes now. The result counts as sent: the worker reports changes from it. Err: the last good sample.
    pub fn query(&self) -> Result<Sample, Sample> {
        let mut s = lock(&self.shared.state);
        let sample = self.shared.sample_now(&mut s)?;
        s.sent = sample;
        s.sent_at = Instant::now();
        Ok(sample)
    }

    /// The listener runs on the worker thread for each change. None stops the reads.
    pub fn listen(&self, listener: Option<Listener>) {
        lock(&self.shared.state).listener = listener;
        self.shared.wake.notify_all();
    }

    /// Treble when the health HAL is read, Samsung for the nodes.
    pub fn backend(&self) -> Backend {
        match self.shared.source {
            Source::Hal(_) => Backend::Treble,
            Source::Nodes { .. } => Backend::Samsung,
        }
    }

    /// Stops the worker (the same as dropping it).
    pub fn close(self) {}
}

impl Drop for Battery {
    fn drop(&mut self) {
        lock(&self.shared.state).quit = true;
        self.shared.wake.notify_all();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
