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

/// The NDK's numbers the Android back ends use (backend_ndk.rs, meta.rs: the C's static asserts), against the NDK's
/// headers.
#[test]
fn ndk_numbers() {
    use onecamera::backend_ndk::*;
    use onecamera::meta::*;
    let consts: &[(&str, i64)] = &[
        ("ACAMERA_OK", ACAMERA_OK.into()),
        ("ACAMERA_ERROR_UNKNOWN", ACAMERA_ERROR_UNKNOWN.into()),
        ("ACAMERA_ERROR_INVALID_PARAMETER", ACAMERA_ERROR_INVALID_PARAMETER.into()),
        ("ACAMERA_ERROR_CAMERA_DISCONNECTED", ACAMERA_ERROR_CAMERA_DISCONNECTED.into()),
        ("ACAMERA_ERROR_NOT_ENOUGH_MEMORY", ACAMERA_ERROR_NOT_ENOUGH_MEMORY.into()),
        ("ACAMERA_ERROR_METADATA_NOT_FOUND", ACAMERA_ERROR_METADATA_NOT_FOUND.into()),
        ("ACAMERA_ERROR_CAMERA_DEVICE", ACAMERA_ERROR_CAMERA_DEVICE.into()),
        ("ACAMERA_ERROR_CAMERA_SERVICE", ACAMERA_ERROR_CAMERA_SERVICE.into()),
        ("ACAMERA_ERROR_SESSION_CLOSED", ACAMERA_ERROR_SESSION_CLOSED.into()),
        ("ACAMERA_ERROR_INVALID_OPERATION", ACAMERA_ERROR_INVALID_OPERATION.into()),
        ("ACAMERA_ERROR_STREAM_CONFIGURE_FAIL", ACAMERA_ERROR_STREAM_CONFIGURE_FAIL.into()),
        ("ACAMERA_ERROR_CAMERA_IN_USE", ACAMERA_ERROR_CAMERA_IN_USE.into()),
        ("ACAMERA_ERROR_MAX_CAMERA_IN_USE", ACAMERA_ERROR_MAX_CAMERA_IN_USE.into()),
        ("ACAMERA_ERROR_CAMERA_DISABLED", ACAMERA_ERROR_CAMERA_DISABLED.into()),
        ("ACAMERA_ERROR_PERMISSION_DENIED", ACAMERA_ERROR_PERMISSION_DENIED.into()),
        ("ACAMERA_ERROR_UNSUPPORTED_OPERATION", ACAMERA_ERROR_UNSUPPORTED_OPERATION.into()),
        ("ERROR_CAMERA_IN_USE", ERROR_CAMERA_IN_USE.into()),
        ("ERROR_MAX_CAMERAS_IN_USE", ERROR_MAX_CAMERAS_IN_USE.into()),
        ("ERROR_CAMERA_DISABLED", ERROR_CAMERA_DISABLED.into()),
        ("ERROR_CAMERA_DEVICE", ERROR_CAMERA_DEVICE.into()),
        ("ERROR_CAMERA_SERVICE", ERROR_CAMERA_SERVICE.into()),
        ("TEMPLATE_PREVIEW", TEMPLATE_PREVIEW.into()),
        ("CAPTURE_FAILURE_REASON_FLUSHED", CAPTURE_FAILURE_REASON_FLUSHED.into()),
        ("ACAMERA_SENSOR_TIMESTAMP", ACAMERA_SENSOR_TIMESTAMP.into()),
        ("ACAMERA_CONTROL_ZOOM_RATIO", ACAMERA_CONTROL_ZOOM_RATIO.into()),
        ("AIMAGE_FORMAT_YUV_420_888", AIMAGE_FORMAT_YUV_420_888.into()),
        ("AHARDWAREBUFFER_USAGE_CPU_READ_OFTEN", AHARDWAREBUFFER_USAGE_CPU_READ_OFTEN as i64),
        // meta.rs reads the same numbers from the HAL's buffers (backend_ndk.c's _Static_assert)
        ("ACAMERA_CONTROL_AE_TARGET_FPS_RANGE", CAM_TAG_AE_TARGET_FPS_RANGE.into()),
        ("ACAMERA_CONTROL_AE_AVAILABLE_TARGET_FPS_RANGES", CAM_TAG_AE_AVAILABLE_FPS_RANGES.into()),
        ("ACAMERA_LENS_FACING", CAM_TAG_LENS_FACING.into()),
        ("ACAMERA_LENS_INFO_AVAILABLE_FOCAL_LENGTHS", CAM_TAG_FOCAL_LENGTHS.into()),
        ("ACAMERA_REQUEST_AVAILABLE_CAPABILITIES", CAM_TAG_CAPABILITIES.into()),
        ("ACAMERA_SCALER_AVAILABLE_STREAM_CONFIGURATIONS", CAM_TAG_STREAM_CONFIGURATIONS.into()),
        ("ACAMERA_SENSOR_ORIENTATION", CAM_TAG_SENSOR_ORIENTATION.into()),
        ("ACAMERA_SENSOR_INFO_PHYSICAL_SIZE", CAM_TAG_PHYSICAL_SIZE.into()),
        ("ACAMERA_SENSOR_INFO_PIXEL_ARRAY_SIZE", CAM_TAG_PIXEL_ARRAY_SIZE.into()),
        ("ACAMERA_REQUEST_AVAILABLE_CAPABILITIES_LOGICAL_MULTI_CAMERA", CAPABILITY_LOGICAL_MULTI_CAMERA.into()),
        ("ACAMERA_LENS_FACING_FRONT", OC_FACING_FRONT.into()),
        ("ACAMERA_LENS_FACING_BACK", OC_FACING_BACK.into()),
        ("ACAMERA_TYPE_BYTE", CAM_META_BYTE.into()),
        ("ACAMERA_TYPE_INT32", CAM_META_INT32.into()),
        ("ACAMERA_TYPE_FLOAT", CAM_META_FLOAT.into()),
        ("ACAMERA_TYPE_INT64", CAM_META_INT64.into()),
        ("ACAMERA_TYPE_DOUBLE", CAM_META_DOUBLE.into()),
        ("ACAMERA_TYPE_RATIONAL", CAM_META_RATIONAL.into()),
        // backend_hal.rs
        ("FIRST_CALL_TRANSACTION", onecamera::backend_hal::FIRST_CALL_TRANSACTION.into()),
    ];
    let mut probe = Probe::ndk(&[
        "camera/NdkCameraManager.h",
        "camera/NdkCameraMetadataTags.h",
        "media/NdkImageReader.h",
        "android/hardware_buffer.h",
        "android/binder_ibinder.h",
    ]);
    for (name, _) in consts {
        probe.ask(name, &format!("(long long){name}"));
    }
    probe.ask("STATUS_UNKNOWN_TRANSACTION", "(long long)STATUS_UNKNOWN_TRANSACTION");
    let Some(values) = probe.run() else { return };
    for (name, v) in consts {
        assert_eq!(values[*name] as i64, *v, "{name}");
    }
    assert_eq!(values["STATUS_UNKNOWN_TRANSACTION"] as i64, -i64::from(libc::EBADMSG));
}
