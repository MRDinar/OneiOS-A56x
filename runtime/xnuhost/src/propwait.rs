// SPDX-License-Identifier: MIT
//! `xnuhost --prop-wait SECONDS NAME=VALUE...` (docs/booter.md 2.9d, oneos/interface.md 4a): waits, without a process
//! per look, until one of Android's properties NAME has the value VALUE, or SECONDS pass. The OneOS switch's stop loop
//! (`oneios_switch.sh --stop-loop`) runs it to keep zygote, its second one, the boot animation and the Android
//! takeover's stopped services stopped: before this it ran three `getprop` every 0.2 s for the whole session (15
//! processes a second, stage 2a of docs/plan-ios-first-session.md).
//!
//! Read only: bionic's property area, as `getprop` reads it (`__system_property_find`, `__system_property_read_callback`),
//! and its global serial, which init raises with every property it sets (`__system_property_area_serial`,
//! `__system_property_wait` on it: a futex, so the wait costs nothing until a property changes). It sets nothing,
//! signals nothing and ends with the shell that started it (PR_SET_PDEATHSIG).
//!
//! Exit codes: 0 a property has its value (each such NAME on a line of stdout), 1 the time passed, 2 a bad command line
//! or no property area (the host build). An xnuhost from before this exits 2 too (`bad value for --prop-wait`),
//! so the switch reads 0 and 1 as "the wait works" and anything else as "look once a second instead".

use std::time::{Duration, Instant};

/// The longest wait one call takes (the stop loop asks for 60 s).
pub const MAX_SECONDS: u64 = 3600;
/// At most this many NAME=VALUE pairs.
pub const MAX_WANTS: usize = 32;
/// PROP_VALUE_MAX less its NUL.
const VALUE_MAX: usize = 91;

/// One NAME=VALUE.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Want {
    pub name: String,
    pub value: String,
}

/// Android's property area (bionic's on the phone, a stand-in in the tests).
pub trait Area {
    /// The area's serial: it changes with every property init sets or adds.
    fn serial(&mut self) -> u32;
    /// Property `name`'s value; None when there is no such property.
    fn value(&mut self, name: &str) -> Option<String>;
    /// Waits until the area's serial is no longer `old`, `timeout` at most. Whether it changed.
    fn wait(&mut self, old: u32, timeout: Duration) -> bool;
}

/// The command line after `--prop-wait`: SECONDS, then one NAME=VALUE or more.
pub fn parse(args: &[String]) -> Result<(Duration, Vec<Want>), String> {
    let (secs, pairs) = args.split_first().ok_or("--prop-wait takes SECONDS NAME=VALUE...")?;
    let secs: u64 = secs
        .parse()
        .ok()
        .filter(|s| *s <= MAX_SECONDS)
        .ok_or_else(|| format!("--prop-wait: SECONDS '{secs}' is not a whole number from 0 to {MAX_SECONDS}"))?;
    if pairs.is_empty() || pairs.len() > MAX_WANTS {
        return Err(format!("--prop-wait takes 1 to {MAX_WANTS} NAME=VALUE"));
    }
    let mut wants = Vec::with_capacity(pairs.len());
    for p in pairs {
        let (name, value) = p.split_once('=').ok_or_else(|| format!("--prop-wait: '{p}' is not NAME=VALUE"))?;
        let name_ok = !name.is_empty()
            && name.len() <= 256
            && name.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b'@' | b':'));
        if !name_ok || value.len() > VALUE_MAX || value.bytes().any(|b| b == 0 || b == b'\n') {
            return Err(format!("--prop-wait: '{p}' is not a property name and a value"));
        }
        wants.push(Want { name: name.to_owned(), value: value.to_owned() });
    }
    Ok((Duration::from_secs(secs), wants))
}

/// Waits until one of `wants` holds, `timeout` at most: the names that hold (empty: the time passed). Every look reads
/// the area's serial first, so a property set between the look and the wait ends the wait at once.
pub fn run(area: &mut dyn Area, timeout: Duration, wants: &[Want], mut now: impl FnMut() -> Instant) -> Vec<String> {
    let end = now() + timeout;
    loop {
        let serial = area.serial();
        let hits: Vec<String> = wants
            .iter()
            .filter(|w| area.value(&w.name).as_deref() == Some(w.value.as_str()))
            .map(|w| w.name.clone())
            .collect();
        if !hits.is_empty() {
            return hits;
        }
        let left = end.saturating_duration_since(now());
        if left.is_zero() {
            return hits;
        }
        area.wait(serial, left);
    }
}

#[cfg(target_os = "android")]
mod bionic {
    use std::collections::HashMap;
    use std::ffi::{c_char, c_void, CStr, CString};
    use std::time::Duration;

    #[repr(C)]
    struct PropInfo {
        _opaque: [u8; 0],
    }

    extern "C" {
        fn __system_property_find(name: *const c_char) -> *const PropInfo;
        fn __system_property_read_callback(
            pi: *const PropInfo,
            callback: extern "C" fn(cookie: *mut c_void, name: *const c_char, value: *const c_char, serial: u32),
            cookie: *mut c_void,
        );
        fn __system_property_area_serial() -> u32;
        fn __system_property_wait(
            pi: *const PropInfo,
            old_serial: u32,
            new_serial: *mut u32,
            relative_timeout: *const libc::timespec,
        ) -> bool;
    }

    extern "C" fn got_value(cookie: *mut c_void, _name: *const c_char, value: *const c_char, _serial: u32) {
        // SAFETY: cookie is the &mut String value() passed; value is bionic's NUL-terminated value.
        unsafe { *(cookie as *mut String) = CStr::from_ptr(value).to_string_lossy().into_owned() };
    }

    /// bionic's property area. A prop_info, once found, stays where it is for the life of the area.
    #[derive(Default)]
    pub struct Bionic {
        found: HashMap<String, std::ptr::NonNull<PropInfo>>,
    }

    impl super::Area for Bionic {
        fn serial(&mut self) -> u32 {
            // SAFETY: bionic's call, no arguments.
            unsafe { __system_property_area_serial() }
        }

        fn value(&mut self, name: &str) -> Option<String> {
            let pi = match self.found.get(name) {
                Some(pi) => *pi,
                None => {
                    let c = CString::new(name).ok()?;
                    // SAFETY: a NUL-terminated name; the result is NULL or a prop_info that lives as long as the process.
                    let pi = std::ptr::NonNull::new(unsafe { __system_property_find(c.as_ptr()) } as *mut PropInfo)?;
                    self.found.insert(name.to_owned(), pi);
                    pi
                }
            };
            let mut value = String::new();
            // SAFETY: a prop_info __system_property_find returned; the callback writes the String it is given, during
            // the call.
            unsafe {
                __system_property_read_callback(pi.as_ptr(), got_value, &mut value as *mut String as *mut c_void)
            };
            Some(value)
        }

        fn wait(&mut self, old: u32, timeout: Duration) -> bool {
            let ts = libc::timespec { tv_sec: timeout.as_secs() as libc::time_t, tv_nsec: timeout.subsec_nanos() as _ };
            let mut new = old;
            // SAFETY: NULL waits on the area's serial; new and ts outlive the call.
            unsafe { __system_property_wait(std::ptr::null(), old, &mut new, &ts) }
        }
    }
}

/// `xnuhost --prop-wait ...`: `args` are the words after `--prop-wait`. The exit code (above).
pub fn main(args: &[String]) -> i32 {
    let (timeout, wants) = match parse(args) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("xnuhost: {e}");
            return 2;
        }
    };
    #[cfg(target_os = "android")]
    {
        // It ends with the shell that started it: a stop loop that is killed leaves no waiter behind.
        // SAFETY: prctl with integer arguments.
        unsafe { libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL as libc::c_ulong, 0, 0, 0) };
        // SAFETY: getppid has no arguments.
        if unsafe { libc::getppid() } == 1 {
            return 1;
        }
        let hits = run(&mut bionic::Bionic::default(), timeout, &wants, Instant::now);
        if hits.is_empty() {
            return 1;
        }
        for h in hits {
            println!("{h}");
        }
        0
    }
    #[cfg(not(target_os = "android"))]
    {
        let _ = (timeout, wants);
        eprintln!("xnuhost: --prop-wait: no Android property area in this build");
        2
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn words(s: &str) -> Vec<String> {
        s.split_whitespace().map(String::from).collect()
    }

    /// A stand-in area: values, a serial, and what happens at each wait (a property set, or nothing).
    struct Fake {
        values: HashMap<String, String>,
        serial: u32,
        /// at the n-th wait: set this property to this value (and raise the serial), or let the time pass
        script: Vec<Option<(&'static str, &'static str)>>,
        waits: usize,
        looks: usize,
        clock: std::rc::Rc<std::cell::Cell<Duration>>,
    }

    impl Area for Fake {
        fn serial(&mut self) -> u32 {
            self.serial
        }
        fn value(&mut self, name: &str) -> Option<String> {
            self.looks += 1;
            self.values.get(name).cloned()
        }
        fn wait(&mut self, old: u32, timeout: Duration) -> bool {
            assert_eq!(old, self.serial, "the wait is on the serial read before the look");
            let step = self.script.get(self.waits).copied().flatten();
            self.waits += 1;
            match step {
                Some((k, v)) => {
                    self.values.insert(k.into(), v.into());
                    self.serial += 1;
                    self.clock.set(self.clock.get() + Duration::from_millis(10));
                    true
                }
                None => {
                    self.clock.set(self.clock.get() + timeout);
                    false
                }
            }
        }
    }

    fn fake(values: &[(&str, &str)], script: Vec<Option<(&'static str, &'static str)>>) -> Fake {
        Fake {
            values: values.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
            serial: 7,
            script,
            waits: 0,
            looks: 0,
            clock: Default::default(),
        }
    }

    fn go(f: &mut Fake, secs: u64, wants: &str) -> Vec<String> {
        let (_, w) = parse(&words(&format!("{secs} {wants}"))).unwrap();
        let start = Instant::now();
        let clock = f.clock.clone();
        run(f, Duration::from_secs(secs), &w, move || start + clock.get())
    }

    #[test]
    fn the_command_line() {
        let (t, w) = parse(&words("60 init.svc.zygote=running init.svc.media.tuner=running")).unwrap();
        assert_eq!(t, Duration::from_secs(60));
        assert_eq!(w[1], Want { name: "init.svc.media.tuner".into(), value: "running".into() });
        // an empty value is a value (a property that is set to "")
        assert_eq!(parse(&words("0 a.b=")).unwrap().1[0].value, "");
        for bad in ["", "60", "x a=b", "-1 a=b", "3601 a=b", "60 ab", "60 =b", "60 a/b=c", "60 a$b=c"] {
            assert!(parse(&words(bad)).is_err(), "{bad}");
        }
        let many = (0..=MAX_WANTS).map(|i| format!("p{i}=1")).collect::<Vec<_>>().join(" ");
        assert!(parse(&words(&format!("1 {many}"))).is_err());
        assert!(parse(&words(&format!("1 a={}", "v".repeat(92)))).is_err());
    }

    #[test]
    fn a_property_that_already_holds_ends_it_at_once() {
        let mut f = fake(&[("init.svc.zygote", "stopped"), ("init.svc.bootanim", "running")], vec![]);
        assert_eq!(go(&mut f, 60, "init.svc.zygote=running init.svc.bootanim=running"), ["init.svc.bootanim"]);
        assert_eq!(f.waits, 0);
    }

    #[test]
    fn it_sleeps_until_a_property_changes_and_looks_again_only_then() {
        // Two unrelated changes wake it (a look each), the third is zygote's start.
        let mut f = fake(
            &[("init.svc.zygote", "stopped")],
            vec![
                Some(("sys.usb.state", "adb")),
                Some(("init.svc.zygote", "starting")),
                Some(("init.svc.zygote", "running")),
            ],
        );
        assert_eq!(go(&mut f, 60, "init.svc.zygote=running init.svc.zygote_secondary=running"), ["init.svc.zygote"]);
        assert_eq!(f.waits, 3);
        // two names per look, four looks: no look without a change
        assert_eq!(f.looks, 8);
    }

    #[test]
    fn the_time_passes_with_nothing_found() {
        let mut f = fake(&[("init.svc.zygote", "stopped")], vec![None]);
        assert!(go(&mut f, 60, "init.svc.zygote=running init.svc.nothere=running").is_empty());
        assert_eq!(f.waits, 1);
        // 0 s: one look, no wait (the switch's probe)
        let mut f = fake(&[], vec![]);
        assert!(go(&mut f, 0, "a=b").is_empty());
        assert_eq!((f.waits, f.looks), (0, 1));
    }

    #[test]
    fn a_property_that_appears_later_is_found() {
        let mut f = fake(&[], vec![Some(("init.svc.perfmond", "running"))]);
        assert_eq!(go(&mut f, 5, "init.svc.perfmond=running"), ["init.svc.perfmond"]);
    }

    #[test]
    fn the_host_build_says_it_cannot_wait() {
        // exit 2: the switch then looks once a second (as it does with an xnuhost from before this)
        #[cfg(not(target_os = "android"))]
        assert_eq!(main(&words("0 a=b")), 2);
        assert_eq!(main(&words("x a=b")), 2);
    }
}
