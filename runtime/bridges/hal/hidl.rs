// SPDX-License-Identifier: MIT
//! The wire format of a HIDL transaction on /dev/hwbinder, as far as the Bluetooth HAL needs it: an interface token,
//! hidl_string and hidl_vec<uint8_t> arguments (scatter-gather buffer objects), strong binders. Written from the
//! kernel's binder UAPI and libhwbinder's Parcel layout; no Android library. The transport is [`super::binder`].
//! C: the generic half of bt/hidl_parcel.h, bt/hidl_parcel.c (the Bluetooth HAL's parcels: `bt::hidl_parcel`).

use super::binder::{self, Outgoing};
use std::ffi::CStr;
use std::marker::PhantomData;
use xh_core::bytes::ne;

pub const TYPE_BINDER: u32 = binder::uapi::TYPE_BINDER;
pub const TYPE_HANDLE: u32 = binder::uapi::TYPE_HANDLE;
pub const TYPE_PTR: u32 = binder::uapi::TYPE_PTR;
pub const BUFFER_HAS_PARENT: u32 = binder::uapi::BUFFER_FLAG_HAS_PARENT;

pub const PARCEL_DATA: usize = 8192;
pub const PARCEL_OBJECTS: usize = 16;
pub const PARCEL_SCRATCH: usize = 1024;
/// hidl_string and hidl_vec<T> have the same 16-byte shape: { u64 pointer; u32 size; u8 owns; u8 pad[3] }.
pub const HIDL_REF: usize = 16;
pub const FLAT_OBJECT: usize = 24;
pub const BUFFER_OBJECT: usize = 40;

// HIDL transaction codes.
pub const FIRST_CALL: u32 = 1;
pub const INTERFACE_CHAIN: u32 = 0x0f43_484e;
pub const DEBUG: u32 = 0x0f44_4247;
pub const INTERFACE_DESCRIPTOR: u32 = 0x0f44_5343;
pub const GET_HASH_CHAIN: u32 = 0x0f48_5348;
pub const SET_HAL_INSTRUMENTATION: u32 = 0x0f49_4e54;
pub const PING: u32 = 0x0f50_4e47;
pub const GET_DEBUG_INFO: u32 = 0x0f52_4546;
pub const NOTIFY_SYSPROPS_CHANGED: u32 = 0x0f53_5953;
/// -EBADMSG, android::UNKNOWN_TRANSACTION
pub const UNKNOWN_TRANSACTION: i32 = binder::UNKNOWN_TRANSACTION;

/// The kernel's binder_buffer_object (40 bytes).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BufferObject {
    pub kind: u32,
    pub flags: u32,
    pub buffer: u64,
    pub length: u64,
    pub parent: u64,
    pub parent_offset: u64,
}

impl BufferObject {
    pub fn to_bytes(self) -> [u8; BUFFER_OBJECT] {
        let mut b = [0u8; BUFFER_OBJECT];
        b[0..4].copy_from_slice(&self.kind.to_ne_bytes());
        b[4..8].copy_from_slice(&self.flags.to_ne_bytes());
        b[8..16].copy_from_slice(&self.buffer.to_ne_bytes());
        b[16..24].copy_from_slice(&self.length.to_ne_bytes());
        b[24..32].copy_from_slice(&self.parent.to_ne_bytes());
        b[32..40].copy_from_slice(&self.parent_offset.to_ne_bytes());
        b
    }
    pub fn parse(b: &[u8]) -> BufferObject {
        BufferObject {
            kind: ne::u32(b, 0),
            flags: ne::u32(b, 4),
            buffer: ne::u64(b, 8),
            length: ne::u64(b, 16),
            parent: ne::u64(b, 24),
            parent_offset: ne::u64(b, 32),
        }
    }
}

/// An outgoing parcel. The buffers given to the put_* calls are borrowed for 'a: the kernel copies them when the
/// transaction is written. The refs of strings and vectors live in the parcel's own scratch (on the heap: they keep
/// their address when the parcel moves).
pub struct HidlParcel<'a> {
    data: Vec<u8>,
    offsets: Vec<u64>,
    buffers_size: u64, // the buffer objects' lengths, each rounded up to 8
    scratch: Box<[u64; PARCEL_SCRATCH / 8]>,
    scratch_used: usize, // in u64 words
    overflow: bool,
    _borrows: PhantomData<&'a [u8]>,
}

impl Default for HidlParcel<'_> {
    fn default() -> Self {
        HidlParcel {
            data: Vec::with_capacity(256),
            offsets: Vec::new(),
            buffers_size: 0,
            scratch: Box::new([0; PARCEL_SCRATCH / 8]),
            scratch_used: 0,
            overflow: false,
            _borrows: PhantomData,
        }
    }
}

impl<'a> HidlParcel<'a> {
    pub fn new() -> HidlParcel<'a> {
        HidlParcel::default()
    }
    /// Empties it for the next parcel (its memory is kept).
    pub fn clear(&mut self) {
        self.data.clear();
        self.offsets.clear();
        self.buffers_size = 0;
        self.scratch_used = 0;
        self.overflow = false;
    }
    /// The same memory, emptied, for parcels that borrow for another lifetime (a caller keeps one parcel for every
    /// packet it sends).
    pub fn recycle<'b>(mut self) -> HidlParcel<'b> {
        self.clear();
        HidlParcel {
            data: std::mem::take(&mut self.data),
            offsets: std::mem::take(&mut self.offsets),
            buffers_size: 0,
            scratch: std::mem::replace(&mut self.scratch, Box::new([0; PARCEL_SCRATCH / 8])),
            scratch_used: 0,
            overflow: false,
            _borrows: PhantomData,
        }
    }
    pub fn size(&self) -> usize {
        self.data.len()
    }
    pub fn bytes(&self) -> &[u8] {
        &self.data
    }
    pub fn object_offsets(&self) -> &[u64] {
        &self.offsets
    }
    pub fn buffers(&self) -> u64 {
        self.buffers_size
    }
    pub fn is_overflow(&self) -> bool {
        self.overflow
    }
    /// The object at `index` as a buffer object (the tests look at what was written).
    pub fn buffer_object(&self, index: usize) -> BufferObject {
        let at = self.offsets[index] as usize;
        BufferObject::parse(&self.data[at..at + BUFFER_OBJECT])
    }
    /// A reader over this parcel (the client's tests read their own send parcels).
    pub fn reader(&self) -> HidlReader<'_> {
        self.reader_prefix(self.data.len())
    }
    /// A reader over the first `size` bytes (a truncated parcel).
    pub fn reader_prefix(&self, size: usize) -> HidlReader<'_> {
        // SAFETY: every buffer pointer in this parcel is one of its own scratch's or a buffer borrowed for 'a,
        // which outlives the reader's borrow of self.
        unsafe { HidlReader::new(&self.data[..size.min(self.data.len())], binder::offsets_bytes(&self.offsets)) }
    }

    fn reserve(&mut self, bytes: usize) -> Option<usize> {
        let padded = (bytes + 3) & !3;
        if self.overflow || padded > PARCEL_DATA - self.data.len() {
            self.overflow = true;
            return None;
        }
        let at = self.data.len();
        self.data.resize(at + padded, 0);
        Some(at)
    }

    /// `words` zeroed u64s of scratch: their address and index, None (and overflow) when full.
    fn scratch(&mut self, words: usize) -> Option<(u64, usize)> {
        if self.overflow || words > self.scratch.len() - self.scratch_used {
            self.overflow = true;
            return None;
        }
        let at = self.scratch_used;
        self.scratch[at..at + words].fill(0);
        self.scratch_used += words;
        Some((self.scratch[at..].as_ptr() as u64, at))
    }

    /// A hidl_ref { pointer, size } in scratch word `at`.
    fn set_ref(&mut self, at: usize, pointer: u64, size: u32) {
        self.scratch[at] = pointer;
        let mut second = [0u8; 8];
        second[..4].copy_from_slice(&size.to_ne_bytes());
        self.scratch[at + 1] = u64::from_ne_bytes(second); // owns 0, padding 0
    }

    /// writeInterfaceToken: the name and its NUL, padded to 4.
    pub fn put_token(&mut self, interface: &CStr) {
        let n = interface.to_bytes_with_nul();
        if let Some(at) = self.reserve(n.len()) {
            self.data[at..at + n.len()].copy_from_slice(n);
        }
    }

    pub fn put_i32(&mut self, value: i32) {
        if let Some(at) = self.reserve(4) {
            self.data[at..at + 4].copy_from_slice(&value.to_ne_bytes());
        }
    }

    fn put_object(&mut self, object: &[u8]) -> usize {
        if self.offsets.len() == PARCEL_OBJECTS {
            self.overflow = true;
            return 0;
        }
        let Some(at) = self.reserve(object.len()) else { return 0 };
        self.data[at..at + object.len()].copy_from_slice(object);
        self.offsets.push(at as u64);
        self.offsets.len() - 1
    }

    fn put_buffer_raw(&mut self, buffer: u64, length: usize, parent: Option<(usize, usize)>) -> usize {
        let o = BufferObject {
            kind: TYPE_PTR,
            flags: if parent.is_some() { BUFFER_HAS_PARENT } else { 0 },
            buffer,
            length: length as u64,
            parent: parent.map_or(0, |p| p.0 as u64),
            parent_offset: parent.map_or(0, |p| p.1 as u64),
        };
        self.buffers_size += (length as u64 + 7) & !7;
        self.put_object(&o.to_bytes())
    }

    /// A top-level buffer: its index among the parcel's objects.
    pub fn put_buffer(&mut self, buffer: &'a [u8]) -> usize {
        self.put_buffer_raw(buffer.as_ptr() as u64, buffer.len(), None)
    }
    /// A buffer embedded in `parent` at `parent_offset`.
    pub fn put_embedded(&mut self, buffer: &'a [u8], parent: usize, parent_offset: usize) -> usize {
        self.put_buffer_raw(buffer.as_ptr() as u64, buffer.len(), Some((parent, parent_offset)))
    }

    /// A hidl_ref in scratch and its pointee: the ref as a top-level buffer, the pointee embedded in it.
    fn put_ref(&mut self, pointer: u64, size: u32, length: usize) {
        let Some((at, word)) = self.scratch(2) else { return };
        self.set_ref(word, pointer, size);
        let parent = self.put_buffer_raw(at, HIDL_REF, None);
        self.put_buffer_raw(pointer, length, Some((parent, 0)));
    }

    /// hidl_string: the text and its NUL.
    pub fn put_string(&mut self, text: &'a CStr) {
        let n = text.to_bytes().len();
        self.put_ref(text.as_ptr() as u64, n as u32, n + 1);
    }
    /// hidl_vec<uint8_t>
    pub fn put_bytes(&mut self, bytes: &'a [u8]) {
        self.put_ref(bytes.as_ptr() as u64, bytes.len() as u32, bytes.len());
    }
    /// hidl_vec<hidl_string>: the vector's ref, its array of refs embedded in it, each string embedded in the array.
    pub fn put_string_vec(&mut self, texts: &[&'a CStr]) {
        let Some((vec_at, vec_word)) = self.scratch(2) else { return };
        let Some((items_at, items_word)) = self.scratch(2 * texts.len()) else { return };
        self.set_ref(vec_word, items_at, texts.len() as u32);
        let parent = self.put_buffer_raw(vec_at, HIDL_REF, None);
        let array = self.put_buffer_raw(items_at, texts.len() * HIDL_REF, Some((parent, 0)));
        for (i, text) in texts.iter().enumerate() {
            let n = text.to_bytes().len();
            self.set_ref(items_word + 2 * i, text.as_ptr() as u64, n as u32);
            self.put_buffer_raw(text.as_ptr() as u64, n + 1, Some((array, i * HIDL_REF)));
        }
    }
    /// An empty hidl_vec<T>: its ref and a null embedded buffer (getHashChain's answer).
    pub fn put_empty_vec(&mut self) {
        self.put_ref(0, 0, 0);
    }
    /// A struct of `words` u64s in scratch as a top-level buffer (DebugInfo).
    pub fn put_struct(&mut self, words: &[u64]) {
        let Some((at, word)) = self.scratch(words.len()) else { return };
        self.scratch[word..word + words.len()].copy_from_slice(words);
        self.put_buffer_raw(at, words.len() * 8, None);
    }
    /// A local object (0x7f: the lowest priority the object accepts; 0x100: FLAT_BINDER_FLAG_ACCEPTS_FDS).
    pub fn put_binder(&mut self, pointer: u64, cookie: u64) {
        let o = super::aidl::FlatObject { kind: TYPE_BINDER, flags: 0x17f, binder_or_handle: pointer, cookie };
        self.put_object(&o.to_bytes());
    }
}

impl Outgoing for HidlParcel<'_> {
    fn data(&self) -> &[u8] {
        &self.data
    }
    fn offsets(&self) -> &[u64] {
        &self.offsets
    }
    fn buffers_size(&self) -> Option<u64> {
        Some(self.buffers_size)
    }
    fn overflow(&self) -> bool {
        self.overflow
    }
}

/// An incoming parcel: data and the offsets of its objects, as the kernel hands them over (buffer pointers already
/// point into this process). A malformed one sets `bad`.
pub struct HidlReader<'a> {
    data: &'a [u8],
    offsets: &'a [u8],
    position: usize,
    next_object: usize,
    bad: bool,
}

impl<'a> HidlReader<'a> {
    /// # Safety
    /// Every buffer object's pointer in `data` (and the hidl_refs they point at) must be readable for its length
    /// for 'a: true of a transaction the driver delivered (until its buffer is freed) and of our own parcels.
    pub unsafe fn new(data: &'a [u8], offsets: &'a [u8]) -> HidlReader<'a> {
        HidlReader { data, offsets, position: 0, next_object: 0, bad: false }
    }
    pub fn is_bad(&self) -> bool {
        self.bad
    }

    /// The interface token; None (and bad) when it is not a terminated string.
    pub fn token(&mut self) -> Option<&'a CStr> {
        if self.bad || self.position >= self.data.len() {
            self.bad = true;
            return None;
        }
        match CStr::from_bytes_until_nul(&self.data[self.position..]) {
            Ok(text) => {
                self.position += (text.to_bytes().len() + 1 + 3) & !3;
                Some(text)
            }
            Err(_) => {
                self.bad = true;
                None
            }
        }
    }

    pub fn i32(&mut self) -> i32 {
        if self.bad || self.data.len() < 4 || self.position > self.data.len() - 4 {
            self.bad = true;
            return 0;
        }
        let v = ne::u32(self.data, self.position) as i32;
        self.position += 4;
        v
    }

    /// The object at the read position, which must be the next in the offsets list.
    fn next_object(&mut self, size: usize) -> Option<&'a [u8]> {
        let offset = if self.bad { None } else { binder::offset_at(self.offsets, self.next_object) };
        match offset {
            Some(o) if o == self.position as u64 && size <= self.data.len() && o as usize <= self.data.len() - size => {
                self.next_object += 1;
                self.position += size;
                Some(&self.data[o as usize..o as usize + size])
            }
            _ => {
                self.bad = true;
                None
            }
        }
    }

    /// The next hidl_vec<uint8_t>: its bytes. None (and bad) on a malformed parcel.
    pub fn bytes(&mut self) -> Option<&'a [u8]> {
        let parent_index = self.next_object as u64;
        let top = BufferObject::parse(self.next_object(BUFFER_OBJECT)?);
        let child = BufferObject::parse(self.next_object(BUFFER_OBJECT)?);
        if top.kind != TYPE_PTR
            || child.kind != TYPE_PTR
            || top.length != HIDL_REF as u64
            || top.buffer == 0
            || child.flags & BUFFER_HAS_PARENT == 0
            || child.parent != parent_index
            || child.parent_offset != 0
        {
            self.bad = true;
            return None;
        }
        // SAFETY: top.buffer is a buffer of HIDL_REF bytes the parcel carries (HidlReader::new's contract).
        let r = unsafe { std::slice::from_raw_parts(top.buffer as *const u8, HIDL_REF) };
        let (pointer, size) = (ne::u64(r, 0), ne::u32(r, 8));
        if size as u64 != child.length || (size != 0 && pointer != child.buffer) {
            self.bad = true;
            return None;
        }
        if size == 0 {
            return Some(&[]);
        }
        // SAFETY: child.buffer is a buffer of child.length bytes the parcel carries (HidlReader::new's contract).
        Some(unsafe { std::slice::from_raw_parts(child.buffer as *const u8, size as usize) })
    }

    /// The next strong binder: a handle, or None for a null or local object (bad when there is none).
    pub fn handle(&mut self) -> Option<u32> {
        let o = super::aidl::FlatObject::parse(self.next_object(FLAT_OBJECT)?);
        (o.kind == TYPE_HANDLE).then_some(o.binder_or_handle as u32)
    }
}
