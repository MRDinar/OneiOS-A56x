// SPDX-License-Identifier: MIT
//! The mirrors against their C headers: every constant of the header (by its prefix) and every struct's size and
//! fields' offsets.

use super::actuator::*;
use super::bridge::*;
use super::cheader::{check_struct, check_values, repo_file, Header};
use super::wire::Wire;

#[test]
fn bridge_proto_matches_the_c_header() {
    let h = Header::read(&repo_file("drivers/bridge/oneios_bridge_proto.h"));
    #[rustfmt::skip]
    let values: &[(&str, i64)] = &[
        ("OB_VIRTIO_DEVICE_ID", OB_VIRTIO_DEVICE_ID.into()), ("OB_PROTO_VERSION", OB_PROTO_VERSION.into()),
        ("OB_MAX_MESSAGE", OB_MAX_MESSAGE.into()), ("OB_MAX_PAYLOAD", OB_MAX_PAYLOAD.into()),
        ("OB_QUEUE_TX", OB_QUEUE_TX.into()), ("OB_QUEUE_RX", OB_QUEUE_RX.into()),
        ("OB_CH_CONTROL", OB_CH_CONTROL.into()), ("OB_CH_HAPTIC", OB_CH_HAPTIC.into()), ("OB_CH_HCI", OB_CH_HCI.into()),
        ("OB_CH_CAMERA", OB_CH_CAMERA.into()), ("OB_CH_BRIGHTNESS", OB_CH_BRIGHTNESS.into()),
        ("OB_CH_BATTERY", OB_CH_BATTERY.into()), ("OB_CH_THERMAL", OB_CH_THERMAL.into()), ("OB_CH_COUNT", OB_CH_COUNT.into()),
        ("OB_CTL_HELLO", OB_CTL_HELLO.into()), ("OB_CTL_SHUTDOWN", OB_CTL_SHUTDOWN.into()),
        ("OB_FEAT_HAPTIC_IMPACT", OB_FEAT_HAPTIC_IMPACT.into()),
        ("OB_HAPTIC_VIB", OB_HAPTIC_VIB.into()), ("OB_HAPTIC_STOP", OB_HAPTIC_STOP.into()),
        ("OB_HAPTIC_IMPACT", OB_HAPTIC_IMPACT.into()), ("OB_IMPACT_LIGHT", OB_IMPACT_LIGHT.into()),
        ("OB_IMPACT_MEDIUM", OB_IMPACT_MEDIUM.into()), ("OB_IMPACT_HEAVY", OB_IMPACT_HEAVY.into()),
        ("OB_HCI_COMMAND", OB_HCI_COMMAND.into()), ("OB_HCI_ACL", OB_HCI_ACL.into()), ("OB_HCI_SCO", OB_HCI_SCO.into()),
        ("OB_HCI_EVENT", OB_HCI_EVENT.into()), ("OB_HCI_ISO", OB_HCI_ISO.into()), ("OB_HCI_OPEN", OB_HCI_OPEN.into()),
        ("OB_HCI_CLOSE", OB_HCI_CLOSE.into()), ("OB_HCI_STATE", OB_HCI_STATE.into()),
        ("OB_HCI_STATE_OFF", OB_HCI_STATE_OFF.into()), ("OB_HCI_STATE_READY", OB_HCI_STATE_READY.into()),
        ("OB_HCI_STATE_FAILED", OB_HCI_STATE_FAILED.into()),
        ("OB_BRIGHT_SET", OB_BRIGHT_SET.into()), ("OB_BRIGHT_QUERY", OB_BRIGHT_QUERY.into()),
        ("OB_BRIGHT_STATE", OB_BRIGHT_STATE.into()), ("OB_BRIGHT_ALS", OB_BRIGHT_ALS.into()),
        ("OB_BRIGHT_POWER", OB_BRIGHT_POWER.into()), ("OB_BRIGHT_LINEAR", OB_BRIGHT_LINEAR.into()),
        ("OB_BRIGHT_HAS_PANEL", OB_BRIGHT_HAS_PANEL.into()), ("OB_BRIGHT_HAS_ALS", OB_BRIGHT_HAS_ALS.into()),
        ("OB_POWER_FRAMEBUFFER", OB_POWER_FRAMEBUFFER.into()),
        ("OB_BATTERY_QUERY", OB_BATTERY_QUERY.into()), ("OB_BATTERY_STATE", OB_BATTERY_STATE.into()),
        ("OB_BATTERY_PRESENT", OB_BATTERY_PRESENT.into()), ("OB_BATTERY_PLUGGED", OB_BATTERY_PLUGGED.into()),
        ("OB_BATTERY_CHARGING", OB_BATTERY_CHARGING.into()), ("OB_BATTERY_FULL", OB_BATTERY_FULL.into()),
        ("OB_BATTERY_UNKNOWN", OB_BATTERY_UNKNOWN.into()),
        ("OB_THERMAL_QUERY", OB_THERMAL_QUERY.into()), ("OB_THERMAL_STATE", OB_THERMAL_STATE.into()),
        ("OB_THERMAL_UNKNOWN", OB_THERMAL_UNKNOWN.into()), ("OB_THERMAL_BY_CASE", OB_THERMAL_BY_CASE.into()),
        ("OB_THERMAL_BY_SOC", OB_THERMAL_BY_SOC.into()), ("OB_THERMAL_BY_BATTERY", OB_THERMAL_BY_BATTERY.into()),
    ];
    check_values(&h, &["OB_"], values);
    check_struct::<ObHeader>(&h, "ob_header", &["channel", "type", "length"]);
    check_struct::<ObCtlHello>(&h, "ob_ctl_hello", &["version", "channels"]);
    check_struct::<ObCtlHelloExt>(&h, "ob_ctl_hello_ext", &["version", "channels", "features"]);
    check_struct::<ObHapticImpact>(&h, "ob_haptic_impact", &["kind"]);
    check_struct::<ObHapticVib>(&h, "ob_haptic_vib", &["on_ms", "intensity", "frequency_hz"]);
    check_struct::<ObHciState>(&h, "ob_hci_state", &["state", "bd_addr", "reserved"]);
    check_struct::<ObBrightSet>(&h, "ob_bright_set", &["level", "fade_ms", "flags"]);
    check_struct::<ObBrightState>(&h, "ob_bright_state", &["flags", "max_nits", "min_millinits", "level"]);
    check_struct::<ObBrightPower>(&h, "ob_bright_power", &["on", "source"]);
    check_struct::<ObBrightAls>(&h, "ob_bright_als", &["millilux", "clear", "ir", "uv"]);
    #[rustfmt::skip]
    check_struct::<ObBatteryState>(&h, "ob_battery_state", &["flags", "percent", "temp_deci_c", "voltage_mv", "current_ma",
                                                              "full_mah", "design_mah", "cycles", "minutes"]);
    #[rustfmt::skip]
    check_struct::<ObThermalState>(&h, "ob_thermal_state", &["level", "reason", "host_step", "case_temp", "soc", "cpu",
                                                              "gpu", "ddr", "xo", "board", "battery"]);
    let m =
        message(OB_CH_HAPTIC, OB_HAPTIC_VIB, &ObHapticVib { on_ms: 30, intensity: 500, frequency_hz: 0 }.to_bytes());
    assert_eq!(m, [1, 0, 1, 0, 8, 0, 0, 0, 30, 0, 0, 0, 0xf4, 1, 0, 0]);
    assert_eq!(ObHeader::parse(&m), Some(ObHeader { channel: 1, kind: 1, length: 8 }));
    assert_eq!(ObHapticVib::parse_short(&[5, 0]), ObHapticVib { on_ms: 5, ..Default::default() });
}

#[test]
fn actuator_matches_the_c_header() {
    let h = Header::read(&repo_file("drivers/haptics/oneios_actuator.h"));
    #[rustfmt::skip]
    check_values(&h, &["OA_"], &[
        ("OA_RATE", OA_RATE.into()), ("OA_SILENT", OA_SILENT.into()), ("OA_TAP_MS", OA_TAP_MS.into()),
        ("OA_SEGMENT_MS", OA_SEGMENT_MS.into()), ("OA_RENEW_MS", OA_RENEW_MS.into()), ("OA_LONG_MS", OA_LONG_MS.into()),
        ("OA_QUIET_MS", OA_QUIET_MS.into()), ("OA_LEVEL_STEP", OA_LEVEL_STEP.into()),
        ("OA_NONE", OA_NONE.into()), ("OA_VIB", OA_VIB.into()), ("OA_STOP", OA_STOP.into()), ("OA_IMPACT", OA_IMPACT.into()),
    ]);
    // The state's fields, in the header's order (the Rust struct is the caller's state, not a wire layout).
    let names: Vec<&str> = h.layout("oa_state").fields.iter().map(|f| f.name.as_str()).collect();
    #[rustfmt::skip]
    assert_eq!(names, ["on", "native_taps", "short_peak", "now_us", "until_us", "level", "hz", "run_us", "run_crossings",
                       "quiet_us", "sign"]);
    let names: Vec<&str> = h.layout("oa_request").fields.iter().map(|f| f.name.as_str()).collect();
    assert_eq!(names, ["action", "ms", "intensity", "hz"]);

    // A tap: 5 ms of a loud 200 Hz sine asks for one 20 ms vibration; silence lets it run out; a long buzz is renewed
    // in 50 ms segments and stopped after 10 ms of silence.
    let mut s = OaState::default();
    let loud: Vec<i16> = (0..240).map(|i| if (i / 120) % 2 == 0 { 16000 } else { -16000 }).collect();
    let quiet = vec![0i16; 240];
    let r = oa_period(&mut s, &loud, 240, 1);
    assert!(r.action == OA_VIB && r.ms == OA_TAP_MS && r.intensity == 488 && r.hz == 0);
    assert_eq!(oa_period(&mut s, &quiet, 240, 1).action, OA_NONE);
    let mut actions = Vec::new();
    for _ in 0..40 {
        actions.push(oa_period(&mut s, &loud, 240, 1).action);
    }
    assert!(actions.iter().filter(|&&a| a == OA_VIB).count() >= 3);
    assert_eq!((0..2).map(|_| oa_period(&mut s, &quiet, 240, 1).action).collect::<Vec<_>>(), [OA_NONE, OA_STOP]);
    // Native taps: a short run confirmed by silence is one impact.
    let mut s = OaState { native_taps: true, ..OaState::default() };
    assert_eq!(oa_period(&mut s, &loud, 240, 1).action, OA_NONE);
    assert_eq!(oa_period(&mut s, &quiet, 240, 1).action, OA_NONE);
    let r = oa_period(&mut s, &quiet, 240, 1);
    assert!(r.action == OA_IMPACT && r.intensity == 488);
}
