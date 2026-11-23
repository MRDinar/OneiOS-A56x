// SPDX-License-Identifier: MIT
//! Port of usb.c: the phone's USB gadget in xnuhost's log (docs/booter.md 2.9a): a line at the start and on every
//! change,
//!   usb: state <sys.usb.state>, config <sys.usb.config>, UDC <sys.usb.controller> <its state>
//! so that a session's log says when adb's USB function, or the controller under it, went away (phone, 2026-11-13: a
//! cable plugged in during an iOS session, and the Mac saw no USB device at all). Read only: three properties of
//! Android's property area, read as bionic does (__system_property_find once a property exists, its serial to see
//! that it changed, __system_property_read_callback for its value), and one named sysfs file,
//! /sys/class/udc/<sys.usb.controller>/state. /sys/class/udc is never listed: the controller is found by its name.
//! Portable: the property source is a parameter (bionic's on the phone, a stand-in in the host tests).

/// The properties read.
pub const PROPS: [&str; 3] = ["sys.usb.state", "sys.usb.config", "sys.usb.controller"];
pub const STATE: usize = 0;
pub const CONFIG: usize = 1;
pub const CONTROLLER: usize = 2;
/// PROP_VALUE_MAX
pub const VALUE_MAX: usize = 92;

/// Where the properties come from (xh_usb_source).
pub trait PropSource {
    /// Property `name` (the i-th of [`PROPS`]): None when there is none; else a number that changes with every change
    /// of its value (bionic's __system_property_serial).
    fn serial(&mut self, i: usize, name: &str) -> Option<u32>;
    /// Its value; None when it cannot be read.
    fn value(&mut self, i: usize, name: &str) -> Option<String>;
}

/// What the watch has seen (xh_usb_watch).
#[derive(Clone, Debug, Default)]
pub struct UsbWatch {
    /// prefix of the /sys path: "" on the phone, a folder in the tests
    pub root: String,
    value: [String; 3],
    serial: [u32; 3],
    known: [bool; 3],
    logged: bool,
    /// the last line, "usb: state S, config C, UDC NAME STATE"
    pub line: String,
}

/// xh_usb_controller_ok: a controller's name is used only when it is one path component.
pub fn controller_ok(name: &str) -> bool {
    !name.is_empty()
        && name.len() < VALUE_MAX
        && name != "."
        && name != ".."
        && name.bytes().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b':' | b'-'))
}

/// The first line of a small named file, without its line break; "" when it cannot be read.
fn read_line(path: &str) -> String {
    let buf = xh_core::fs::read_bytes(path, 63).unwrap_or_default();
    let text = String::from_utf8_lossy(&buf);
    text.split(['\r', '\n']).next().unwrap_or("").to_owned()
}

fn shown(value: &str) -> &str {
    if value.is_empty() {
        "(none)"
    } else {
        value
    }
}

impl UsbWatch {
    pub fn new(root: &str) -> Self {
        UsbWatch { root: root.to_owned(), ..Self::default() }
    }

    /// xh_usb_poll: reads the gadget once (a property's value only when its serial changed): true when the line is new
    /// (the first poll or a change).
    pub fn poll(&mut self, mut src: Option<&mut dyn PropSource>) -> bool {
        for (i, name) in PROPS.iter().enumerate() {
            let serial = src.as_deref_mut().and_then(|s| s.serial(i, name));
            let Some(serial) = serial else {
                // no such property (yet)
                self.value[i].clear();
                self.known[i] = false;
                continue;
            };
            if self.known[i] && serial == self.serial[i] {
                continue; // unchanged: the value read before
            }
            let mut value = src.as_deref_mut().and_then(|s| s.value(i, name)).unwrap_or_default();
            xh_core::text::truncate_string(&mut value, VALUE_MAX - 1);
            self.value[i] = value;
            self.serial[i] = serial;
            self.known[i] = true;
        }
        let controller = &self.value[CONTROLLER];
        let named = controller_ok(controller);
        let state =
            if named { read_line(&format!("{}/sys/class/udc/{controller}/state", self.root)) } else { String::new() };
        let udc = if named {
            controller.as_str()
        } else if controller.is_empty() {
            "(none)"
        } else {
            "(not a name)"
        };
        let line = format!(
            "usb: state {}, config {}, UDC {udc} {}",
            shown(&self.value[STATE]),
            shown(&self.value[CONFIG]),
            if state.is_empty() { "(unreadable)" } else { &state }
        );
        let changed = !self.logged || line != self.line;
        self.line = line;
        self.logged = true;
        changed
    }
}

/// The phone's property area through bionic. A prop_info, once found, stays where it is for the life of the area:
/// found once, then only its serial is read on each poll (as __system_property_wait does), and its value when that
/// changed.
#[cfg(target_os = "android")]
mod bionic {
    use std::ffi::{c_char, c_void, CStr, CString};

    #[repr(C)]
    struct PropInfo {
        _opaque: [u8; 0],
    }

    extern "C" {
        fn __system_property_find(name: *const c_char) -> *const PropInfo;
        fn __system_property_serial(pi: *const PropInfo) -> u32;
        fn __system_property_read_callback(
            pi: *const PropInfo,
            callback: extern "C" fn(cookie: *mut c_void, name: *const c_char, value: *const c_char, serial: u32),
            cookie: *mut c_void,
        );
    }

    /// bionic's property area.
    #[derive(Default)]
    pub struct Bionic {
        pi: [Option<std::ptr::NonNull<PropInfo>>; 3],
    }

    // SAFETY: a prop_info is bionic's, shared memory every thread may read; Bionic only reads it.
    unsafe impl Send for Bionic {}

    extern "C" fn got_value(cookie: *mut c_void, _name: *const c_char, value: *const c_char, _serial: u32) {
        // SAFETY: cookie is the &mut String value() passed; value is bionic's NUL-terminated value.
        unsafe { *(cookie as *mut String) = CStr::from_ptr(value).to_string_lossy().into_owned() };
    }

    impl super::PropSource for Bionic {
        fn serial(&mut self, i: usize, name: &str) -> Option<u32> {
            if self.pi[i].is_none() {
                let name = CString::new(name).ok()?;
                // SAFETY: a NUL-terminated name; the result is NULL or a prop_info that lives as long as the process.
                self.pi[i] = std::ptr::NonNull::new(unsafe { __system_property_find(name.as_ptr()) } as *mut PropInfo);
            }
            // SAFETY: a prop_info __system_property_find returned.
            self.pi[i].map(|pi| unsafe { __system_property_serial(pi.as_ptr()) })
        }

        fn value(&mut self, i: usize, _name: &str) -> Option<String> {
            let pi = self.pi[i]?;
            let mut value = String::new();
            // SAFETY: a prop_info __system_property_find returned; the callback writes the String it is given, during
            // the call.
            unsafe {
                __system_property_read_callback(pi.as_ptr(), got_value, &mut value as *mut String as *mut c_void)
            };
            Some(value)
        }
    }
}

/// xh_usb_bionic: the phone's property area; None where there is none (the host build).
pub fn bionic() -> Option<Box<dyn PropSource + Send>> {
    #[cfg(target_os = "android")]
    {
        Some(Box::new(bionic::Bionic::default()))
    }
    #[cfg(not(target_os = "android"))]
    {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use std::fs;

    /// A stand-in property area: values with bionic's kind of serial (changes with the value), and a count of the reads.
    #[derive(Default)]
    struct Fake {
        value: [String; 3],
        serial: [u32; 3],
        present: [bool; 3],
        reads: u32,
    }

    impl Fake {
        fn set(&mut self, i: usize, value: &str) {
            self.value[i] = value.to_owned();
            self.present[i] = true;
            self.serial[i] += 2;
        }
    }

    impl PropSource for Fake {
        fn serial(&mut self, i: usize, _name: &str) -> Option<u32> {
            self.present[i].then_some(self.serial[i])
        }
        fn value(&mut self, i: usize, _name: &str) -> Option<String> {
            self.reads += 1;
            Some(self.value[i].clone())
        }
    }

    #[test]
    fn usb_line() {
        let dir = std::env::temp_dir().join(format!("xnuhost-usb-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let udc = dir.join("usb/sys/class/udc/a600000.dwc3");
        fs::create_dir_all(&udc).unwrap();
        let state = udc.join("state");
        fs::write(&state, "configured\n").unwrap();
        let mut w = UsbWatch::new(dir.join("usb").to_str().unwrap());
        let mut f = Fake::default();
        // No property yet (the host, or a property area that does not have them): said so, and logged once.
        assert!(w.poll(Some(&mut f)) && w.line == "usb: state (none), config (none), UDC (none) (unreadable)");
        assert!(!w.poll(Some(&mut f)));
        assert!(!w.poll(None)); // no property area at all: the same
        f.set(STATE, "mtp,adb");
        f.set(CONFIG, "mtp,adb");
        f.set(CONTROLLER, "a600000.dwc3");
        assert!(w.poll(Some(&mut f)) && w.line == "usb: state mtp,adb, config mtp,adb, UDC a600000.dwc3 configured");
        // Unchanged serials: no value is read again, and nothing to log.
        let reads = f.reads;
        assert!(!w.poll(Some(&mut f)) && f.reads == reads);
        // A value read only through its serial: a value that changed under the same serial is not seen (bionic never
        // does that).
        f.value[STATE] = "none".into();
        assert!(!w.poll(Some(&mut f)) && w.line.contains("state mtp,adb,"));
        // The cable out: the UDC's state file changes, the properties do not.
        fs::write(&state, "not attached\n").unwrap();
        assert!(w.poll(Some(&mut f)) && w.line == "usb: state mtp,adb, config mtp,adb, UDC a600000.dwc3 not attached");
        assert!(!w.poll(Some(&mut f)));
        // adb gone from the gadget: one property, one read.
        let reads = f.reads;
        f.set(STATE, "mtp");
        assert!(w.poll(Some(&mut f)) && f.reads == reads + 1);
        assert_eq!(w.line, "usb: state mtp, config mtp,adb, UDC a600000.dwc3 not attached");
        // A property that goes away (a stand-in only: bionic's do not), then a controller that is not a name: its file
        // is never opened.
        f.present[CONFIG] = false;
        assert!(w.poll(Some(&mut f)) && w.line.contains("config (none),"));
        f.set(CONFIG, "adb");
        f.set(CONTROLLER, "../../../proc/self");
        assert!(w.poll(Some(&mut f)) && w.line == "usb: state mtp, config adb, UDC (not a name) (unreadable)");
        f.set(CONTROLLER, "");
        assert!(w.poll(Some(&mut f)) && w.line == "usb: state mtp, config adb, UDC (none) (unreadable)");
        f.set(CONTROLLER, "dummy_udc.0");
        assert!(w.poll(Some(&mut f)) && w.line == "usb: state mtp, config adb, UDC dummy_udc.0 (unreadable)");
        assert!(controller_ok("a600000.dwc3") && controller_ok("musb-hdrc.0:x") && !controller_ok("a/b"));
        assert!(!controller_ok("..") && !controller_ok(".") && !controller_ok("") && !controller_ok("a b"));
        // The configuration key: 3 s by default, 0 = off, at most an hour.
        let mut c = Config::new();
        assert!(!c.given("usb-log") && c.usb_log == 3);
        assert!(c.set("usb-log", "0").is_ok() && c.usb_log == 0 && c.set("usb-log", "3601").is_err());
        let _ = fs::remove_dir_all(&dir);
    }
}
