// SPDX-License-Identifier: MIT
//! The virtio devices' host tests (C: tests/test_bridges.c, test_bridge_device, test_bridge_native_impacts,
//! test_bridge_battery, test_bridge_brightness, test_net_device, test_snd_device): each device on xh-core's real
//! transport against a guest made of plain memory (xh_core::virtio::testing).

use super::*;
use crate::audio::{AudioBackend, NullAudio};
use crate::bt::hci::{Hci, HciBackend, HciState};
use crate::haptics::test_haptic::{FakeMotor, Motor};
use crate::haptics::{Haptic, Impact};
use crate::hw::testdir::{env_lock, TempDir};
use crate::net::{Net, NetBackend, NetConfig, RxFn};
use crate::proto::actuator::{oa_period, OaState, OA_IMPACT, OA_NONE, OA_STOP, OA_VIB};
use crate::proto::*;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use xh_core::mmio::MmioDevice;
use xh_core::virtio::testing::{install, Driver, GuestRam, RING_BYTES};

const QN: u16 = 16;
const BUF: usize = 0x10000; // the buffers after the rings of four queues

/// A guest: its memory (rings, then buffers) and its driver.
struct Guest {
    ram: GuestRam,
    drv: Driver,
}

impl Guest {
    fn up(dev: Arc<dyn MmioDevice>, nqueues: u32, features: u64) -> Guest {
        install();
        let ram = GuestRam::new(BUF + 0x40000);
        let drv = Driver::up(dev, features, nqueues, QN, ram.pa(0)).expect("the device comes up");
        assert!(4 * RING_BYTES as usize <= BUF);
        Guest { ram, drv }
    }

    /// The guest address of buffer area `slot` (16 KiB each).
    fn slot(&self, slot: usize) -> u64 {
        self.ram.pa(BUF + slot * 0x4000)
    }

    fn at(&self, slot: usize) -> usize {
        BUF + slot * 0x4000
    }

    /// Posts one buffer of `len` at `slot`.
    fn post1(&mut self, q: u32, slot: usize, len: u32, writable: bool) -> u16 {
        let pa = self.slot(slot);
        self.drv.post(q, &[(pa, len, writable)])
    }

    /// The length of the next used element of q, or None.
    fn reap(&mut self, q: u32) -> Option<u32> {
        self.drv.reap(q).map(|(_, len)| len)
    }

    /// Waits up to 2 s for a used element of q (the device's own threads fill it).
    fn reap_wait(&mut self, q: u32) -> Option<u32> {
        let t0 = Instant::now();
        loop {
            if let Some(n) = self.reap(q) {
                return Some(n);
            }
            if t0.elapsed() > Duration::from_secs(2) {
                return None;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    /// A message on the bridge's transmit queue (slot 0), handled within the kick.
    fn send_message(&mut self, channel: u16, kind: u16, payload: &[u8]) {
        let m = message(channel, kind, payload);
        self.ram.write(self.at(0), &m);
        self.post1(OB_QUEUE_TX, 0, m.len() as u32, false);
        assert_eq!(self.reap(OB_QUEUE_TX), Some(0));
    }

    fn header(&self, slot: usize) -> ObHeader {
        ObHeader::read_from(&self.ram.read(self.at(slot), 8))
    }

    fn body(&self, slot: usize, len: usize) -> Vec<u8> {
        self.ram.read(self.at(slot) + 8, len)
    }
}

// ---- a fake radio --------------------------------------------------------------------------------
#[derive(Default)]
struct Radio {
    opens: u32,
    closes: u32,
    sends: u32,
    last_type: u8,
    last_length: usize,
}

struct FakeRadio(Arc<Mutex<Radio>>);

impl HciBackend for FakeRadio {
    fn open(&self) -> Result<(), i32> {
        self.0.lock().unwrap().opens += 1;
        Ok(())
    }
    fn send(&self, kind: u8, packet: &[u8]) -> Result<(), i32> {
        let mut r = self.0.lock().unwrap();
        r.sends += 1;
        r.last_type = kind;
        r.last_length = packet.len();
        Ok(())
    }
    fn close(&self) {
        self.0.lock().unwrap().closes += 1;
    }
}

#[test]
fn bridge_device() {
    let _env = env_lock();
    std::env::remove_var("ONEIOS_HAPTIC_NATIVE_IMPACTS");
    crate::haptics::set_native_impacts(None);
    let tmp = TempDir::new("bridge-dev");
    let address = tmp.put("bt_addr", "A4:75:B9:0A:0B:0C\n");
    let (backend, motor) = FakeMotor::new(Motor::default());
    let m = || motor.lock().unwrap();
    let haptic = Arc::new(Haptic::open_backend(backend).unwrap());
    let radio = Arc::new(Mutex::new(Radio::default()));
    let made: Arc<Mutex<Option<Arc<Hci>>>> = Arc::default();
    let factory: HciFactory = {
        let (radio, made) = (Arc::clone(&radio), Arc::clone(&made));
        Box::new(move |rx, state| {
            let radio = Arc::clone(&radio);
            let hci = Hci::new(move |_| FakeRadio(radio), Some(rx), Some(state));
            *made.lock().unwrap() = Some(Arc::clone(&hci));
            Some(hci)
        })
    };
    let config = BridgeConfig {
        haptic: Some(Arc::clone(&haptic)),
        hci_factory: Some(factory),
        bt_address_path: Some(address),
        ..BridgeConfig::default()
    };
    let dev = bridge_dev::create(0, 40, config).unwrap();
    let mut g = Guest::up(dev.clone(), 2, 0);
    assert!(g.drv.read(0x008) == OB_VIRTIO_DEVICE_ID && g.drv.read(0x100) == OB_PROTO_VERSION);
    let r = || radio.lock().unwrap();
    let hci = || made.lock().unwrap().clone().unwrap();

    for i in 1..3 {
        g.post1(OB_QUEUE_RX, i, OB_MAX_MESSAGE, true);
    }
    assert_eq!(g.reap(OB_QUEUE_RX), None);

    g.send_message(OB_CH_CONTROL, OB_CTL_HELLO, &ObCtlHello { version: OB_PROTO_VERSION, channels: 0 }.to_bytes());
    assert_eq!(g.reap(OB_QUEUE_RX), Some(16));
    let (h, hello) = (g.header(1), ObCtlHello::read_from(&g.body(1, 8)));
    assert!(h.channel == OB_CH_CONTROL && h.kind == OB_CTL_HELLO && hello.channels == 7);

    let vib = ObHapticVib { on_ms: 250, intensity: 800, frequency_hz: 0 };
    g.send_message(OB_CH_HAPTIC, OB_HAPTIC_VIB, &vib.to_bytes());
    haptic.flush();
    assert!(m().plays == 1 && m().last_length == 250);
    g.send_message(OB_CH_HAPTIC, OB_HAPTIC_STOP, &[]);
    haptic.flush();
    assert_eq!(m().stops, 1);
    g.send_message(OB_CH_HAPTIC, OB_HAPTIC_IMPACT, &ObHapticImpact { kind: OB_IMPACT_HEAVY }.to_bytes());
    haptic.flush();
    assert!(m().impacts == 0 && m().plays == 1); // feature absent => fail closed
    let hs = haptic.stats(); // the guest's two messages (and the device start's stops), as counted
    assert!(hs.vib == 1 && hs.stop >= 1 && hs.played == 1 && hs.stopped == 1 && hs.failed == 0);

    // Bluetooth: open, the HAL becomes ready, a command goes out, its event comes back.
    let reset = [0x03, 0x0c, 0x00];
    let complete = [0x0e, 0x04, 0x01, 0x03, 0x0c, 0x00];
    g.send_message(OB_CH_HCI, OB_HCI_COMMAND, &reset); // before open: ignored
    assert_eq!(r().sends, 0);
    g.send_message(OB_CH_HCI, OB_HCI_OPEN, &[]);
    assert_eq!(r().opens, 1);
    hci().set_state(HciState::On);
    assert_eq!(g.reap(OB_QUEUE_RX), Some(20));
    let (h, state) = (g.header(2), ObHciState::read_from(&g.body(2, 12)));
    assert!(h.channel == OB_CH_HCI && h.kind == OB_HCI_STATE && state.state == OB_HCI_STATE_READY);
    assert_eq!(state.bd_addr, [0xa4, 0x75, 0xb9, 0x0a, 0x0b, 0x0c]);
    g.send_message(OB_CH_HCI, OB_HCI_COMMAND, &reset);
    assert!(r().sends == 1 && r().last_type == 1 && r().last_length == 3);
    // The guest has no buffer posted: the event waits, and arrives when one is.
    hci().deliver(4, &complete);
    assert_eq!(g.reap(OB_QUEUE_RX), None);
    g.post1(OB_QUEUE_RX, 3, OB_MAX_MESSAGE, true);
    assert_eq!(g.reap(OB_QUEUE_RX), Some(14));
    let h = g.header(3);
    assert!(h.kind == OB_HCI_EVENT && h.length == 6 && g.body(3, 6) == complete);

    // A second open of a controller that is ready is answered with its state again (a new iOS client waits for it).
    g.post1(OB_QUEUE_RX, 4, OB_MAX_MESSAGE, true);
    g.send_message(OB_CH_HCI, OB_HCI_OPEN, &[]);
    assert!(r().opens == 1 && g.reap(OB_QUEUE_RX) == Some(20));
    let (h, state) = (g.header(4), ObHciState::read_from(&g.body(4, 12)));
    assert!(h.kind == OB_HCI_STATE && state.state == OB_HCI_STATE_READY);

    // A controller that failed after it was opened (the HAL died, no initializationComplete) is closed and opened again.
    let closes = r().closes;
    hci().set_state(HciState::Failed);
    g.send_message(OB_CH_HCI, OB_HCI_OPEN, &[]);
    assert!(r().opens == 2 && r().closes == closes + 1);
    hci().set_state(HciState::On);

    // Malformed messages from the guest change nothing.
    g.ram.write(g.at(0), &[1, 2, 3, 4]);
    g.post1(OB_QUEUE_TX, 0, 4, false);
    assert_eq!(g.reap(OB_QUEUE_TX), Some(0));
    g.ram.write(g.at(0), &ObHeader { channel: OB_CH_HCI, kind: OB_HCI_COMMAND, length: 4000 }.to_bytes());
    g.post1(OB_QUEUE_TX, 0, 8, false);
    assert!(g.reap(OB_QUEUE_TX) == Some(0) && r().sends == 1);
    g.send_message(99, 1, &[]);

    // The return's graceful-shutdown ask: a host->guest control message with no payload, queued like any other. Drain
    // anything the HCI section left (leaving one buffer posted), then the ask is the next message delivered.
    loop {
        g.post1(OB_QUEUE_RX, 1, OB_MAX_MESSAGE, true);
        if g.reap(OB_QUEUE_RX).is_none() {
            break;
        }
    }
    assert!(request_shutdown());
    assert_eq!(g.reap(OB_QUEUE_RX), Some(8));
    let h = g.header(1);
    assert!(h.channel == OB_CH_CONTROL && h.kind == OB_CTL_SHUTDOWN && h.length == 0);

    // A reset stops the motor and releases the controller.
    g.send_message(OB_CH_HAPTIC, OB_HAPTIC_VIB, &ObHapticVib { on_ms: 5000, ..vib }.to_bytes());
    haptic.flush();
    g.drv.write(0x70, 0);
    haptic.flush();
    assert!(m().stops == 2 && r().closes == closes + 2); // the reopen above closed it once
}

/// Plays the guest's Actuator periods (240 mono frames, 5 ms at 48 kHz) of `sample` through the same classifier as
/// the kernel's oneios_haptic_actuator_play() and sends what it asks over the real queues.
fn send_pcm_periods(g: &mut Guest, haptic: &Haptic, actuator: &mut OaState, periods: u32, sample: i16) {
    let samples = [sample; 240];
    for _ in 0..periods {
        let request = oa_period(actuator, &samples, 240, 1);
        match request.action {
            OA_IMPACT => {
                let kind = match request.intensity {
                    0..=250 => OB_IMPACT_LIGHT,
                    251..=800 => OB_IMPACT_MEDIUM,
                    _ => OB_IMPACT_HEAVY,
                };
                g.send_message(OB_CH_HAPTIC, OB_HAPTIC_IMPACT, &ObHapticImpact { kind }.to_bytes());
            }
            OA_VIB => {
                let vib = ObHapticVib {
                    on_ms: request.ms,
                    intensity: request.intensity as u16,
                    frequency_hz: request.hz as u16,
                };
                g.send_message(OB_CH_HAPTIC, OB_HAPTIC_VIB, &vib.to_bytes());
            }
            OA_STOP => g.send_message(OB_CH_HAPTIC, OB_HAPTIC_STOP, &[]),
            _ => {}
        }
        if request.action != OA_NONE {
            haptic.flush();
        }
    }
}

/// A native-impacts-enabled host replies with an extended HELLO without changing OB_PROTO_VERSION, and accepts only
/// correctly typed impact payloads.
#[test]
fn bridge_native_impacts() {
    let _env = env_lock();
    std::env::set_var("ONEIOS_HAPTIC_NATIVE_IMPACTS", "1");
    crate::haptics::set_native_impacts(None);
    let (backend, motor) = FakeMotor::new(Motor { impact_ms: 400, ..Motor::default() });
    let m = || motor.lock().unwrap();
    let haptic = Arc::new(Haptic::open_backend(backend).unwrap());
    assert!(haptic.impacts_available());
    let dev = bridge_dev::create(0, 41, BridgeConfig { haptic: Some(Arc::clone(&haptic)), ..BridgeConfig::default() });
    let mut g = Guest::up(dev.unwrap(), 2, 0);
    g.post1(OB_QUEUE_RX, 1, OB_MAX_MESSAGE, true);
    g.send_message(OB_CH_CONTROL, OB_CTL_HELLO, &ObCtlHello { version: OB_PROTO_VERSION, channels: 0 }.to_bytes());
    assert_eq!(g.reap(OB_QUEUE_RX), Some((ObHeader::SIZE + ObCtlHelloExt::SIZE) as u32));
    let reply = ObCtlHelloExt::read_from(&g.body(1, 12));
    assert!(reply.version == OB_PROTO_VERSION && reply.channels & 1 << OB_CH_HAPTIC != 0);
    assert!(reply.features & OB_FEAT_HAPTIC_IMPACT != 0);

    let impact = ObHapticImpact { kind: OB_IMPACT_HEAVY };
    g.send_message(OB_CH_HAPTIC, OB_HAPTIC_IMPACT, &impact.to_bytes());
    haptic.flush();
    assert!(m().impacts == 1 && m().plays == 0 && m().last_kind == Some(Impact::Heavy));
    let bad = ObHapticImpact { kind: 99 }.to_bytes();
    g.send_message(OB_CH_HAPTIC, OB_HAPTIC_IMPACT, &bad);
    g.send_message(OB_CH_HAPTIC, OB_HAPTIC_IMPACT, &bad[..2]);
    haptic.flush();
    assert_eq!(m().impacts, 1); // invalid kind and truncated payload ignored
    g.send_message(OB_CH_HAPTIC, OB_HAPTIC_STOP, &[]);
    haptic.flush();
    assert_eq!(m().stops, 1); // guest STOP cancels the active impact

    // Model the guest's boot arg oneios_native_impacts=1 plus negotiated OB_FEAT_HAPTIC_IMPACT. Only an isolated PCM
    // burst becomes a preset.
    let mut actuator = OaState { native_taps: reply.features & OB_FEAT_HAPTIC_IMPACT != 0, ..OaState::default() };
    assert!(actuator.native_taps);
    send_pcm_periods(&mut g, &haptic, &mut actuator, 1, 32000); // 5 ms strong pulse
    send_pcm_periods(&mut g, &haptic, &mut actuator, 2, 0); // 10 ms silence: classify
    let st = haptic.stats();
    assert!(m().impacts == 2 && m().last_kind == Some(Impact::Heavy) && m().uploads == 0 && m().plays == 0);
    assert!(st.impacts == 2 && st.vib == 0 && st.failed == 0);

    // Also validate the low/medium branches of the guest's threshold mapping.
    send_pcm_periods(&mut g, &haptic, &mut actuator, 1, 3200);
    send_pcm_periods(&mut g, &haptic, &mut actuator, 2, 0);
    assert!(m().impacts == 3 && m().last_kind == Some(Impact::Light));
    send_pcm_periods(&mut g, &haptic, &mut actuator, 1, 16000);
    send_pcm_periods(&mut g, &haptic, &mut actuator, 2, 0);
    assert!(m().impacts == 4 && m().last_kind == Some(Impact::Medium) && m().uploads == 0 && m().plays == 0);

    // If the guest did not opt in, the same short PCM must remain a legacy duration-based VIB, despite native support
    // being advertised by the host.
    let mut legacy = OaState::default();
    g.send_message(OB_CH_HAPTIC, OB_HAPTIC_STOP, &[]);
    haptic.flush();
    send_pcm_periods(&mut g, &haptic, &mut legacy, 1, 32000);
    send_pcm_periods(&mut g, &haptic, &mut legacy, 2, 0);
    let st = haptic.stats();
    assert!(m().impacts == 4 && m().plays == 1 && st.vib == 1);

    // An opted-in sustained waveform (>=30 ms) must also use the ordinary VIB/STOP route, not get misclassified as an
    // impact.
    g.send_message(OB_CH_HAPTIC, OB_HAPTIC_STOP, &[]);
    haptic.flush();
    let mut sustained = OaState { native_taps: true, ..OaState::default() };
    send_pcm_periods(&mut g, &haptic, &mut sustained, 12, 20000); // 60 ms continuous
    send_pcm_periods(&mut g, &haptic, &mut sustained, 2, 0);
    let st = haptic.stats();
    assert!(m().impacts == 4 && m().plays >= 2 && st.vib >= 2 && st.stop >= 3 && st.failed == 0);

    // Allow a full-second preset so a loaded CI host cannot accidentally let the fake effect expire between dispatch
    // and reset.
    m().impact_ms = 1000;
    // A guest reset cancels a still-active native impact, with no evdev fallback.
    send_pcm_periods(&mut g, &haptic, &mut actuator, 1, 32000);
    send_pcm_periods(&mut g, &haptic, &mut actuator, 2, 0);
    let stops_before_reset = m().stops;
    g.drv.write(0x70, 0);
    haptic.flush();
    assert!(m().impacts == 5 && m().stops == stops_before_reset + 1);
    std::env::remove_var("ONEIOS_HAPTIC_NATIVE_IMPACTS");
}

/// The battery channel: the hello's bit, the first state on the query, a change pushed by the worker, no more after a
/// reset.
#[test]
fn bridge_battery() {
    let _settings = crate::hw::settings_lock();
    let tmp = TempDir::new("bridge-battery");
    let node = |rel: &str, text: &str| {
        tmp.put(&format!("power_supply/{rel}"), text);
    };
    node("battery/capacity", "42\n");
    node("battery/status", "Discharging\n");
    node("battery/present", "1\n");
    node("battery/temp", "311\n");
    node("battery/voltage_now", "3900000\n");
    node("battery/current_now", "-500000\n");
    node("battery/charge_full", "5000000\n");
    node("usb/online", "0\n");
    let options = crate::battery::Options {
        dir: Some(tmp.at("power_supply")),
        poll_ms: 10,
        ..crate::battery::Options::default()
    };
    let battery = Arc::new(crate::battery::Battery::open(&options).unwrap());
    let dev =
        bridge_dev::create(0, 42, BridgeConfig { battery: Some(Arc::clone(&battery)), ..BridgeConfig::default() });
    let mut g = Guest::up(dev.unwrap(), 2, 0);
    for i in 1..5 {
        g.post1(OB_QUEUE_RX, i, OB_MAX_MESSAGE, true);
    }
    g.send_message(OB_CH_CONTROL, OB_CTL_HELLO, &ObCtlHello { version: OB_PROTO_VERSION, channels: 0 }.to_bytes());
    assert_eq!(g.reap(OB_QUEUE_RX), Some(16));
    assert!(ObCtlHello::read_from(&g.body(1, 8)).channels & 1 << OB_CH_BATTERY != 0);

    let size = (8 + ObBatteryState::SIZE) as u32;
    g.send_message(OB_CH_BATTERY, OB_BATTERY_QUERY, &[]);
    assert_eq!(g.reap(OB_QUEUE_RX), Some(size));
    let (h, s) = (g.header(2), ObBatteryState::read_from(&g.body(2, ObBatteryState::SIZE)));
    assert!(h.channel == OB_CH_BATTERY && h.kind == OB_BATTERY_STATE && h.length as usize == ObBatteryState::SIZE);
    assert!(s.flags == OB_BATTERY_PRESENT && s.percent == 42 && s.temp_deci_c == 311 && s.voltage_mv == 3900);
    assert!(s.current_ma == -500 && s.full_mah == 5000 && s.design_mah == 5000 && s.minutes == 252);

    node("battery/status", "Charging\n");
    node("usb/online", "1\n");
    assert_eq!(g.reap_wait(OB_QUEUE_RX), Some(size));
    let s = ObBatteryState::read_from(&g.body(3, ObBatteryState::SIZE));
    let both = OB_BATTERY_PLUGGED | OB_BATTERY_CHARGING;
    assert!(s.flags & both == both && s.current_ma == 500);

    // The base S25 on a wireless pad: the cps4038's `wireless` supply is online, USB is not, and the pad does not keep
    // up with the phone. Plugged stays true, charging turns false, and the current is negative again.
    node("wireless/online", "1\n");
    node("usb/online", "0\n");
    node("battery/status", "Discharging\n");
    assert_eq!(g.reap_wait(OB_QUEUE_RX), Some(size));
    let s = ObBatteryState::read_from(&g.body(4, ObBatteryState::SIZE));
    assert!(s.flags & OB_BATTERY_PLUGGED != 0 && s.flags & OB_BATTERY_CHARGING == 0 && s.current_ma == -500);

    // A reset stops the updates: the guest is gone, nothing queues for it.
    g.drv.write(0x70, 0);
    node("battery/capacity", "43\n");
    std::thread::sleep(Duration::from_millis(60)); // six of the worker's polls
    assert_eq!(g.reap(OB_QUEUE_RX), None);
}

/// The brightness channel: the hello's bit, the state answer, a set that reaches the panel's node.
#[test]
fn bridge_brightness() {
    let _settings = crate::hw::settings_lock();
    let tmp = TempDir::new("bridge-bright");
    tmp.put("backlight/panel/max_brightness", "510\n");
    let node = tmp.put("backlight/panel/brightness", "100\n");
    let options = crate::brightness::Options {
        backlight_dir: Some(tmp.at("backlight").into()),
        touch_dir: Some(tmp.at("no-touch").into()),
        no_als: true,
        max_nits: 500,
        ..crate::brightness::Options::default()
    };
    let bright = Arc::new(crate::brightness::Bright::open(&options).unwrap());
    let dev =
        bridge_dev::create(0, 41, BridgeConfig { brightness: Some(Arc::clone(&bright)), ..BridgeConfig::default() });
    let mut g = Guest::up(dev.unwrap(), 2, 0);
    for i in 1..3 {
        g.post1(OB_QUEUE_RX, i, OB_MAX_MESSAGE, true);
    }
    g.send_message(OB_CH_CONTROL, OB_CTL_HELLO, &ObCtlHello { version: OB_PROTO_VERSION, channels: 0 }.to_bytes());
    assert_eq!(g.reap(OB_QUEUE_RX), Some(16));
    assert_eq!(ObCtlHello::read_from(&g.body(1, 8)).channels, 1 << OB_CH_CONTROL | 1 << OB_CH_BRIGHTNESS);
    g.send_message(OB_CH_BRIGHTNESS, OB_BRIGHT_QUERY, &[]);
    assert_eq!(g.reap(OB_QUEUE_RX), Some((8 + ObBrightState::SIZE) as u32));
    let (h, s) = (g.header(2), ObBrightState::read_from(&g.body(2, ObBrightState::SIZE)));
    assert!(h.channel == OB_CH_BRIGHTNESS && h.kind == OB_BRIGHT_STATE && s.flags == OB_BRIGHT_HAS_PANEL);
    assert!(s.max_nits == 500 && s.min_millinits == 500000 / 255 && s.level > 40000 && s.level < 42000); // 100 of 255
    let until = |want: u32| {
        let t0 = Instant::now();
        while bright.current() != want && t0.elapsed() < Duration::from_secs(2) {
            std::thread::sleep(Duration::from_millis(5));
        }
        bright.current()
    };
    g.send_message(OB_CH_BRIGHTNESS, OB_BRIGHT_SET, &ObBrightSet { level: 65535, fade_ms: 0, flags: 0 }.to_bytes());
    g.send_message(OB_CH_BRIGHTNESS, OB_BRIGHT_SET, &[0; 4]); // too short: ignored
    assert_eq!(until(255), 255);
    // iOS's display off and on (the framebuffer's power request): the floor, then the level from before
    let mut power = ObBrightPower { on: 0, source: OB_POWER_FRAMEBUFFER };
    g.send_message(OB_CH_BRIGHTNESS, OB_BRIGHT_POWER, &power.to_bytes());
    assert_eq!(until(1), 1);
    power.on = 1;
    g.send_message(OB_CH_BRIGHTNESS, OB_BRIGHT_POWER, &power.to_bytes()[..4]); // too short: ignored
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(bright.current(), 1);
    g.send_message(OB_CH_BRIGHTNESS, OB_BRIGHT_POWER, &power.to_bytes());
    assert_eq!(until(255), 255);
    drop(g);
    Arc::try_unwrap(bright).ok().expect("the test holds the last reference once the device is gone").close();
    assert_eq!(std::fs::read_to_string(node).unwrap().trim(), "100");
}

#[test]
fn net_device() {
    let c = NetConfig { guest_mac: [0x02, 0xd7, 0x01, 0x02, 0x03, 0x04], ..NetConfig::default() };
    let wire: Arc<Mutex<Option<(OwnedFd, RxFn)>>> = Arc::default();
    let open: crate::net::NetOpen = {
        let wire = Arc::clone(&wire);
        Box::new(move |c, rx| {
            let mut fds = [0; 2];
            // SAFETY: a valid array of two descriptors.
            assert_eq!(unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_DGRAM, 0, fds.as_mut_ptr()) }, 0);
            // SAFETY: two new descriptors nothing else owns.
            let (mine, theirs) = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
            *wire.lock().unwrap() = Some((theirs, Arc::clone(&rx)));
            Net::open_fd(c, mine, rx).ok().map(|n| Box::new(n) as Box<dyn NetBackend>)
        })
    };
    let dev = virtio_net::create(0, 41, &c, Some(open)).unwrap();
    let (wire_fd, guest_rx) = wire.lock().unwrap().take().unwrap();
    let wire_owned = wire_fd;
    let wire_fd = wire_owned.as_raw_fd();
    let mut g = Guest::up(dev.clone(), 2, 1 << 5);
    assert!(g.drv.read(0x008) == 1 && g.drv.read(0x100) & 0xffffff == 0x01d702); // the MAC in the configuration space
    g.post1(0, 1, 2048, true);
    g.post1(0, 2, 2048, true);
    // Guest to wire: the 12-byte header is taken off.
    let tx: Vec<u8> = [0u8; 12].into_iter().chain(0..64).collect();
    g.ram.write(g.at(3), &tx);
    g.post1(1, 3, tx.len() as u32, false);
    assert_eq!(g.reap(1), Some(0));
    let mut seen = [0u8; 200];
    // SAFETY: a valid buffer of its length.
    let n = unsafe { libc::recv(wire_fd, seen.as_mut_ptr().cast(), seen.len(), 0) };
    assert!(n == 64 && seen[13] == 13);
    // Wire to guest: a header with num_buffers = 1, then the frame.
    // SAFETY: a valid buffer of 60 bytes.
    assert_eq!(unsafe { libc::send(wire_fd, seen.as_ptr().cast(), 60, 0) }, 60);
    assert_eq!(g.reap_wait(0), Some(72));
    let got = g.ram.read(g.at(1), 72);
    assert!(got[10] == 1 && got[11] == 0 && got[12 + 13] == 13);
    // DHCP from the guest is answered into its receive queue.
    let dhcp: Vec<u8> = [0u8; 12].into_iter().chain(crate::net::test_net::make_dhcp(1, 5)).collect();
    g.ram.write(g.at(3), &dhcp);
    g.post1(1, 3, dhcp.len() as u32, false);
    assert_eq!(g.reap(1), Some(0));
    assert!(g.reap(0).is_some_and(|n| n >= 12 + 342) && g.ram.read(g.at(2), 12 + 43)[12 + 42] == 2);
    // No buffer posted: the frame is dropped, nothing breaks.
    // SAFETY: a valid buffer of 60 bytes.
    assert_eq!(unsafe { libc::send(wire_fd, seen.as_ptr().cast(), 60, 0) }, 60);
    std::thread::sleep(Duration::from_millis(20));
    assert_eq!(g.reap(0), None);
    // A frame of the Wi-Fi station's own type (0x88b5) is not dropped at once: it waits for a buffer. One posted 10 ms
    // later takes it; with none, it is dropped after the wait (50 ms).
    let mut control = [0u8; 60];
    control[12] = 0x88;
    control[13] = 0xb5;
    control[14] = 0x77;
    let slot1 = g.slot(1);
    std::thread::scope(|s| {
        let rx = Arc::clone(&guest_rx);
        let handle = s.spawn(move || rx(&control));
        std::thread::sleep(Duration::from_millis(10));
        g.drv.post(0, &[(slot1, 2048, true)]);
        handle.join().unwrap();
    });
    assert_eq!(g.reap(0), Some(72));
    let got = g.ram.read(g.at(1), 12 + 15);
    assert!(got[12 + 12] == 0x88 && got[12 + 14] == 0x77);
    let t0 = Instant::now();
    guest_rx(&control);
    assert!(g.reap(0).is_none() && t0.elapsed() >= Duration::from_millis(40));
    drop(g);
    drop(dev);
}

#[derive(Default)]
struct Sound {
    starts: [AtomicU32; 2],
    stops: [AtomicU32; 2],
    frames: AtomicU32,
    first: Mutex<Option<i16>>,
}

impl AudioBackend for Sound {
    fn start(&self, output: bool, rate: u32, channels: u32) -> Result<(), i32> {
        self.starts[usize::from(output)].fetch_add(1, Ordering::Relaxed);
        if rate == 48000 && channels == if output { 2 } else { 1 } {
            Ok(())
        } else {
            Err(libc::EINVAL)
        }
    }
    fn stop(&self, output: bool) -> Result<(), i32> {
        self.stops[usize::from(output)].fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
    fn write(&self, pcm: &[i16]) -> usize {
        self.first.lock().unwrap().get_or_insert(pcm[0]);
        self.frames.fetch_add(pcm.len() as u32 / 2, Ordering::Relaxed);
        pcm.len() / 2
    }
    fn read(&self, pcm: &mut [i16]) -> i32 {
        std::thread::sleep(Duration::from_millis(2));
        for (i, s) in pcm.iter_mut().enumerate() {
            *s = 100 + i as i16;
        }
        pcm.len() as i32
    }
}

fn u32s(words: &[u32]) -> Vec<u8> {
    words.iter().flat_map(|w| w.to_le_bytes()).collect()
}

#[test]
fn snd_device() {
    let sound = Arc::new(Sound::default());
    let dev = virtio_snd::create(0, 42, Arc::clone(&sound) as Arc<dyn AudioBackend>).unwrap();
    let mut g = Guest::up(dev.clone(), 4, 0);
    assert!(g.drv.read(0x008) == 25 && g.drv.read(0x104) == 2);
    // A control request (slot 0) and its reply (slot 1): the status.
    let control = |g: &mut Guest, request: &[u8], reply_len: u32| -> (u32, Vec<u8>) {
        g.ram.write(g.at(0), request);
        g.ram.fill(g.at(1), 0, reply_len as usize);
        let (a, b) = (g.slot(0), g.slot(1));
        g.drv.post(0, &[(a, request.len() as u32, false), (b, reply_len, true)]);
        assert!(g.reap(0).is_some_and(|n| n >= 4));
        let reply = g.ram.read(g.at(1), reply_len as usize);
        (u32::from_le_bytes(reply[..4].try_into().unwrap()), reply)
    };
    let (status, reply) = control(&mut g, &u32s(&[0x0100, 0, 2, 32]), 4 + 2 * 32);
    assert_eq!(status, 0x8000);
    assert!(reply[4 + 24] == 0 && reply[4 + 25] == 2 && reply[4 + 32 + 24] == 1 && reply[4 + 32 + 26] == 1); // stereo out, mono in
    assert_eq!(u64::from_le_bytes(reply[12..20].try_into().unwrap()), 1 << 5);
    let params = |stream: u32, channels: u8, rate: u8| {
        let mut p = u32s(&[0x0101, stream, 15360, 960, 0]);
        p.extend([channels, 5, rate, 0]);
        p
    };
    assert_eq!(control(&mut g, &params(0, 2, 7), 4).0, 0x8000);
    assert_eq!(control(&mut g, &params(0, 2, 6), 4).0, 0x8002); // 44.1 kHz: not offered
    assert_eq!(control(&mut g, &params(1, 1, 7), 4).0, 0x8000);
    assert_eq!(control(&mut g, &u32s(&[0x0102, 0]), 4).0, 0x8000);
    assert!(control(&mut g, &u32s(&[0x0104, 0]), 4).0 == 0x8000 && sound.starts[1].load(Ordering::Relaxed) == 1);
    assert_eq!(control(&mut g, &u32s(&[0x0104, 7]), 4).0, 0x8001);

    // One output period: stream id (slot 2), the PCM (slot 3), the status (slot 4).
    let period: Vec<i16> = (1..=480).collect();
    g.ram.write(g.at(2), &0u32.to_le_bytes());
    g.ram.write(g.at(3), &crate::audio::proto::au_message(Default::default(), &period)[8..]);
    let parts = [(g.slot(2), 4, false), (g.slot(3), 960, false), (g.slot(4), 8, true)];
    g.drv.post(2, &parts);
    assert_eq!(g.reap(2), Some(8));
    assert!(g.ram.u32(g.at(4)) == 0x8000 && sound.frames.load(Ordering::Relaxed) == 240);
    assert_eq!(*sound.first.lock().unwrap(), Some(1));
    // Periods in flight: the device takes each period inside the guest's notify (a vCPU's MMIO write), so it is used
    // and with the backend before the notify returns: none waits in the queue. The hop's time is printed.
    let (mut sum, mut max) = (0f64, 0f64);
    for _ in 0..64 {
        let before = sound.frames.load(Ordering::Relaxed);
        let t = Instant::now();
        g.drv.post(2, &parts);
        let us = t.elapsed().as_secs_f64() * 1e6;
        assert!(sound.frames.load(Ordering::Relaxed) == before + 240 && g.reap(2) == Some(8)); // 0 in flight
        sum += us;
        max = max.max(us);
    }
    println!(
        "virtio-snd: 64 periods, each with the back end before its notify returned (0 in flight): {:.1} us on average, \
         {max:.1} us at most",
        sum / 64.0
    );

    // Capture: posted buffers come back holding a period each (stream id, data, status: slots 5.., two chains).
    for i in 0..2 {
        let base = 5 + 3 * i;
        g.ram.write(g.at(base), &1u32.to_le_bytes());
        let chain = [(g.slot(base), 4, false), (g.slot(base + 1), 480, true), (g.slot(base + 2), 8, true)];
        g.drv.post(3, &chain);
    }
    assert!(control(&mut g, &u32s(&[0x0104, 1]), 4).0 == 0x8000 && sound.starts[0].load(Ordering::Relaxed) == 1);
    assert_eq!(g.reap_wait(3), Some(488));
    let data = g.ram.read(g.at(6), 480);
    let s = |i: usize| i16::from_le_bytes([data[2 * i], data[2 * i + 1]]);
    assert!(s(0) == 100 && s(239) == 339 && g.ram.u32(g.at(7)) == 0x8000);
    assert!(control(&mut g, &u32s(&[0x0105, 1]), 4).0 == 0x8000 && sound.stops[0].load(Ordering::Relaxed) == 1);
    assert!(control(&mut g, &u32s(&[0x0105, 0]), 4).0 == 0x8000 && sound.stops[1].load(Ordering::Relaxed) == 1);
    // A period for a stopped stream is acknowledged and not played.
    g.drv.post(2, &parts);
    assert!(g.reap(2) == Some(8) && sound.frames.load(Ordering::Relaxed) == 65 * 240);

    let null = NullAudio;
    assert!(null.start(true, 48000, 2).is_ok() && null.write(&period) == 240);
    // The latency probe is the helper's only (xnuhost then leaves the kernel's default); telling the null backend is a
    // no-op.
    assert!(null.probe_latency(10) == Err(libc::ENOTSUP) && sound.probe_latency(10) == Err(libc::ENOTSUP));
    null.told(480);
}
