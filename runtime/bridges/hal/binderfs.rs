// SPDX-License-Identifier: MIT
//! A private binderfs for the binder tests on the build host (the Makefile's `test-binder`): a test binary that
//! finds no binder device in its environment runs itself again in a user namespace (`unshare -Urm`, no root) with
//! a binderfs mounted there, the device's path in the variable it reads. The mount goes with the namespace. A
//! kernel without binderfs (no "binder" in /proc/filesystems) or without unprivileged user namespaces skips, with
//! the reason printed.
//!
//! ```ignore
//! fn main() {
//!     let device = match binderfs::enter("ONEIOS_BINDER", "binder") {
//!         binderfs::Entered::Device(path) => path,
//!         binderfs::Entered::Skipped(why) => { println!("test_x: skipped ({why})"); return; }
//!         binderfs::Entered::Ran(code) => std::process::exit(code),
//!     };
//!     ...
//! }
//! ```

use super::binder::uapi::BINDER_CTL_ADD;
use std::ffi::CString;
use std::path::Path;
use std::process::Command;

pub enum Entered {
    /// The device to use (this process is the one in the namespace, or the variable was set by the caller).
    Device(String),
    /// No binderfs here: why.
    Skipped(String),
    /// The test ran again inside a namespace and ended with this exit code.
    Ran(i32),
}

/// A binderfs of a kernel built with no default devices (CONFIG_ANDROID_BINDER_DEVICES empty: Ubuntu's, colima's
/// VM) has only binder-control: asks it for the device by name. Nothing when the device is there. Err: why not.
pub fn add_device(path: &str) -> Result<(), String> {
    let p = Path::new(path);
    if p.exists() {
        return Ok(());
    }
    let (Some(dir), Some(name)) = (p.parent(), p.file_name()) else { return Ok(()) };
    let control = dir.join("binder-control");
    let c = CString::new(control.as_os_str().as_encoded_bytes()).map_err(|e| e.to_string())?;
    // SAFETY: a NUL-terminated path.
    let fd = unsafe { libc::open(c.as_ptr(), libc::O_RDWR | libc::O_CLOEXEC) };
    if fd < 0 {
        return Ok(()); // no binder-control: the device is simply missing
    }
    // struct binderfs_device { char name[256]; __u32 major, minor; }
    let mut device = [0u8; 264];
    let name = name.as_encoded_bytes();
    let n = name.len().min(255);
    device[..n].copy_from_slice(&name[..n]);
    // SAFETY: BINDER_CTL_ADD reads and writes the 264-byte struct.
    let rc = unsafe { libc::ioctl(fd, BINDER_CTL_ADD as _, device.as_mut_ptr()) };
    let error = std::io::Error::last_os_error();
    // SAFETY: our fd.
    unsafe { libc::close(fd) };
    if rc != 0 {
        return Err(format!(
            "{}: BINDER_CTL_ADD {}: {}",
            control.display(),
            String::from_utf8_lossy(&name[..n]),
            error
        ));
    }
    Ok(())
}

/// The device named by `env`, or this test again in a namespace with a binderfs whose `device` it gets.
pub fn enter(env: &str, device: &str) -> Entered {
    if let Ok(path) = std::env::var(env) {
        if let Err(why) = add_device(&path) {
            println!("{why}");
        }
        let c = CString::new(path.clone()).unwrap_or_default();
        // SAFETY: a NUL-terminated path.
        if unsafe { libc::access(c.as_ptr(), libc::R_OK | libc::W_OK) } != 0 {
            return Entered::Skipped(format!("{env}={path} is not a binder device this test can open"));
        }
        return Entered::Device(path);
    }
    let filesystems = std::fs::read_to_string("/proc/filesystems").unwrap_or_default();
    if !filesystems.lines().any(|l| l.split_whitespace().any(|w| w == "binder")) {
        return Entered::Skipped("no binderfs in this kernel".to_owned());
    }
    let Ok(me) = std::env::current_exe() else { return Entered::Skipped("no path of this test".to_owned()) };
    if !Command::new("unshare").args(["-Urm", "true"]).status().is_ok_and(|s| s.success()) {
        return Entered::Skipped("no user namespace for this user (unshare -Urm)".to_owned());
    }
    let dir = std::env::temp_dir().join(format!("onebridges-binderfs-{}", std::process::id()));
    if let Err(e) = std::fs::create_dir_all(&dir) {
        return Entered::Skipped(format!("{}: {e}", dir.display()));
    }
    let script = format!("mount -t binder binder \"$1\" && {env}=\"$1/{device}\" exec \"$2\"");
    let status = Command::new("unshare").args(["-Urm", "sh", "-c", &script, "sh"]).arg(&dir).arg(&me).status();
    let _ = std::fs::remove_dir(&dir);
    match status {
        Ok(s) => Entered::Ran(s.code().unwrap_or(1)),
        Err(e) => Entered::Skipped(format!("unshare: {e}")),
    }
}
