// SPDX-License-Identifier: MIT
//! oneios-audio (the bin; C: audio/aaudio_helper.c's main): the AAudio helper on the NDK's libaaudio. A dynamic
//! executable (AAudio is a shared library; xnuhost is static and starts this as `oneios-audio FD`). The helper's logic
//! is onebridges::audio::aaudio_helper; this file is only the NDK's AAudio behind its traits.
//!
//! AAudio_setMMapPolicy and AAudioStream_isMMapUsed (AAudioTesting.h, exported by libaaudio: Android 17 on the phone)
//! and libaudioclient's AudioSystem volume calls are looked up at run time: the NDK's stub libraries do not list them.

#[cfg(target_os = "android")]
mod ndk {
    use onebridges::audio::aaudio_helper::{AAudio, AStream, StreamRequest, VolumeCalls};
    use std::ffi::{c_char, c_void, CStr};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, OnceLock};

    #[repr(C)]
    struct AAudioStreamBuilder(c_void);
    #[repr(C)]
    struct AAudioStream(c_void);

    const DIRECTION_OUTPUT: i32 = 0;
    const DIRECTION_INPUT: i32 = 1;
    const FORMAT_PCM_I16: i32 = 1;
    const USAGE_MEDIA: i32 = 1;
    const CONTENT_TYPE_MUSIC: i32 = 2;
    const INPUT_PRESET_VOICE_RECOGNITION: i32 = 6;

    #[link(name = "aaudio")]
    extern "C" {
        fn AAudio_createStreamBuilder(builder: *mut *mut AAudioStreamBuilder) -> i32;
        fn AAudio_convertResultToText(result: i32) -> *const c_char;
        fn AAudioStreamBuilder_setDirection(b: *mut AAudioStreamBuilder, v: i32);
        fn AAudioStreamBuilder_setSampleRate(b: *mut AAudioStreamBuilder, v: i32);
        fn AAudioStreamBuilder_setChannelCount(b: *mut AAudioStreamBuilder, v: i32);
        fn AAudioStreamBuilder_setFormat(b: *mut AAudioStreamBuilder, v: i32);
        fn AAudioStreamBuilder_setPerformanceMode(b: *mut AAudioStreamBuilder, v: i32);
        fn AAudioStreamBuilder_setSharingMode(b: *mut AAudioStreamBuilder, v: i32);
        fn AAudioStreamBuilder_setUsage(b: *mut AAudioStreamBuilder, v: i32);
        fn AAudioStreamBuilder_setContentType(b: *mut AAudioStreamBuilder, v: i32);
        fn AAudioStreamBuilder_setInputPreset(b: *mut AAudioStreamBuilder, v: i32);
        fn AAudioStreamBuilder_openStream(b: *mut AAudioStreamBuilder, stream: *mut *mut AAudioStream) -> i32;
        fn AAudioStreamBuilder_delete(b: *mut AAudioStreamBuilder) -> i32;
        fn AAudioStream_requestStart(s: *mut AAudioStream) -> i32;
        fn AAudioStream_requestStop(s: *mut AAudioStream) -> i32;
        fn AAudioStream_close(s: *mut AAudioStream) -> i32;
        fn AAudioStream_write(s: *mut AAudioStream, buffer: *const c_void, frames: i32, timeout_ns: i64) -> i32;
        fn AAudioStream_read(s: *mut AAudioStream, buffer: *mut c_void, frames: i32, timeout_ns: i64) -> i32;
        fn AAudioStream_setBufferSizeInFrames(s: *mut AAudioStream, frames: i32) -> i32;
        fn AAudioStream_getTimestamp(
            s: *mut AAudioStream,
            clock: libc::clockid_t,
            position: *mut i64,
            time: *mut i64,
        ) -> i32;
        fn AAudioStream_getFramesPerBurst(s: *mut AAudioStream) -> i32;
        fn AAudioStream_getBufferSizeInFrames(s: *mut AAudioStream) -> i32;
        fn AAudioStream_getBufferCapacityInFrames(s: *mut AAudioStream) -> i32;
        fn AAudioStream_getSampleRate(s: *mut AAudioStream) -> i32;
        fn AAudioStream_getChannelCount(s: *mut AAudioStream) -> i32;
        fn AAudioStream_getDeviceId(s: *mut AAudioStream) -> i32;
        fn AAudioStream_getSessionId(s: *mut AAudioStream) -> i32;
        fn AAudioStream_getXRunCount(s: *mut AAudioStream) -> i32;
        fn AAudioStream_getSharingMode(s: *mut AAudioStream) -> i32;
        fn AAudioStream_getPerformanceMode(s: *mut AAudioStream) -> i32;
        fn AAudioStream_getFramesWritten(s: *mut AAudioStream) -> i64;
        fn AAudioStream_getFramesRead(s: *mut AAudioStream) -> i64;
    }

    /// A symbol of `lib` as already loaded (RTLD_NOLOAD: a second copy of libaudioclient would open the binder driver
    /// again), else of the process.
    fn symbol(lib: &CStr, name: &CStr) -> *mut c_void {
        // SAFETY: dlopen/dlsym with NUL-terminated names; RTLD_NOLOAD loads nothing.
        unsafe {
            let handle = libc::dlopen(lib.as_ptr(), libc::RTLD_NOW | libc::RTLD_NOLOAD);
            let f = if handle.is_null() { std::ptr::null_mut() } else { libc::dlsym(handle, name.as_ptr()) };
            if f.is_null() {
                libc::dlsym(libc::RTLD_DEFAULT, name.as_ptr())
            } else {
                f
            }
        }
    }

    type SetPolicy = unsafe extern "C" fn(i32) -> i32;
    type MmapUsed = unsafe extern "C" fn(*mut AAudioStream) -> bool;

    fn set_policy() -> Option<SetPolicy> {
        static F: OnceLock<usize> = OnceLock::new();
        let p = *F.get_or_init(|| symbol(c"libaaudio.so", c"AAudio_setMMapPolicy") as usize);
        // SAFETY: AAudio_setMMapPolicy's signature (AAudioTesting.h).
        (p != 0).then(|| unsafe { std::mem::transmute::<usize, SetPolicy>(p) })
    }

    fn mmap_used_fn() -> Option<MmapUsed> {
        static F: OnceLock<usize> = OnceLock::new();
        let p = *F.get_or_init(|| symbol(c"libaaudio.so", c"AAudioStream_isMMapUsed") as usize);
        // SAFETY: AAudioStream_isMMapUsed's signature (AAudioTesting.h).
        (p != 0).then(|| unsafe { std::mem::transmute::<usize, MmapUsed>(p) })
    }

    // ---- libaudioclient's AudioSystem (fix-audio-volume) -------------------------------------------------------
    /// system/audio.h's audio_attributes_t.
    #[repr(C, packed)]
    struct Attributes {
        content_type: u32,
        usage: u32,
        source: u32,
        flags: u32,
        tags: [c_char; 256],
    }
    const DEVICE_OUT_SPEAKER: u32 = 0x2;
    const MEDIA: Attributes = Attributes { content_type: 2, usage: 1, source: 0, flags: 0, tags: [0; 256] };
    type GetIndex = unsafe extern "C" fn(*const Attributes, *mut i32, u32) -> i32;
    type GetMax = unsafe extern "C" fn(*const Attributes, *mut i32) -> i32;
    type SetIndex = unsafe extern "C" fn(*const Attributes, i32, u32) -> i32;
    type SetIndexMuted = unsafe extern "C" fn(*const Attributes, i32, bool, u32) -> i32;

    #[derive(Clone, Copy)]
    struct Volume {
        get: usize,
        max: usize,
        set: usize,
        set_muted: usize,
    }

    fn volume() -> Volume {
        static V: OnceLock<Volume> = OnceLock::new();
        *V.get_or_init(|| {
            let f = |name: &CStr| symbol(c"libaudioclient.so", name) as usize;
            Volume {
                get: f(
                    c"_ZN7android11AudioSystem27getVolumeIndexForAttributesERK18audio_attributes_tRi15audio_devices_t",
                ),
                max: f(c"_ZN7android11AudioSystem30getMaxVolumeIndexForAttributesERK18audio_attributes_tRi"),
                set: f(
                    c"_ZN7android11AudioSystem27setVolumeIndexForAttributesERK18audio_attributes_ti15audio_devices_t",
                ),
                set_muted: f(
                    c"_ZN7android11AudioSystem27setVolumeIndexForAttributesERK18audio_attributes_tib15audio_devices_t",
                ),
            }
        })
    }

    pub struct Ndk;

    struct Stream {
        s: *mut AAudioStream,
        closed: AtomicBool,
    }

    // SAFETY: AAudio's stream calls may be made from any thread (the helper's writer and its receive loop both use it,
    // as the C helper did); the pointer is closed once, after the threads that use it are joined.
    unsafe impl Send for Stream {}
    // SAFETY: as above.
    unsafe impl Sync for Stream {}

    impl Drop for Stream {
        fn drop(&mut self) {
            self.close();
        }
    }

    macro_rules! get {
        ($($name:ident => $f:ident -> $t:ty),* $(,)?) => {$(
            fn $name(&self) -> $t {
                // SAFETY: an open stream (closed only by close, after its users stopped).
                unsafe { $f(self.s) }
            }
        )*};
    }

    impl AStream for Stream {
        get! {
            request_start => AAudioStream_requestStart -> i32,
            request_stop => AAudioStream_requestStop -> i32,
            frames_per_burst => AAudioStream_getFramesPerBurst -> i32,
            buffer_size => AAudioStream_getBufferSizeInFrames -> i32,
            buffer_capacity => AAudioStream_getBufferCapacityInFrames -> i32,
            sample_rate => AAudioStream_getSampleRate -> i32,
            channel_count => AAudioStream_getChannelCount -> i32,
            device_id => AAudioStream_getDeviceId -> i32,
            session_id => AAudioStream_getSessionId -> i32,
            xrun_count => AAudioStream_getXRunCount -> i32,
            sharing_mode => AAudioStream_getSharingMode -> i32,
            performance_mode => AAudioStream_getPerformanceMode -> i32,
            frames_written => AAudioStream_getFramesWritten -> i64,
            frames_read => AAudioStream_getFramesRead -> i64,
        }
        fn close(&self) {
            if !self.closed.swap(true, Ordering::AcqRel) {
                // SAFETY: the stream, closed once.
                unsafe { AAudioStream_close(self.s) };
            }
        }
        fn write(&self, pcm: &[i16], frames: i32, timeout_ns: i64) -> i32 {
            let frames = frames.min((pcm.len() / self.channel_count().max(1) as usize) as i32);
            // SAFETY: `pcm` holds `frames` frames of the stream's channels.
            unsafe { AAudioStream_write(self.s, pcm.as_ptr().cast(), frames, timeout_ns) }
        }
        fn read(&self, pcm: &mut [i16], frames: i32, timeout_ns: i64) -> i32 {
            let frames = frames.min((pcm.len() / self.channel_count().max(1) as usize) as i32);
            // SAFETY: `pcm` has room for `frames` frames of the stream's channels.
            unsafe { AAudioStream_read(self.s, pcm.as_mut_ptr().cast(), frames, timeout_ns) }
        }
        fn set_buffer_size(&self, frames: i32) -> i32 {
            // SAFETY: an open stream.
            unsafe { AAudioStream_setBufferSizeInFrames(self.s, frames) }
        }
        fn timestamp(&self) -> Option<(i64, i64)> {
            let (mut position, mut time) = (0i64, 0i64);
            // SAFETY: an open stream and two valid out pointers.
            let rc = unsafe { AAudioStream_getTimestamp(self.s, libc::CLOCK_MONOTONIC, &mut position, &mut time) };
            (rc == 0).then_some((position, time))
        }
        fn mmap_used(&self) -> i32 {
            // SAFETY: AAudioStream_isMMapUsed on an open stream.
            mmap_used_fn().map_or(-1, |f| i32::from(unsafe { f(self.s) }))
        }
    }

    impl AAudio for Ndk {
        fn open(&self, r: &StreamRequest) -> Result<Arc<dyn AStream>, i32> {
            let mut b = std::ptr::null_mut();
            // SAFETY: a builder made, set and deleted here; the stream pointer is ours once opened.
            unsafe {
                let rc = AAudio_createStreamBuilder(&mut b);
                if rc != 0 {
                    return Err(rc);
                }
                AAudioStreamBuilder_setDirection(b, if r.output { DIRECTION_OUTPUT } else { DIRECTION_INPUT });
                AAudioStreamBuilder_setSampleRate(b, r.rate as i32);
                AAudioStreamBuilder_setChannelCount(b, r.channels as i32);
                AAudioStreamBuilder_setFormat(b, FORMAT_PCM_I16);
                AAudioStreamBuilder_setPerformanceMode(b, r.perf);
                AAudioStreamBuilder_setSharingMode(b, r.sharing);
                if r.output {
                    AAudioStreamBuilder_setUsage(b, USAGE_MEDIA);
                    AAudioStreamBuilder_setContentType(b, CONTENT_TYPE_MUSIC);
                } else {
                    AAudioStreamBuilder_setInputPreset(b, INPUT_PRESET_VOICE_RECOGNITION);
                }
                let mut s = std::ptr::null_mut();
                let rc = AAudioStreamBuilder_openStream(b, &mut s);
                AAudioStreamBuilder_delete(b);
                if rc != 0 {
                    return Err(rc);
                }
                Ok(Arc::new(Stream { s, closed: AtomicBool::new(false) }))
            }
        }
        fn result_text(&self, rc: i32) -> String {
            // SAFETY: AAudio returns a static NUL-terminated text.
            let p = unsafe { AAudio_convertResultToText(rc) };
            if p.is_null() {
                return format!("{rc}");
            }
            // SAFETY: as above.
            unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
        }
        fn set_mmap_policy(&self, policy: i32) -> Option<i32> {
            // SAFETY: AAudio_setMMapPolicy with a policy value.
            set_policy().map(|f| unsafe { f(policy) })
        }
        fn volume_calls(&self) -> VolumeCalls {
            let v = volume();
            VolumeCalls { get: v.get != 0, max: v.max != 0, set: v.set != 0, set_muted: v.set_muted != 0 }
        }
        fn volume_index(&self) -> (i32, i32) {
            let mut index = -1;
            // SAFETY: the function found under its mangled name (the signature it names), valid pointers.
            let rc =
                unsafe { std::mem::transmute::<usize, GetIndex>(volume().get)(&MEDIA, &mut index, DEVICE_OUT_SPEAKER) };
            (rc, index)
        }
        fn volume_max(&self) -> (i32, i32) {
            let mut max = -1;
            // SAFETY: as above.
            let rc = unsafe { std::mem::transmute::<usize, GetMax>(volume().max)(&MEDIA, &mut max) };
            (rc, max)
        }
        fn set_volume_index(&self, index: i32) -> i32 {
            let v = volume();
            // SAFETY: as above; the `muted` form (Android 16 and later) when there is one.
            unsafe {
                if v.set_muted != 0 {
                    std::mem::transmute::<usize, SetIndexMuted>(v.set_muted)(&MEDIA, index, false, DEVICE_OUT_SPEAKER)
                } else {
                    std::mem::transmute::<usize, SetIndex>(v.set)(&MEDIA, index, DEVICE_OUT_SPEAKER)
                }
            }
        }
    }
}

#[cfg(target_os = "android")]
fn main() {
    let args: Vec<String> = std::env::args().collect();
    std::process::exit(onebridges::audio::aaudio_helper::main_with(&args, std::sync::Arc::new(ndk::Ndk)));
}

#[cfg(not(target_os = "android"))]
fn main() {
    eprintln!("oneios-audio: AAudio is Android's: this build has none (the helper's logic is tested with a fake)");
    std::process::exit(1);
}
