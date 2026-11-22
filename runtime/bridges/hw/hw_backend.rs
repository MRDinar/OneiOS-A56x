// SPDX-License-Identifier: MIT
//! The hardware backend layer's settings and choice (C: hw/hw_backend.c, include/bridges/hw_backend.h). Which form of
//! the phone's hardware each bridge uses (docs/hardware.md, "Hardware backends", the audit table; docs/architecture.md,
//! "The backend layer"):
//!   samsung   today's paths, the default: the Galaxy S24 Ultra's own nodes and names (sec_*, the panel's backlight
//!             node, Samsung's thermistors, the force-feedback device sec_vibrator_inputff, ...). Unchanged.
//!   treble    the vendor-neutral interfaces any Treble/GSI phone has: the stable AIDL HALs through servicemanager
//!             (health, thermal, vibrator, ...), evdev found by its capabilities, the standard sysfs classes found by
//!             named paths (never a directory walk).
//!
//! Settings (xnuhost.conf, `--set KEY=VALUE` or `--hw-backend X` on the command line; the ctl tools: the environment
//! ONEIOS_HW_BACKEND, ONEIOS_HW_BACKEND_<BRIDGE>, ONEIOS_HW_FALLBACK):
//!   hw-backend=samsung|treble           every bridge (default samsung)
//!   hw-backend-<bridge>=samsung|treble  one bridge (backlight, battery, thermal, haptics, touch, keys, audio, camera,
//!                                       wifi, bluetooth, display, encoder): wins over hw-backend
//!   hw-backend-fallback=0|1             a treble bridge whose interface is missing falls back to samsung (1) or stays
//!                                       off (0, the default)
//!
//! Pure apart from the process's settings ([`hw_configure`]) and the log lines (hw_log.rs). xnuhost configures it once,
//! before the bridges open.

use std::sync::{PoisonError, RwLock};

/// A backend. Unset: in the settings, not given; in a bridge's options, the process's setting.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Backend {
    #[default]
    Unset,
    Samsung,
    Treble,
}

impl Backend {
    /// "samsung", "treble", "unset".
    pub fn name(self) -> &'static str {
        match self {
            Backend::Samsung => "samsung",
            Backend::Treble => "treble",
            Backend::Unset => "unset",
        }
    }

    /// "samsung" or "treble" (exactly); anything else is None.
    pub fn parse(text: &str) -> Option<Backend> {
        match text {
            "samsung" => Some(Backend::Samsung),
            "treble" => Some(Backend::Treble),
            _ => None,
        }
    }
}

/// A bridge that has a backend choice (C: BR_HW_BACKLIGHT ... BR_HW_ENCODER, in this order).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Bridge {
    Backlight,
    Battery,
    Thermal,
    Haptics,
    Touch,
    Keys,
    Audio,
    Camera,
    Wifi,
    Bluetooth,
    Display,
    Encoder,
}

impl Bridge {
    pub const ALL: [Bridge; 12] = [
        Bridge::Backlight,
        Bridge::Battery,
        Bridge::Thermal,
        Bridge::Haptics,
        Bridge::Touch,
        Bridge::Keys,
        Bridge::Audio,
        Bridge::Camera,
        Bridge::Wifi,
        Bridge::Bluetooth,
        Bridge::Display,
        Bridge::Encoder,
    ];
    pub const COUNT: usize = Bridge::ALL.len();

    /// "backlight", "battery", ...: the hw-backend-<bridge> key's last word.
    pub fn name(self) -> &'static str {
        match self {
            Bridge::Backlight => "backlight",
            Bridge::Battery => "battery",
            Bridge::Thermal => "thermal",
            Bridge::Haptics => "haptics",
            Bridge::Touch => "touch",
            Bridge::Keys => "keys",
            Bridge::Audio => "audio",
            Bridge::Camera => "camera",
            Bridge::Wifi => "wifi",
            Bridge::Bluetooth => "bluetooth",
            Bridge::Display => "display",
            Bridge::Encoder => "encoder",
        }
    }

    /// The bridge of that name (exactly), or None.
    pub fn from_name(name: &str) -> Option<Bridge> {
        Bridge::ALL.into_iter().find(|b| b.name() == name)
    }

    /// ONEIOS_HW_BACKEND_BACKLIGHT, ...: the ctl tools' variable for this bridge.
    pub fn env_name(self) -> String {
        format!("ONEIOS_HW_BACKEND_{}", self.name().to_ascii_uppercase())
    }
}

/// Why a setting was not taken (C: br_hw_setting's -1 and 1).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HwSettingError {
    /// A key of ours with a bad value (or hw-backend-<a bridge there is none of>: a typo, not someone else's key).
    BadValue,
    /// Not a key of ours.
    NotOurs,
}

/// The settings: all unset by default (every bridge samsung, no fallback).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HwSettings {
    /// hw-backend
    pub all: Backend,
    /// hw-backend-<bridge>, by `Bridge as usize`
    pub one: [Backend; Bridge::COUNT],
    /// hw-backend-fallback: None unset
    pub fallback: Option<bool>,
}

impl HwSettings {
    /// One setting.
    pub fn setting(&mut self, key: &str, value: &str) -> Result<(), HwSettingError> {
        let rest = key.strip_prefix("hw-backend").ok_or(HwSettingError::NotOurs)?;
        if rest == "-fallback" {
            self.fallback = Some(match value {
                "1" | "yes" | "true" => true,
                "0" | "no" | "false" => false,
                _ => return Err(HwSettingError::BadValue),
            });
            return Ok(());
        }
        let backend = Backend::parse(value);
        if rest.is_empty() {
            self.all = backend.ok_or(HwSettingError::BadValue)?;
            return Ok(());
        }
        let name = rest.strip_prefix('-').ok_or(HwSettingError::NotOurs)?;
        let bridge = Bridge::from_name(name).ok_or(HwSettingError::BadValue)?;
        self.one[bridge as usize] = backend.ok_or(HwSettingError::BadValue)?;
        Ok(())
    }

    /// What `self` leaves unset is taken from `under` (the command line over the image set's xnuhost.conf).
    pub fn merge(&mut self, under: &HwSettings) {
        if self.all == Backend::Unset {
            self.all = under.all;
        }
        for (mine, theirs) in self.one.iter_mut().zip(under.one) {
            if *mine == Backend::Unset {
                *mine = theirs;
            }
        }
        if self.fallback.is_none() {
            self.fallback = under.fallback;
        }
    }

    /// The environment's (ONEIOS_HW_BACKEND, ONEIOS_HW_BACKEND_BACKLIGHT, ..., ONEIOS_HW_FALLBACK): the ctl tools.
    /// Err: the bad variable's name.
    pub fn env(&mut self) -> Result<(), String> {
        let given = |name: &str| std::env::var(name).ok().filter(|v| !v.is_empty());
        if let Some(v) = given("ONEIOS_HW_BACKEND") {
            self.setting("hw-backend", &v).map_err(|_| "ONEIOS_HW_BACKEND".to_owned())?;
        }
        for bridge in Bridge::ALL {
            let name = bridge.env_name();
            if let Some(v) = given(&name) {
                self.setting(&format!("hw-backend-{}", bridge.name()), &v).map_err(|_| name.clone())?;
            }
        }
        if let Some(v) = given("ONEIOS_HW_FALLBACK") {
            self.setting("hw-backend-fallback", &v).map_err(|_| "ONEIOS_HW_FALLBACK".to_owned())?;
        }
        Ok(())
    }

    /// The choice for one bridge: forced (a bridge's options, a test; Unset: none), else hw-backend-<bridge>, else
    /// hw-backend, else samsung; and which: "hw-backend-touch=treble", "hw-backend=treble", "the default", "the
    /// caller's choice".
    pub fn choose(&self, bridge: Bridge, forced: Backend) -> (Backend, String) {
        let one = self.one[bridge as usize];
        if forced != Backend::Unset {
            (forced, "the caller's choice".to_owned())
        } else if one != Backend::Unset {
            (one, format!("hw-backend-{}={}", bridge.name(), one.name()))
        } else if self.all != Backend::Unset {
            (self.all, format!("hw-backend={}", self.all.name()))
        } else {
            (Backend::Samsung, "the default".to_owned())
        }
    }

    /// The backend [`HwSettings::choose`] gives, without the why.
    pub fn backend(&self, bridge: Bridge, forced: Backend) -> Backend {
        self.choose(bridge, forced).0
    }

    pub fn fallback_allowed(&self) -> bool {
        self.fallback == Some(true)
    }
}

static CURRENT: RwLock<HwSettings> =
    RwLock::new(HwSettings { all: Backend::Unset, one: [Backend::Unset; Bridge::COUNT], fallback: None });

/// The process's settings: xnuhost sets them from its config before any bridge opens; a tool from the environment.
/// Before that they are all unset (every bridge samsung, no fallback). `&HwSettings::default()` resets them.
pub fn hw_configure(settings: &HwSettings) {
    *CURRENT.write().unwrap_or_else(PoisonError::into_inner) = *settings;
}

/// A copy of the process's settings.
pub fn hw_current() -> HwSettings {
    *CURRENT.read().unwrap_or_else(PoisonError::into_inner)
}

/// Whether the process's settings let a treble bridge whose interface is missing fall back to samsung.
pub fn fallback_allowed() -> bool {
    hw_current().fallback_allowed()
}
