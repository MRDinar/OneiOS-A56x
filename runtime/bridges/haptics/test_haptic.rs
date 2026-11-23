// SPDX-License-Identifier: MIT
//! The haptics bridge's host tests (C: tests/test_bridges.c, test_haptic): the mapping, the worker on a fake motor,
//! the stand-in, the native impacts' opt-in.

use super::haptic::ff_effect_size;
use super::*;
use crate::hw::testdir::env_lock;
use std::sync::{Arc, Mutex};

#[derive(Default, Debug)]
pub(crate) struct Motor {
    pub uploads: u32,
    pub plays: u32,
    pub stops: u32,
    pub last_length: u32,
    pub last_period: u32,
    pub last_magnitude: u32,
    pub gain: u32,
    pub impacts: u32,
    pub last_kind: Option<Impact>,
    pub impact_ms: u32,
    pub next_id: i32,
}

/// A fake motor whose state the test reads.
pub(crate) struct FakeMotor(pub Arc<Mutex<Motor>>);

impl FakeMotor {
    pub(crate) fn new(motor: Motor) -> (Box<FakeMotor>, Arc<Mutex<Motor>>) {
        let m = Arc::new(Mutex::new(motor));
        (Box::new(FakeMotor(Arc::clone(&m))), m)
    }
}

impl HapticBackend for FakeMotor {
    fn upload(&mut self, id: i32, length_ms: u32, period_ms: u32, magnitude: u32) -> Result<i32, Errno> {
        let mut m = self.0.lock().unwrap();
        m.uploads += 1;
        (m.last_length, m.last_period, m.last_magnitude) = (length_ms, period_ms, magnitude);
        if id < 0 {
            m.next_id += 1;
            Ok(m.next_id - 1)
        } else {
            Ok(id)
        }
    }
    fn play(&mut self, _id: i32, on: bool) -> Result<(), Errno> {
        let mut m = self.0.lock().unwrap();
        if on {
            m.plays += 1;
        } else {
            m.stops += 1;
        }
        Ok(())
    }
    fn gain(&mut self, gain: u32) -> Result<(), Errno> {
        self.0.lock().unwrap().gain = gain;
        Ok(())
    }
    fn has_impact(&self) -> bool {
        true
    }
    fn impact(&mut self, kind: Impact) -> Result<u32, Errno> {
        let mut m = self.0.lock().unwrap();
        m.impacts += 1;
        m.last_kind = Some(kind);
        Ok(if m.impact_ms != 0 { m.impact_ms } else { 400 })
    }
}

#[test]
fn mapping() {
    assert_eq!(map(1000, 0), (6, 255));
    assert_eq!(map(0, 0).1, 0);
    let (_, m) = map(1, 0);
    assert!((40..60).contains(&m));
    let (p, m) = map(500, 230);
    assert!(p == 4 && m > 100 && m < 200);
    assert_eq!(map(5000, 20), (10, 255));
    assert_eq!(ff_effect_size(), 48); // struct ff_effect on a 64-bit kernel
}

#[test]
fn haptic() {
    let _env = env_lock(); // ONEIOS_HAPTIC_NATIVE_IMPACTS and set_native_impacts
    std::env::remove_var("ONEIOS_HAPTIC_NATIVE_IMPACTS");
    set_native_impacts(None);
    let (backend, motor) = FakeMotor::new(Motor { next_id: 3, ..Motor::default() });
    let h = Haptic::open_backend(backend).unwrap();
    let m = || motor.lock().unwrap();
    assert_eq!(m().gain, 10000);
    h.vib(400, 1000, 0);
    h.flush();
    assert!(m().uploads == 1 && m().plays == 1 && m().last_length == 400 && m().last_magnitude == 255);
    h.vib(60000, 500, 0); // longer than one effect may be
    h.flush();
    assert!(m().uploads == 2 && m().last_length == 10000);
    h.stop();
    h.flush();
    assert_eq!(m().stops, 1);
    h.stop(); // a stop with nothing playing does not touch the device
    h.flush();
    assert_eq!(m().stops, 1);
    h.vib(100, 0, 0); // intensity 0 is a stop
    h.flush();
    assert_eq!(m().uploads, 2);
    h.vib(100, 300, 0);
    h.flush();
    // The counter line's figures (fix-haptics): 4 vibrate requests (one of intensity 0), 2 stops asked, 3 effects
    // played, 1 stop that reached the device, nothing failed.
    let st = h.stats();
    assert!(st.vib == 4 && st.stop == 2 && st.played == 3 && st.stopped == 1 && st.failed == 0);
    drop(h); // close stops a running effect
    assert_eq!(m().stops, 2);

    // haptics=sim: the stand-in takes every request, plays it, and counts it; a stop of a playing effect counts too.
    let sim = Haptic::open_sim().unwrap();
    for _ in 0..20 {
        sim.vib(20, 600, 170);
        sim.flush();
    }
    sim.stop();
    sim.flush();
    let st = sim.stats();
    assert!(st.vib == 20 && st.played == 20 && st.stop == 1 && st.stopped == 1 && st.failed == 0 && st.replaced == 0);
    drop(sim);

    // Explicit native impacts are opt-in and use the same serialized worker. One isolated impact is not mapped to
    // repeated VIB segments.
    let (backend, motor) = FakeMotor::new(Motor { impact_ms: 400, ..Motor::default() });
    let m = || motor.lock().unwrap();
    let native = Haptic::open_backend(backend).unwrap();
    assert!(!native.impacts_available());
    assert_eq!(native.impact(Impact::Heavy as u32), Err(libc::EOPNOTSUPP));
    let backend = Box::new(FakeMotor(Arc::clone(&motor)));
    drop(native);
    std::env::set_var("ONEIOS_HAPTIC_NATIVE_IMPACTS", "1");
    let native = Haptic::open_backend(backend).unwrap();
    assert!(native.impacts_available());
    assert_eq!(native.impact(999), Err(libc::EOPNOTSUPP));
    assert_eq!(native.impact(Impact::Heavy as u32), Ok(()));
    native.flush();
    let st = native.stats();
    assert!(m().impacts == 1 && m().last_kind == Some(Impact::Heavy) && m().plays == 0);
    assert!(st.vib == 0 && st.stop == 0 && st.impacts == 1 && st.played == 1 && st.failed == 0);
    native.stop();
    native.flush();
    assert_eq!(m().stops, 1); // immediate stop cancels in-flight preset
    assert_eq!(native.impact(Impact::Light as u32), Ok(()));
    native.flush();
    let st = native.stats();
    assert!(m().impacts == 2 && m().last_kind == Some(Impact::Light) && st.impacts == 2 && st.played == 2);
    assert_eq!(st.failed, 0);
    // The preset's 400 ms run out (the C test slept 450 ms too: the worker's clock is what is tested).
    std::thread::sleep(std::time::Duration::from_millis(450));
    native.stop();
    native.flush();
    assert_eq!(m().stops, 1); // completed preset: do not interrupt One UI
    drop(native);
    assert_eq!(m().stops, 1);
    // xnuhost's haptics-native-impacts (set_native_impacts): false wins over the environment's 1, true needs no
    // environment, None hands the choice back to the environment.
    let fresh = || Box::new(FakeMotor(Arc::clone(&motor)));
    set_native_impacts(Some(false));
    let native = Haptic::open_backend(fresh()).unwrap();
    assert!(!native.impacts_available() && native.impact(Impact::Heavy as u32) == Err(libc::EOPNOTSUPP));
    drop(native);
    std::env::remove_var("ONEIOS_HAPTIC_NATIVE_IMPACTS");
    set_native_impacts(Some(true));
    let native = Haptic::open_backend(fresh()).unwrap();
    assert!(native.impacts_available());
    drop(native);
    set_native_impacts(None);
    let native = Haptic::open_backend(fresh()).unwrap();
    assert!(!native.impacts_available());
}

#[test]
fn shutdown_once() {
    // xnuhost's stop: the bridge is shut down through a shared reference (the bridge device holds it), once; requests
    // after it are counted and not played, and a flush does not wait for a worker that is gone.
    let (backend, motor) = FakeMotor::new(Motor::default());
    let h = Arc::new(Haptic::open_backend(backend).unwrap());
    h.vib(100, 500, 0);
    h.flush();
    h.shutdown();
    assert!(motor.lock().unwrap().plays == 1 && motor.lock().unwrap().stops == 1); // the motor stopped at the shutdown
    h.vib(100, 500, 0);
    h.flush();
    h.shutdown();
    assert_eq!(motor.lock().unwrap().plays, 1);
    assert_eq!(h.stats().vib, 2);
    drop(h);
    assert_eq!(motor.lock().unwrap().stops, 1);
}
