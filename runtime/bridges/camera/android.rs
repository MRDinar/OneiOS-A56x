// SPDX-License-Identifier: MIT
//! What both Android back ends need: platform symbols of libbinder_ndk and libnativewindow that the NDK's link
//! libraries leave out (found with dlsym, as the C did), and this process's binder threads.
use std::ffi::{c_void, CStr};
use std::sync::OnceLock;

/// A symbol of the libraries this process has loaded, None when this Android's library lacks it.
pub fn platform_symbol(name: &CStr) -> Option<*mut c_void> {
    // SAFETY: dlsym of a C string in the global namespace.
    let p = unsafe { libc::dlsym(libc::RTLD_DEFAULT, name.as_ptr()) };
    (!p.is_null()).then_some(p)
}

/// Starts this process's binder threads once, 4 at most (`cam_binder_threads`): a plain executable has none, and both
/// back ends are called back over binder (backend_ndk.rs's header). Ordinary threads: no realtime class, no nice
/// value. ENOSYS when libbinder_ndk lacks the platform symbols.
pub fn binder_threads() -> Result<(), i32> {
    static STARTED: OnceLock<Result<(), i32>> = OnceLock::new();
    *STARTED.get_or_init(|| {
        let start = platform_symbol(c"ABinderProcess_startThreadPool").ok_or(libc::ENOSYS)?;
        let max = platform_symbol(c"ABinderProcess_setThreadPoolMaxThreadCount").ok_or(libc::ENOSYS)?;
        // SAFETY: the two platform functions of libbinder_ndk, by their C signatures.
        unsafe {
            let max: extern "C" fn(u32) -> bool = std::mem::transmute(max);
            let start: extern "C" fn() = std::mem::transmute(start);
            max(4);
            start();
        }
        Ok(())
    })
}
