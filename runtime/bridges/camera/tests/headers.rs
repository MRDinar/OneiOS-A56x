// SPDX-License-Identifier: MIT
//! The Rust mirrors of the guest's camera headers (drivers/camera/oneios_camera_proto.h, oneios_camera_abi.h) against
//! the headers: every constant, every struct's size and every field's offset and size, and every name of a header
//! mirrored.
mod cprobe;

use cprobe::{check_consts, check_layout, Probe};
use onecamera::proto::*;
use std::path::{Path, PathBuf};

fn drivers() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../drivers/camera")
}

#[test]
fn camera_proto() {
    let header = drivers().join("oneios_camera_proto.h");
    let consts: &[(&str, u64)] = &[
        ("OC_VIRTIO_DEVICE_ID", OC_VIRTIO_DEVICE_ID.into()),
        ("OC_PROTO_VERSION", OC_PROTO_VERSION.into()),
        ("OC_QUEUE_CONTROL", OC_QUEUE_CONTROL.into()),
        ("OC_QUEUE_FRAME", OC_QUEUE_FRAME.into()),
        ("OC_MAX_WIDTH", OC_MAX_WIDTH.into()),
        ("OC_MAX_HEIGHT", OC_MAX_HEIGHT.into()),
        ("OC_FRAME_DATA", OC_FRAME_DATA.into()),
        ("OC_FRAME_BYTES_MAX", OC_FRAME_BYTES_MAX.into()),
        ("OC_BUFFER_BYTES", OC_BUFFER_BYTES.into()),
        ("OC_MAX_CAMERAS", OC_MAX_CAMERAS.into()),
        ("OC_ROLE_NONE", OC_ROLE_NONE.into()),
        ("OC_ROLE_WIDE", OC_ROLE_WIDE.into()),
        ("OC_ROLE_FRONT", OC_ROLE_FRONT.into()),
        ("OC_ROLE_ULTRAWIDE", OC_ROLE_ULTRAWIDE.into()),
        ("OC_ROLE_TELE3", OC_ROLE_TELE3.into()),
        ("OC_ROLE_TELE5", OC_ROLE_TELE5.into()),
        ("OC_ROLE_OTHER", OC_ROLE_OTHER.into()),
        ("OC_FMT_NV12", OC_FMT_NV12.into()),
        ("OC_FACING_FRONT", OC_FACING_FRONT as u64),
        ("OC_FACING_BACK", OC_FACING_BACK as u64),
        ("OC_FACING_EXTERNAL", OC_FACING_EXTERNAL as u64),
        ("OC_CMD_INFO", OC_CMD_INFO.into()),
        ("OC_CMD_START", OC_CMD_START.into()),
        ("OC_CMD_STOP", OC_CMD_STOP.into()),
        ("OC_FRAME_MAGIC", OC_FRAME_MAGIC.into()),
    ];
    let mut probe = Probe::new(&header, &[]);
    for (name, _) in consts {
        probe.ask(name, name);
    }
    probe
        .layout("struct oc_request", &["code", "role", "width", "height", "fps", "reserved"])
        .layout(
            "struct oc_camera",
            &["role", "facing", "orientation", "max_width", "max_height", "max_fps", "focal_um", "equiv35_mm"],
        )
        .layout("struct oc_reply", &["status", "count", "cameras"])
        .layout(
            "struct oc_frame",
            &[
                "magic",
                "status",
                "sequence",
                "timestamp_ns",
                "width",
                "height",
                "stride",
                "format",
                "bytes",
                "role",
                "orientation",
                "fps",
                "reserved",
            ],
        );
    let Some(values) = probe.run() else { return };
    check_consts(&header, &values, consts, &[]);
    check_layout(
        &values,
        "struct oc_request",
        size_of::<OcRequest>(),
        &[
            field!(OcRequest, code),
            field!(OcRequest, role),
            field!(OcRequest, width),
            field!(OcRequest, height),
            field!(OcRequest, fps),
            field!(OcRequest, reserved),
        ],
    );
    check_layout(
        &values,
        "struct oc_camera",
        size_of::<OcCamera>(),
        &[
            field!(OcCamera, role),
            field!(OcCamera, facing),
            field!(OcCamera, orientation),
            field!(OcCamera, max_width),
            field!(OcCamera, max_height),
            field!(OcCamera, max_fps),
            field!(OcCamera, focal_um),
            field!(OcCamera, equiv35_mm),
        ],
    );
    check_layout(
        &values,
        "struct oc_reply",
        size_of::<OcReply>(),
        &[field!(OcReply, status), field!(OcReply, count), field!(OcReply, cameras)],
    );
    check_layout(
        &values,
        "struct oc_frame",
        size_of::<OcFrame>(),
        &[
            field!(OcFrame, magic),
            field!(OcFrame, status),
            field!(OcFrame, sequence),
            field!(OcFrame, timestamp_ns),
            field!(OcFrame, width),
            field!(OcFrame, height),
            field!(OcFrame, stride),
            field!(OcFrame, format),
            field!(OcFrame, bytes),
            field!(OcFrame, role),
            field!(OcFrame, orientation),
            field!(OcFrame, fps),
            field!(OcFrame, reserved),
        ],
    );
}

#[test]
fn camera_abi() {
    let header = drivers().join("oneios_camera_abi.h");
    let consts: &[(&str, u64)] = &[
        ("ONEIOS_CAMERA_RAW10", ONEIOS_CAMERA_RAW10.into()),
        ("ONEIOS_CAMERA_NV12", ONEIOS_CAMERA_NV12.into()),
        ("ONEIOS_CAMERA_NO_FOCUS", ONEIOS_CAMERA_NO_FOCUS.into()),
        ("ONEIOS_CAMERA_WIDE", ONEIOS_CAMERA_WIDE.into()),
        ("ONEIOS_CAMERA_FRONT", ONEIOS_CAMERA_FRONT.into()),
        ("ONEIOS_CAMERA_ULTRAWIDE", ONEIOS_CAMERA_ULTRAWIDE.into()),
        ("ONEIOS_CAMERA_TELE3", ONEIOS_CAMERA_TELE3.into()),
        ("ONEIOS_CAMERA_TELE5", ONEIOS_CAMERA_TELE5.into()),
        ("ONEIOS_CAMERA_EXPOSURE", ONEIOS_CAMERA_EXPOSURE.into()),
        ("ONEIOS_CAMERA_KEEP", ONEIOS_CAMERA_KEEP.into()),
        ("ONEIOS_CAMERA_SENSOR", ONEIOS_CAMERA_SENSOR.into()),
        ("ONEIOS_CAMERA_FOCUS", ONEIOS_CAMERA_FOCUS.into()),
        ("ONEIOS_CAMERA_MODE", ONEIOS_CAMERA_MODE.into()),
        ("ONEIOS_CAMERA_FORMAT", ONEIOS_CAMERA_FORMAT.into()),
    ];
    let mut probe = Probe::new(&header, &[]);
    for (name, _) in consts {
        probe.ask(name, name);
    }
    // The two strings, byte for byte: their lengths and every byte.
    let device = ONEIOS_CAMERA_DEVICE.as_bytes();
    probe.ask("sizeof device", "sizeof(ONEIOS_CAMERA_DEVICE)").ask("sizeof magic", "sizeof(ONEIOS_CAMERA_MAGIC)");
    for i in 0..device.len() {
        probe.ask(&format!("device[{i}]"), &format!("ONEIOS_CAMERA_DEVICE[{i}]"));
    }
    for i in 0..8 {
        probe.ask(&format!("magic[{i}]"), &format!("ONEIOS_CAMERA_MAGIC[{i}]"));
    }
    let frame_fields = [
        "magic",
        "header_bytes",
        "width",
        "height",
        "stride",
        "format",
        "orientation",
        "facing",
        "sequence",
        "time",
        "exposure_lines",
        "line_ns",
        "gain",
        "gain_unity",
        "gain_max",
        "fps",
        "host_time_lo",
        "host_time_hi",
        "margin",
        "sensor",
        "focus",
        "mode",
        "focus_min",
        "focus_max",
        "sensor_w",
        "sensor_h",
    ];
    probe
        .layout("struct oneios_camera_frame", &frame_fields)
        .layout("struct oneios_camera_command", &["command", "bytes", "a", "b", "c", "d"]);
    let Some(values) = probe.run() else { return };
    check_consts(&header, &values, consts, &["ONEIOS_CAMERA_DEVICE", "ONEIOS_CAMERA_MAGIC"]);
    assert_eq!(values["sizeof device"], device.len() as u64 + 1);
    assert_eq!(values["sizeof magic"], 9);
    for (i, b) in device.iter().enumerate() {
        assert_eq!(values[&format!("device[{i}]")], u64::from(*b));
    }
    for (i, b) in ONEIOS_CAMERA_MAGIC.iter().enumerate() {
        assert_eq!(values[&format!("magic[{i}]")], u64::from(*b));
    }
    type F = OneiosCameraFrame;
    check_layout(
        &values,
        "struct oneios_camera_frame",
        size_of::<F>(),
        &[
            field!(F, magic),
            field!(F, header_bytes),
            field!(F, width),
            field!(F, height),
            field!(F, stride),
            field!(F, format),
            field!(F, orientation),
            field!(F, facing),
            field!(F, sequence),
            field!(F, time),
            field!(F, exposure_lines),
            field!(F, line_ns),
            field!(F, gain),
            field!(F, gain_unity),
            field!(F, gain_max),
            field!(F, fps),
            field!(F, host_time_lo),
            field!(F, host_time_hi),
            field!(F, margin),
            field!(F, sensor),
            field!(F, focus),
            field!(F, mode),
            field!(F, focus_min),
            field!(F, focus_max),
            field!(F, sensor_w),
            field!(F, sensor_h),
        ],
    );
    type C = OneiosCameraCommand;
    check_layout(
        &values,
        "struct oneios_camera_command",
        size_of::<C>(),
        &[field!(C, command), field!(C, bytes), field!(C, a), field!(C, b), field!(C, c), field!(C, d)],
    );
}
