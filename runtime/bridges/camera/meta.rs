// SPDX-License-Identifier: MIT
//! Android's camera_metadata buffer, read without libcamera_metadata (not an NDK library), and what the helper
//! derives from a camera's characteristics: which of the phone's cameras it is, the picture sizes and frame rates it
//! gives.
//!
//! Layout (AOSP system/media/camera/src/camera_metadata.c, stable since Android 5): a 48-byte header { size, version,
//! flags, entry_count, entry_capacity, entries_start, data_count, data_capacity, data_start, padding, vendor_id
//! (u64) }, then 16-byte entries { tag, count, offset-or-4-bytes, type, 3 reserved }: values of at most 4 bytes are
//! in the entry, the others at data_start + offset.
use crate::proto::{OcCamera, Role, OC_FACING_BACK, OC_FACING_EXTERNAL, OC_FACING_FRONT, OC_MAX_HEIGHT, OC_MAX_WIDTH};

pub const CAM_META_BYTE: u8 = 0;
pub const CAM_META_INT32: u8 = 1;
pub const CAM_META_FLOAT: u8 = 2;
pub const CAM_META_INT64: u8 = 3;
pub const CAM_META_DOUBLE: u8 = 4;
pub const CAM_META_RATIONAL: u8 = 5;

pub const CAM_TAG_AE_TARGET_FPS_RANGE: u32 = 0x10005;
pub const CAM_TAG_AE_AVAILABLE_FPS_RANGES: u32 = 0x10014;
pub const CAM_TAG_LENS_FACING: u32 = 0x80005;
pub const CAM_TAG_FOCAL_LENGTHS: u32 = 0x90002;
pub const CAM_TAG_CAPABILITIES: u32 = 0xc000c;
pub const CAM_TAG_STREAM_CONFIGURATIONS: u32 = 0xd000a;
pub const CAM_TAG_SENSOR_ORIENTATION: u32 = 0xe000e;
pub const CAM_TAG_PHYSICAL_SIZE: u32 = 0xf0005;
pub const CAM_TAG_PIXEL_ARRAY_SIZE: u32 = 0xf0006;
pub const CAM_TAG_BUFFER_MANAGEMENT: u32 = 0x150002;

/// REQUEST_AVAILABLE_CAPABILITIES_LOGICAL_MULTI_CAMERA
pub const CAPABILITY_LOGICAL_MULTI_CAMERA: u8 = 11;
/// HAL_PIXEL_FORMAT_YCbCr_420_888 (AIMAGE_FORMAT_YUV_420_888)
pub const FORMAT_YUV_420_888: i32 = 0x23;

pub const CAM_MAX_SIZES: usize = 96;
pub const CAM_MAX_LIST: usize = 32;
pub const CAM_MAX_FPS: usize = 32;
/// A camera's id and parent are kept to 31 bytes, as C's `char id[32]`.
const ID_MAX: usize = 31;

const TYPE_SIZE: [u64; 6] = [1, 4, 4, 8, 8, 8];

/// Why an entry could not be had (`-EINVAL`, `-ENOENT`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MetaError {
    /// The buffer is broken (or a value is not what it should be).
    Invalid,
    /// No entry of that tag.
    NotFound,
}

impl MetaError {
    pub fn errno(self) -> i32 {
        match self {
            MetaError::Invalid => libc::EINVAL,
            MetaError::NotFound => libc::ENOENT,
        }
    }
}

/// One entry: its values' bytes, how many values and their type.
#[derive(Clone, Copy, Debug)]
pub struct Entry<'a> {
    pub data: &'a [u8],
    pub count: u32,
    pub ty: u8,
}

impl Entry<'_> {
    fn word(&self, i: usize) -> [u8; 4] {
        self.data[i * 4..i * 4 + 4].try_into().unwrap()
    }
    pub fn i32_at(&self, i: usize) -> i32 {
        i32::from_ne_bytes(self.word(i))
    }
    pub fn f32_at(&self, i: usize) -> f32 {
        f32::from_ne_bytes(self.word(i))
    }
    /// The entry when it is of type `ty` with at least `min` values.
    fn of(self, ty: u8, min: u32) -> Option<Self> {
        (self.ty == ty && self.count >= min).then_some(self)
    }
}

fn rd32(buffer: &[u8], at: usize) -> u32 {
    u32::from_ne_bytes(buffer[at..at + 4].try_into().unwrap())
}

/// The entry of `tag`: (entry's offset, its values' offset, count, type).
fn locate(buffer: &[u8], tag: u32) -> Result<(usize, usize, u32, u8), MetaError> {
    if buffer.len() < 48 {
        return Err(MetaError::Invalid);
    }
    let total = rd32(buffer, 0) as usize;
    let entries = rd32(buffer, 12) as usize;
    let entries_start = rd32(buffer, 20) as usize;
    let data_count = rd32(buffer, 24) as usize;
    let data_start = rd32(buffer, 32) as usize;
    if total > buffer.len()
        || entries_start > total
        || entries as u64 * 16 > (total - entries_start) as u64
        || data_start > total
        || data_count > total - data_start
    {
        return Err(MetaError::Invalid);
    }
    for i in 0..entries {
        let at = entries_start + i * 16;
        if rd32(buffer, at) != tag {
            continue;
        }
        let n = rd32(buffer, at + 4);
        let ty = buffer[at + 12];
        if ty > CAM_META_RATIONAL {
            return Err(MetaError::Invalid);
        }
        let bytes = u64::from(n) * TYPE_SIZE[ty as usize];
        let mut values = at + 8;
        if bytes > 4 {
            let offset = rd32(buffer, at + 8) as u64;
            if offset > data_count as u64 || bytes > data_count as u64 - offset {
                return Err(MetaError::Invalid);
            }
            values = data_start + offset as usize;
        }
        return Ok((at, values, n, ty));
    }
    Err(MetaError::NotFound)
}

fn value_bytes(count: u32, ty: u8) -> usize {
    count as usize * TYPE_SIZE[ty as usize] as usize
}

/// The entry of `tag` (`cam_meta_find`).
pub fn find(buffer: &[u8], tag: u32) -> Result<Entry<'_>, MetaError> {
    let (_, values, count, ty) = locate(buffer, tag)?;
    Ok(Entry { data: &buffer[values..values + value_bytes(count, ty)], count, ty })
}

/// Overwrites an int32 entry's values in place, the same count (`cam_meta_set_i32`).
pub fn set_i32(buffer: &mut [u8], tag: u32, values: &[i32]) -> Result<(), MetaError> {
    let (_, data, have, ty) = locate(buffer, tag)?;
    if ty != CAM_META_INT32 || have as usize != values.len() {
        return Err(MetaError::Invalid);
    }
    for (i, v) in values.iter().enumerate() {
        buffer[data + i * 4..data + i * 4 + 4].copy_from_slice(&v.to_ne_bytes());
    }
    Ok(())
}

/// What the helper knows of one camera (`cam_info`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CamInfo {
    /// the camera's id for its back end
    pub id: String,
    /// "" or, for a physical camera that is not listed by itself, the logical camera it is opened through
    pub parent: String,
    pub role: Role,
    pub facing: i32,
    pub orientation: i32,
    pub logical: bool,
    pub hal_buffers: bool,
    pub focal_mm: f32,
    pub physical_w: f32,
    pub physical_h: f32,
    pub equiv35: f32,
    pub pixel_w: i32,
    pub pixel_h: i32,
    /// YUV_420_888 outputs (at most [`CAM_MAX_SIZES`])
    pub sizes: Vec<(i32, i32)>,
    /// fps ranges, (lo, hi) (at most [`CAM_MAX_FPS`])
    pub fps: Vec<(i32, i32)>,
}

fn short(text: &str) -> String {
    crate::util::truncate(text, ID_MAX).to_owned()
}

/// A camera's characteristics, from wherever they come (the HAL's metadata buffer, the NDK's ACameraMetadata): the
/// entry of a tag, if there is one.
pub trait Characteristics {
    fn entry(&self, tag: u32) -> Option<Entry<'_>>;
}

impl Characteristics for [u8] {
    fn entry(&self, tag: u32) -> Option<Entry<'_>> {
        find(self, tag).ok()
    }
}

impl CamInfo {
    /// A camera of `id` (and `parent`, "" for none): only the names, the facing external.
    pub fn named(id: &str, parent: &str) -> CamInfo {
        CamInfo { id: short(id), parent: short(parent), facing: OC_FACING_EXTERNAL, ..Default::default() }
    }

    /// The fields from a camera's characteristics (`cam_info_from_meta`, and backend_ndk.c's `fill_info`), then
    /// [`CamInfo::finish`]. The id and parent stay.
    pub fn read(&mut self, meta: &(impl Characteristics + ?Sized)) {
        self.facing = OC_FACING_EXTERNAL;
        if let Some(e) = meta.entry(CAM_TAG_LENS_FACING).and_then(|e| e.of(CAM_META_BYTE, 1)) {
            self.facing = i32::from(e.data[0]);
        }
        if let Some(e) = meta.entry(CAM_TAG_SENSOR_ORIENTATION).and_then(|e| e.of(CAM_META_INT32, 1)) {
            self.orientation = e.i32_at(0);
        }
        if let Some(e) = meta.entry(CAM_TAG_FOCAL_LENGTHS).and_then(|e| e.of(CAM_META_FLOAT, 1)) {
            self.focal_mm = e.f32_at(0);
        }
        if let Some(e) = meta.entry(CAM_TAG_PHYSICAL_SIZE).and_then(|e| e.of(CAM_META_FLOAT, 2)) {
            (self.physical_w, self.physical_h) = (e.f32_at(0), e.f32_at(1));
        }
        if let Some(e) = meta.entry(CAM_TAG_PIXEL_ARRAY_SIZE).and_then(|e| e.of(CAM_META_INT32, 2)) {
            (self.pixel_w, self.pixel_h) = (e.i32_at(0), e.i32_at(1));
        }
        if let Some(e) = meta.entry(CAM_TAG_CAPABILITIES).and_then(|e| e.of(CAM_META_BYTE, 0)) {
            if e.data.contains(&CAPABILITY_LOGICAL_MULTI_CAMERA) {
                self.logical = true;
            }
        }
        // The HAL asks for its buffers itself (requestStreamBuffers) when this says HIDL_DEVICE_3_5 (0).
        if meta.entry(CAM_TAG_BUFFER_MANAGEMENT).and_then(|e| e.of(CAM_META_BYTE, 1)).is_some() {
            self.hal_buffers = true;
        }
        self.sizes.clear();
        if let Some(e) = meta.entry(CAM_TAG_STREAM_CONFIGURATIONS).and_then(|e| e.of(CAM_META_INT32, 0)) {
            let n = e.count as usize;
            let mut i = 0;
            while i + 3 < n && self.sizes.len() < CAM_MAX_SIZES {
                if e.i32_at(i) == FORMAT_YUV_420_888 && e.i32_at(i + 3) == 0 {
                    self.sizes.push((e.i32_at(i + 1), e.i32_at(i + 2))); // YCbCr_420_888, output
                }
                i += 4;
            }
        }
        self.fps.clear();
        if let Some(e) = meta.entry(CAM_TAG_AE_AVAILABLE_FPS_RANGES).and_then(|e| e.of(CAM_META_INT32, 0)) {
            let n = e.count as usize;
            let mut i = 0;
            while i + 1 < n && self.fps.len() < CAM_MAX_FPS {
                self.fps.push((e.i32_at(i), e.i32_at(i + 1)));
                i += 2;
            }
        }
        self.finish();
    }

    /// The 35 mm equivalent from the focal length and the sensor's size (`cam_info_finish`): the full frame's
    /// diagonal (43.27 mm) over the sensor's.
    pub fn finish(&mut self) {
        let diagonal = (self.physical_w * self.physical_w + self.physical_h * self.physical_h).sqrt();
        self.equiv35 = if self.focal_mm > 0.0 && diagonal > 0.0 { self.focal_mm * 43.27 / diagonal } else { 0.0 };
    }

    /// The S24 Ultra's rear lenses are 13, 24, 67 and 111 mm equivalent. The limits sit between them.
    fn natural_role(&self) -> Role {
        match self.facing {
            OC_FACING_FRONT => return Role::Front,
            OC_FACING_BACK => {}
            _ => return Role::Other,
        }
        match self.equiv35 {
            e if e <= 0.0 => Role::Wide, // not said: the main camera
            e if e < 18.0 => Role::Ultrawide,
            e if e < 45.0 => Role::Wide,
            e if e < 90.0 => Role::Tele3,
            _ => Role::Tele5,
        }
    }

    /// The picture size to use for a request (`cam_pick_size`): the same size if the camera has it, else the
    /// largest with the same shape that is not larger, else the largest that fits the transport.
    pub fn pick_size(&self, width: i32, height: i32) -> Option<(i32, i32)> {
        let mut best: Option<(i32, i32)> = None;
        let mut fallback: Option<(i32, i32)> = None;
        for &(w, h) in &self.sizes {
            if w <= 0 || h <= 0 || w > OC_MAX_WIDTH as i32 || h > OC_MAX_HEIGHT as i32 || w & 1 != 0 || h & 1 != 0 {
                continue;
            }
            if w == width && h == height {
                best = Some((w, h));
                break;
            }
            let same_shape = i64::from(w) * i64::from(height) == i64::from(h) * i64::from(width);
            if w <= width && h <= height && same_shape && best.is_none_or(|b| w > b.0) {
                best = Some((w, h));
            }
            if fallback.is_none_or(|f| i64::from(w) * i64::from(h) > i64::from(f.0) * i64::from(f.1)) {
                fallback = Some((w, h));
            }
        }
        best.or(fallback)
    }

    /// The frame-rate range for `fps` (`cam_pick_fps`): a fixed range at the rate is best; then the nearest top rate,
    /// the narrower range first. None when the camera lists none.
    pub fn pick_fps(&self, fps: i32) -> Option<(i32, i32)> {
        let cost = |&(lo, hi): &(i32, i32)| (hi - fps).abs() * 100 + (hi - lo);
        // The first of the cheapest, as C's strict `<`.
        self.fps.iter().copied().reduce(|best, r| if cost(&r) < cost(&best) { r } else { best })
    }

    /// What the guest is told of the camera (`cam_info_to_wire`).
    pub fn to_wire(&self) -> OcCamera {
        let (w, h) = self.pick_size(OC_MAX_WIDTH as i32, OC_MAX_HEIGHT as i32).unwrap_or((0, 0));
        let top = self.fps.iter().map(|r| r.1).filter(|&hi| hi > 0).max().unwrap_or(0);
        OcCamera {
            role: self.role as u32,
            facing: self.facing as u32,
            orientation: self.orientation as u32,
            max_width: w as u32,
            max_height: h as u32,
            max_fps: if top != 0 { top as u32 } else { 30 },
            focal_um: (self.focal_mm * 1000.0 + 0.5) as u32,
            equiv35_mm: (self.equiv35 + 0.5) as u32,
        }
    }
}

/// One camera per role, the others [`Role::Other`] (`cam_assign_roles`). The first camera of each kind in the list's
/// order has the role (Android lists the main rear and front cameras first); a plain camera is preferred to a logical
/// one made of several, and both to a physical camera that is only reached through its logical camera (a physical
/// stream).
pub fn assign_roles(list: &mut [CamInfo]) {
    let mut taken = [false; Role::Other as usize + 1];
    for pass in 0..3 {
        for camera in list.iter_mut() {
            let kind = if !camera.parent.is_empty() {
                2
            } else if camera.logical {
                1
            } else {
                0
            };
            if kind != pass {
                continue;
            }
            let mut role = camera.natural_role();
            if role != Role::Other && taken[role as usize] {
                role = Role::Other;
            }
            taken[role as usize] = true;
            camera.role = role;
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A camera_metadata buffer of the entries added: 16 entries at most, data after them.
    pub struct Builder {
        pub buffer: Vec<u8>,
        entries: u32,
        data: u32,
    }

    impl Builder {
        pub fn new() -> Builder {
            Builder { buffer: vec![0; 2048], entries: 0, data: 0 }
        }

        pub fn add(&mut self, tag: u32, ty: u8, count: u32, values: &[u8]) -> &mut Self {
            let bytes = count as usize * TYPE_SIZE[ty as usize] as usize;
            let entry = 48 + self.entries as usize * 16;
            self.buffer[entry..entry + 4].copy_from_slice(&tag.to_ne_bytes());
            self.buffer[entry + 4..entry + 8].copy_from_slice(&count.to_ne_bytes());
            self.buffer[entry + 12] = ty;
            if bytes <= 4 {
                self.buffer[entry + 8..entry + 8 + bytes].copy_from_slice(&values[..bytes]);
            } else {
                self.buffer[entry + 8..entry + 12].copy_from_slice(&self.data.to_ne_bytes());
                let at = 48 + 16 * 16 + self.data as usize;
                self.buffer[at..at + bytes].copy_from_slice(&values[..bytes]);
                self.data += (bytes as u32 + 7) & !7;
            }
            self.entries += 1;
            self
        }

        pub fn i32s(&mut self, tag: u32, values: &[i32]) -> &mut Self {
            let bytes: Vec<u8> = values.iter().flat_map(|v| v.to_ne_bytes()).collect();
            self.add(tag, CAM_META_INT32, values.len() as u32, &bytes)
        }

        pub fn f32s(&mut self, tag: u32, values: &[f32]) -> &mut Self {
            let bytes: Vec<u8> = values.iter().flat_map(|v| v.to_ne_bytes()).collect();
            self.add(tag, CAM_META_FLOAT, values.len() as u32, &bytes)
        }

        pub fn bytes(&mut self, tag: u32, values: &[u8]) -> &mut Self {
            self.add(tag, CAM_META_BYTE, values.len() as u32, values)
        }

        /// The header written; the buffer cut to its size.
        pub fn finish(&mut self) -> Vec<u8> {
            let total = 48 + 16 * 16 + self.data;
            let header = [total, 1, 0, self.entries, 16, 48, self.data, 1024, 48 + 16 * 16, 0, 0, 0];
            for (i, v) in header.iter().enumerate() {
                self.buffer[i * 4..i * 4 + 4].copy_from_slice(&v.to_ne_bytes());
            }
            self.buffer[..total as usize].to_vec()
        }
    }

    /// The wide camera's characteristics, as the C test's.
    pub fn wide_meta() -> Vec<u8> {
        let streams = [
            0x21, 4000, 3000, 0, 0x23, 4000, 3000, 0, 0x23, 1920, 1080, 0, 0x23, 1280, 720, 0, 0x23, 1920, 1080, 1,
            0x22, 640, 480, 0, 0x23, 640, 480, 0, 0x23, 1440, 1080, 0,
        ];
        Builder::new()
            .bytes(CAM_TAG_LENS_FACING, &[1])
            .i32s(CAM_TAG_SENSOR_ORIENTATION, &[90])
            .f32s(CAM_TAG_FOCAL_LENGTHS, &[6.3])
            .f32s(CAM_TAG_PHYSICAL_SIZE, &[9.83, 7.37])
            .i32s(CAM_TAG_PIXEL_ARRAY_SIZE, &[16320, 12240])
            .bytes(CAM_TAG_CAPABILITIES, &[0, 11, 3])
            .i32s(CAM_TAG_STREAM_CONFIGURATIONS, &streams)
            .i32s(CAM_TAG_AE_AVAILABLE_FPS_RANGES, &[15, 30, 30, 30, 60, 60, 7, 24])
            .i32s(CAM_TAG_AE_TARGET_FPS_RANGE, &[15, 30])
            .bytes(CAM_TAG_BUFFER_MANAGEMENT, &[0])
            .finish()
    }

    #[test]
    fn entries() {
        let m = wide_meta();
        let e = find(&m, CAM_TAG_LENS_FACING).unwrap();
        assert!(e.count == 1 && e.ty == CAM_META_BYTE && e.data[0] == 1);
        let e = find(&m, CAM_TAG_PIXEL_ARRAY_SIZE).unwrap();
        assert!(e.count == 2 && e.i32_at(0) == 16320 && e.i32_at(1) == 12240);
        assert_eq!(find(&m, 0x12345).unwrap_err(), MetaError::NotFound);
    }

    #[test]
    fn info_and_choices() {
        let m = wide_meta();
        let mut info = CamInfo::default();
        info.read(&m[..]);
        assert!(info.facing == OC_FACING_BACK && info.orientation == 90 && info.logical && info.hal_buffers);
        assert_eq!(info.pixel_w, 16320);
        assert!(info.equiv35 > 21.0 && info.equiv35 < 24.0);
        assert!(info.sizes.len() == 5 && info.sizes[0].0 == 4000 && info.sizes[4].0 == 1440);
        assert!(info.fps.len() == 4 && info.fps[2].1 == 60);
        assert_eq!(info.pick_size(1280, 720), Some((1280, 720)));
        assert_eq!(info.pick_size(1600, 900), Some((1280, 720))); // the same shape, not larger
        assert_eq!(info.pick_size(800, 800), Some((1920, 1080))); // no such shape: the largest that fits
        assert_eq!(info.pick_size(4000, 3000), Some((1440, 1080))); // never above the transport's limit
        assert_eq!(info.pick_fps(30), Some((30, 30)));
        assert_eq!(info.pick_fps(60), Some((60, 60)));
        assert_eq!(info.pick_fps(25).map(|r| r.1), Some(24));
        info.role = Role::Wide;
        let wire = info.to_wire();
        assert!(wire.role == Role::Wide as u32 && wire.max_width == 1920 && wire.max_height == 1080);
        assert!(wire.max_fps == 60 && wire.focal_um == 6300 && wire.orientation == 90);
        assert_eq!(CamInfo::default().pick_fps(30), None);
        assert_eq!(CamInfo::default().pick_size(640, 480), None);
    }

    #[test]
    fn set_in_place() {
        let mut m = wide_meta();
        assert_eq!(set_i32(&mut m, CAM_TAG_AE_TARGET_FPS_RANGE, &[30, 30]), Ok(()));
        let e = find(&m, CAM_TAG_AE_TARGET_FPS_RANGE).unwrap();
        assert!(e.i32_at(0) == 30 && e.i32_at(1) == 30);
        assert_eq!(set_i32(&mut m, CAM_TAG_AE_TARGET_FPS_RANGE, &[30]), Err(MetaError::Invalid));
        assert_eq!(set_i32(&mut m, CAM_TAG_LENS_FACING, &[30]), Err(MetaError::Invalid));
        assert_eq!(set_i32(&mut m, 0x999, &[30, 30]), Err(MetaError::NotFound));
    }

    #[test]
    fn broken_buffers() {
        // Every truncation, and offsets and counts out of range: refused, not read past.
        let m = wide_meta();
        for cut in (0..m.len()).step_by(3) {
            assert_eq!(find(&m[..cut], CAM_TAG_STREAM_CONFIGURATIONS).unwrap_err(), MetaError::Invalid, "cut {cut}");
        }
        let huge = 0x7fff_ffffu32.to_ne_bytes();
        let broken = |at: usize, bytes: &[u8]| {
            let mut b = m.clone();
            b[at..at + bytes.len()].copy_from_slice(bytes);
            b
        };
        let stream_entry = 48 + 6 * 16;
        assert_eq!(
            find(&broken(stream_entry + 8, &huge), CAM_TAG_STREAM_CONFIGURATIONS).unwrap_err(),
            MetaError::Invalid
        );
        assert_eq!(
            find(&broken(stream_entry + 4, &huge), CAM_TAG_STREAM_CONFIGURATIONS).unwrap_err(),
            MetaError::Invalid
        );
        assert_eq!(find(&broken(12, &huge), CAM_TAG_LENS_FACING).unwrap_err(), MetaError::Invalid);
        assert_eq!(find(&broken(48 + 12, &[9]), CAM_TAG_LENS_FACING).unwrap_err(), MetaError::Invalid);
    }

    fn lens(id: &str, parent: &str, facing: i32, logical: bool, focal: f32, pw: f32, ph: f32) -> CamInfo {
        let mut c = CamInfo::named(id, parent);
        (c.facing, c.logical, c.focal_mm, c.physical_w, c.physical_h) = (facing, logical, focal, pw, ph);
        c.finish();
        c
    }

    #[test]
    fn roles() {
        use Role::*;
        // The S24 Ultra's lenses, a second wide one, a logical camera listed first, an external one.
        let (b, f) = (OC_FACING_BACK, OC_FACING_FRONT);
        let cases = [
            (lens("", "", b, true, 6.3, 9.83, 7.37), Other),
            (lens("", "", b, false, 6.3, 9.83, 7.37), Wide),
            (lens("", "", f, false, 3.3, 4.5, 3.4), Front),
            (lens("", "", b, false, 2.2, 5.9, 4.4), Ultrawide),
            (lens("", "", b, false, 7.9, 4.1, 3.1), Tele3),
            (lens("", "", b, false, 18.6, 5.8, 4.35), Tele5),
            (lens("", "", b, false, 6.3, 9.83, 7.37), Other),
            (lens("", "", OC_FACING_EXTERNAL, false, 4.0, 5.0, 4.0), Other),
        ];
        let mut list: Vec<CamInfo> = cases.iter().map(|c| c.0.clone()).collect();
        assign_roles(&mut list);
        assert_eq!(list.iter().map(|c| c.role).collect::<Vec<_>>(), cases.iter().map(|c| c.1).collect::<Vec<_>>());
        // Only a logical rear camera: it is the wide one.
        let mut list = vec![CamInfo { facing: b, logical: true, ..Default::default() }];
        assign_roles(&mut list);
        assert_eq!(list[0].role, Wide);
        // The phone's list by cameraserver (2026-11-11): logical 0, front 1, ultra-wide 2, a second front 3; then
        // logical 0's physical cameras that are not listed themselves, in any order (a wide one too). The listed
        // logical camera stays the wide one; the telephotos come from its physical cameras.
        let phone = [
            (lens("0", "", b, true, 6.3, 9.83, 7.37), Wide),
            (lens("1", "", f, false, 3.3, 4.5, 3.4), Front),
            (lens("2", "", b, false, 2.2, 5.9, 4.4), Ultrawide),
            (lens("3", "", f, false, 3.3, 3.8, 2.9), Other),
            (lens("58", "0", b, false, 18.6, 5.8, 4.35), Tele5),
            (lens("5", "0", b, false, 6.3, 9.83, 7.37), Other),
            (lens("52", "0", b, false, 7.9, 4.1, 3.1), Tele3),
        ];
        let mut list: Vec<CamInfo> = phone.iter().map(|c| c.0.clone()).collect();
        assign_roles(&mut list);
        assert_eq!(list.iter().map(|c| c.role).collect::<Vec<_>>(), phone.iter().map(|c| c.1).collect::<Vec<_>>());
    }
}
