// SPDX-License-Identifier: MIT
//! hw/: the hardware backends (`hw-backend=samsung|treble`, `hw-backend-<bridge>`, `hw-backend-fallback`) and the host
//! device (the table of known phones and its detection). docs/hardware.md 7e and 7h.

pub mod host_device; // hw/host_device.c: the host phone and its defaults
pub mod hw_backend; // hw/hw_backend.c: the settings and the choice (pure)
pub mod hw_log; // hw/hw_log.c: the `hw: <bridge>: backend ...` and `... treble: missing: ...` lines

pub use host_device::{
    host_choose, host_describe, host_env_apply, host_env_describe, host_find, host_match, host_names, host_read,
    host_table, host_tool, HostDevice, HostFacts,
};
pub use hw_backend::{fallback_allowed, hw_configure, hw_current, Backend, Bridge, HwSettingError, HwSettings};
pub use hw_log::{missing, select};

#[cfg(test)]
mod test_hw_backend;
#[cfg(test)]
pub(crate) mod testdir; // hw/test_hw_backend.c
#[cfg(test)]
#[allow(unused_imports)] // the type for tests that keep a guard in a struct
pub(crate) use hw_backend::{settings_lock, SettingsGuard};
