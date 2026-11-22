// SPDX-License-Identifier: MIT
//! The guest's two camera headers, mirrored (they stay C for the kernel's driver; tests/headers.rs checks every
//! constant, size and offset against them):
//!
//! - `drivers/camera/oneios_camera_proto.h`: the `oneios,camera` virtio-mmio device between xnuhost and the guest
//!   driver. Queue 0 (control): one chain per command, a readable [`OcRequest`], then a writable [`OcReply`]; the host
//!   answers at once. Queue 1 (frames): the guest posts writable buffers of [`OC_BUFFER_BYTES`]; the host fills one
//!   per frame, an [`OcFrame`] and then `bytes` of picture at [`OC_FRAME_DATA`]. A buffer with `bytes == 0` and
//!   `status != 0` says the camera stopped with that errno; `bytes == 0` and `status == 0` carries nothing (the
//!   guest posts it again). All little-endian.
//! - `drivers/camera/oneios_camera_abi.h`: `/dev/oneios-camera`, what the guest's capture plug-in reads (the layout
//!   of iOSPlus10's /dev/negronicam and HakiOS's /dev/hakios-camera: a 120-byte `NCFRAME1` header, a 24-byte command).
//!   The host never speaks it; it is mirrored so that the Rust side has every camera layout in one place.
use crate::pod::Pod;

pub const OC_VIRTIO_DEVICE_ID: u32 = 0x4f43; // not a registered VirtIO id; the guest matches the node, not the id
pub const OC_PROTO_VERSION: u32 = 1;
pub const OC_QUEUE_CONTROL: u32 = 0;
pub const OC_QUEUE_FRAME: u32 = 1;

/// The largest picture the transport carries (NV12: 12 bits a pixel).
pub const OC_MAX_WIDTH: u32 = 1920;
pub const OC_MAX_HEIGHT: u32 = 1080;
/// The picture's offset in a frame buffer.
pub const OC_FRAME_DATA: u32 = 64;
pub const OC_FRAME_BYTES_MAX: u32 = OC_MAX_WIDTH * OC_MAX_HEIGHT * 3 / 2;
pub const OC_BUFFER_BYTES: u32 = OC_FRAME_DATA + OC_FRAME_BYTES_MAX;
pub const OC_MAX_CAMERAS: u32 = 8;

/// Which camera (`OC_ROLE_*`). The S24 Ultra's five: S5KHP2 wide, S5K3LU front, IMX564 ultra-wide, IMX754 3x,
/// IMX854 5x.
#[repr(u32)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Role {
    #[default]
    None = 0,
    Wide = 1,
    Front = 2,
    Ultrawide = 3,
    Tele3 = 4,
    Tele5 = 5,
    Other = 6,
}

impl Role {
    pub const ALL: [Role; 7] =
        [Role::None, Role::Wide, Role::Front, Role::Ultrawide, Role::Tele3, Role::Tele5, Role::Other];

    /// The role of a wire value; `None` for one that is not a role.
    pub fn from_wire(value: u32) -> Option<Role> {
        Role::ALL.get(value as usize).copied()
    }

    /// The name camctl and the logs use (`cam_role_name`): "none", "wide", ... "other".
    pub fn name(self) -> &'static str {
        ["none", "wide", "front", "ultrawide", "tele3", "tele5", "other"][self as usize]
    }

    /// The name of a wire value: a value that is not a role is "other", as `cam_role_name`.
    pub fn name_of(value: u32) -> &'static str {
        Role::from_wire(value).unwrap_or(Role::Other).name()
    }

    /// A role by its name (`cam_role_from_name`): only the five cameras' names; "none", "other" and anything else
    /// are [`Role::None`].
    pub fn from_name(name: &str) -> Role {
        Role::ALL[1..6].iter().copied().find(|r| r.name() == name).unwrap_or(Role::None)
    }
}

pub const OC_ROLE_NONE: u32 = Role::None as u32;
pub const OC_ROLE_WIDE: u32 = Role::Wide as u32;
pub const OC_ROLE_FRONT: u32 = Role::Front as u32;
pub const OC_ROLE_ULTRAWIDE: u32 = Role::Ultrawide as u32;
pub const OC_ROLE_TELE3: u32 = Role::Tele3 as u32;
pub const OC_ROLE_TELE5: u32 = Role::Tele5 as u32;
pub const OC_ROLE_OTHER: u32 = Role::Other as u32;

/// Y plane, then interleaved CbCr at half size; rows `stride` apart.
pub const OC_FMT_NV12: u32 = 1;
pub const OC_FACING_FRONT: i32 = 0;
pub const OC_FACING_BACK: i32 = 1;
pub const OC_FACING_EXTERNAL: i32 = 2;

pub const OC_CMD_INFO: u32 = 1;
pub const OC_CMD_START: u32 = 2;
pub const OC_CMD_STOP: u32 = 3;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OcRequest {
    /// `OC_CMD_*`
    pub code: u32,
    /// START: `OC_ROLE_*`
    pub role: u32,
    /// START: the picture asked for (the nearest the camera has is used)
    pub width: u32,
    pub height: u32,
    /// START: 0 = 30
    pub fps: u32,
    pub reserved: [u32; 3],
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OcCamera {
    pub role: u32,
    pub facing: u32,
    /// degrees the sensor is turned from the phone's upright position
    pub orientation: u32,
    /// the largest picture it gives within the transport's limit
    pub max_width: u32,
    pub max_height: u32,
    pub max_fps: u32,
    /// focal length in micrometres
    pub focal_um: u32,
    /// 35 mm equivalent focal length, 0 = not known
    pub equiv35_mm: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OcReply {
    /// 0, or an errno
    pub status: u32,
    /// INFO: cameras that follow
    pub count: u32,
    pub cameras: [OcCamera; OC_MAX_CAMERAS as usize],
}

/// "OCF1"
pub const OC_FRAME_MAGIC: u32 = 0x3146_434f;

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OcFrame {
    pub magic: u32,
    /// 0, or the errno the camera stopped with (then bytes is 0)
    pub status: u32,
    /// rises by one a frame the camera made (a gap = frames the guest missed); never 0
    pub sequence: u64,
    /// the sensor's time for the frame (host CLOCK_BOOTTIME or MONOTONIC)
    pub timestamp_ns: u64,
    pub width: u32,
    pub height: u32,
    /// bytes from one row to the next, both planes
    pub stride: u32,
    /// OC_FMT_NV12
    pub format: u32,
    /// stride * height * 3 / 2
    pub bytes: u32,
    pub role: u32,
    pub orientation: u32,
    /// what the camera was asked for
    pub fps: u32,
    pub reserved: [u32; 2],
}

// SAFETY: repr(C), u32/u64 fields only, laid out without padding (the static asserts below and tests/headers.rs).
unsafe impl Pod for OcRequest {}
// SAFETY: as above.
unsafe impl Pod for OcCamera {}
// SAFETY: as above.
unsafe impl Pod for OcReply {}
// SAFETY: as above.
unsafe impl Pod for OcFrame {}

const _: () = assert!(
    std::mem::size_of::<OcRequest>() == 32
        && std::mem::size_of::<OcFrame>() == OC_FRAME_DATA as usize
        && std::mem::size_of::<OcReply>() == 8 + 32 * OC_MAX_CAMERAS as usize,
    "wire layout"
);

// ---- /dev/oneios-camera (oneios_camera_abi.h) ---------------------------------------------------------------------
pub const ONEIOS_CAMERA_DEVICE: &str = "/dev/oneios-camera";
pub const ONEIOS_CAMERA_MAGIC: &[u8; 8] = b"NCFRAME1";
pub const ONEIOS_CAMERA_RAW10: u32 = 1;
pub const ONEIOS_CAMERA_NV12: u32 = 2;
pub const ONEIOS_CAMERA_NO_FOCUS: u32 = 0xffff_ffff;
// The values of OC_ROLE_*: 1 is the rear main camera, as in NCFRAME1.
pub const ONEIOS_CAMERA_WIDE: u32 = 1;
pub const ONEIOS_CAMERA_FRONT: u32 = 2;
pub const ONEIOS_CAMERA_ULTRAWIDE: u32 = 3;
pub const ONEIOS_CAMERA_TELE3: u32 = 4;
pub const ONEIOS_CAMERA_TELE5: u32 = 5;
// The commands: EXPOSURE, KEEP, FOCUS and MODE are ignored; SENSOR picks a camera; FORMAT the size and rate.
pub const ONEIOS_CAMERA_EXPOSURE: u32 = 1;
pub const ONEIOS_CAMERA_KEEP: u32 = 2;
pub const ONEIOS_CAMERA_SENSOR: u32 = 3;
pub const ONEIOS_CAMERA_FOCUS: u32 = 4;
pub const ONEIOS_CAMERA_MODE: u32 = 5;
pub const ONEIOS_CAMERA_FORMAT: u32 = 6;

/// The 120-byte frame header `read()` gives (NCFRAME1), then the picture.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OneiosCameraFrame {
    pub magic: [u8; 8],
    pub header_bytes: u32,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub format: u32,
    pub orientation: u32,
    pub facing: u32,
    pub sequence: u64,
    pub time: u64,
    pub exposure_lines: u32,
    pub line_ns: u32,
    pub gain: u32,
    pub gain_unity: u32,
    pub gain_max: u32,
    pub fps: u32,
    pub host_time_lo: u32,
    pub host_time_hi: u32,
    pub margin: u32,
    pub sensor: u32,
    pub focus: u32,
    pub mode: u32,
    pub focus_min: u32,
    pub focus_max: u32,
    pub sensor_w: u32,
    pub sensor_h: u32,
}

/// One `write()`: a command and its arguments.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OneiosCameraCommand {
    pub command: u32,
    /// of what follows this structure (0)
    pub bytes: u32,
    pub a: u32,
    pub b: u32,
    pub c: u32,
    pub d: u32,
}

const _: () = assert!(std::mem::size_of::<OneiosCameraFrame>() == 120, "the frame header is 120 bytes (NCFRAME1)");
const _: () = assert!(std::mem::size_of::<OneiosCameraCommand>() == 24, "the command is 24 bytes (NCFRAME1)");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roles() {
        assert_eq!(Role::from_name("tele5"), Role::Tele5);
        assert_eq!(Role::from_name("other"), Role::None);
        assert_eq!(Role::from_name("none"), Role::None);
        assert_eq!(Role::Ultrawide.name(), "ultrawide");
        assert_eq!(Role::name_of(99), "other");
        assert_eq!(Role::from_wire(6), Some(Role::Other));
        assert_eq!(Role::from_wire(7), None);
    }

    #[test]
    fn bytes() {
        let f = OcFrame { magic: OC_FRAME_MAGIC, sequence: 7, ..Default::default() };
        let b = f.as_bytes();
        assert_eq!(&b[0..4], b"OCF1");
        assert_eq!(OcFrame::read_from(b), f);
        assert_eq!(OcFrame::read_from(&b[..4]).magic, OC_FRAME_MAGIC);
        assert_eq!(OcFrame::read_from(&b[..4]).sequence, 0);
    }
}
