// SPDX-License-Identifier: MIT
//! Port of launcher.c. oneios-display: starts the display helper. The layer needs the framework's SurfaceControl
//! classes, so the helper is a dex run by app_process (no zygote, no system_server); this finds the dex and the native
//! library next to itself and execs app_process, keeping its pid.
//!   oneios-display --session DIR [--takeover] [--wait SECONDS] ...   (see README.md)
use oneios_display::sys::{self, Errno};
use std::ffi::CString;
use std::os::unix::ffi::OsStringExt;
use std::process::ExitCode;

fn cstring(s: impl Into<Vec<u8>>) -> CString {
    CString::new(s).unwrap_or_default()
}

fn main() -> ExitCode {
    let me = match sys::self_exe() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("oneios-display: /proc/self/exe: {}", Errno::from(e));
            return ExitCode::from(2);
        }
    };
    let dir = me.parent().map(|d| d.display().to_string()).unwrap_or_else(|| ".".into());
    let mut dex = format!("{dir}/../lib/oneios-display.dex"); // the runtime package's layout
    if sys::access(dex.as_ref(), libc::R_OK).is_err() {
        dex = format!("{dir}/oneios-display.dex"); // one folder (a test push)
    }
    let lib = format!("{dir}/liboneios-display.so");
    if sys::access(dex.as_ref(), libc::R_OK).is_err() || sys::access(lib.as_ref(), libc::R_OK).is_err() {
        eprintln!("oneios-display: {dex} or {lib} is missing");
        return ExitCode::from(2);
    }
    let process = std::env::var_os("ONEIOS_APP_PROCESS").unwrap_or_else(|| "/system/bin/app_process".into());
    std::env::set_var("CLASSPATH", &dex);
    let mut args: Vec<CString> =
        ["app_process", "/system/bin", "OneiOSDisplay", "--lib", lib.as_str()].into_iter().map(cstring).collect();
    args.extend(std::env::args_os().skip(1).map(|a| cstring(a.into_vec())));
    let mut argv: Vec<*const libc::c_char> = args.iter().map(|a| a.as_ptr()).collect();
    argv.push(std::ptr::null());
    let path = cstring(process.clone().into_vec());
    // SAFETY: path and argv are NUL-terminated strings and a NULL-terminated array that live across the call; execv
    // returns only on failure.
    unsafe { libc::execv(path.as_ptr(), argv.as_ptr()) };
    eprintln!("oneios-display: {}: {}", process.to_string_lossy(), Errno::last());
    ExitCode::from(2)
}
