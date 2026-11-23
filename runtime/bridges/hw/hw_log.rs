// SPDX-License-Identifier: MIT
//! The hardware backend layer's log lines (C: hw/hw_log.c): which backend each bridge uses and why, and the one line
//! of a treble interface that is missing.

use super::hw_backend::{hw_current, Backend, Bridge};
use crate::log::Level;

/// The choice for a bridge from the process's settings, with its one log line:
///   "hw: backlight: backend treble (hw-backend=treble)"
pub fn select(bridge: Bridge, forced: Backend) -> Backend {
    let (backend, why) = hw_current().choose(bridge, forced);
    crate::br_info!("hw: {}: backend {} ({why})", bridge.name(), backend.name());
    backend
}

/// A treble interface that is missing: one line, and whether the bridge may use samsung instead:
///   "hw: battery: treble: missing: <what>; falling back to samsung (hw-backend-fallback=1)"   -> true
///   "hw: battery: treble: missing: <what>; the bridge stays off (hw-backend-fallback=0)"       -> false
pub fn missing(bridge: Bridge, what: &str) -> bool {
    let fallback = hw_current().fallback_allowed();
    crate::br_log!(
        if fallback { Level::Info } else { Level::Error },
        "hw: {}: treble: missing: {what}; {}",
        bridge.name(),
        if fallback {
            "falling back to samsung (hw-backend-fallback=1)"
        } else {
            "the bridge stays off (hw-backend-fallback=0)"
        }
    );
    fallback
}
