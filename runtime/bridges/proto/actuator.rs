// SPDX-License-Identifier: MIT
//! OneiOS haptics (C: drivers/haptics/oneios_actuator.h, which stays for the kernel's oneios_haptic.c): what the
//! "Actuator" audio device's samples ask of the motor. The host uses it in its tests, which play the guest's side of
//! the haptic channel.
//!
//! iOS plays its haptics as audio on the Actuator device: 48 kHz samples whose envelope is the force and whose carrier
//! is the sharpness (145..300 Hz on an iPhone). The phone's LRA is driven by sine effects (bridges/haptics): one
//! request is "vibrate for N ms at intensity I and F Hz". So the samples are turned into few requests:
//!   - a period is loud when a sample's magnitude is above OA_SILENT;
//!   - the first loud period asks for OA_TAP_MS; while the run goes on, a new request is sent when the one in flight
//!     has less than OA_RENEW_MS left (OA_SEGMENT_MS each once the run is longer than a tap) or the level moved by
//!     more than OA_LEVEL_STEP of 1000;
//!   - a long run (OA_LONG_MS or more) is stopped after OA_QUIET_MS of silence; a tap runs out;
//!   - intensity = the period's peak, 1..1000 of full scale; frequency = the run's zero crossings, once the run is
//!     10 ms long (0 before: the host plays the actuator's resonance).

pub const OA_RATE: u32 = 48000;
pub const OA_SILENT: u32 = 256; // of 32768
pub const OA_TAP_MS: u32 = 20;
pub const OA_SEGMENT_MS: u32 = 50;
pub const OA_RENEW_MS: u32 = 12;
pub const OA_LONG_MS: u32 = 30;
pub const OA_QUIET_MS: u32 = 10;
pub const OA_LEVEL_STEP: u32 = 100;

pub const OA_NONE: i32 = 0;
pub const OA_VIB: i32 = 1;
pub const OA_STOP: i32 = 2;
pub const OA_IMPACT: i32 = 3;

/// struct oa_state: the caller's state; time is counted in the periods it is given.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OaState {
    /// a request is in flight
    pub on: bool,
    /// opt-in; only if the host advertised native impacts
    pub native_taps: bool,
    /// strongest PCM sample in a not-yet-classified tap
    pub short_peak: u32,
    /// the periods given so far, in microseconds
    pub now_us: u32,
    /// when the request in flight ends
    pub until_us: u32,
    pub level: u32,
    pub hz: u32,
    pub run_us: u32,
    pub run_crossings: u32,
    pub quiet_us: u32,
    /// the sign of the first channel's last sample above OA_SILENT (0: none yet)
    pub sign: i8,
}

/// struct oa_request: OA_NONE, OA_VIB, OA_STOP or OA_IMPACT.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OaRequest {
    pub action: i32,
    pub ms: u32,
    pub intensity: u32,
    pub hz: u32,
}

/// oa_period: one engine period of `count` frames of `channels` interleaved samples.
pub fn oa_period(s: &mut OaState, frames: &[i16], count: u32, channels: u32) -> OaRequest {
    let mut r = OaRequest::default();
    let (mut peak, mut crossings) = (0u32, 0u32);
    let period_us = (u64::from(count) * 1_000_000 / u64::from(OA_RATE)) as u32;
    let channels = channels.max(1) as usize;
    for i in 0..count as usize {
        for c in 0..channels {
            peak = peak.max(i32::from(frames[i * channels + c]).unsigned_abs());
        }
        let x = frames[i * channels];
        if i32::from(x).unsigned_abs() > OA_SILENT {
            // a crossing with hysteresis: the sign of loud samples only
            let sign = if x < 0 { -1 } else { 1 };
            if s.sign != 0 && sign != s.sign {
                crossings += 1;
            }
            s.sign = sign;
        }
    }
    s.now_us = s.now_us.wrapping_add(period_us);
    if s.on && s.now_us >= s.until_us {
        s.on = false; // the request ran out by itself
    }
    if peak > OA_SILENT {
        s.short_peak = s.short_peak.max(peak);
        s.run_us += period_us;
        s.run_crossings += crossings;
        s.quiet_us = 0;
        let intensity = (peak * 1000 / 32768).clamp(1, 1000);
        let hz = if s.run_us >= 10000 {
            (u64::from(s.run_crossings) * 1_000_000 / (2 * u64::from(s.run_us))) as u32
        } else {
            0
        };
        let moved = intensity.abs_diff(s.level);
        let renew = s.on && s.until_us.wrapping_sub(s.now_us) < OA_RENEW_MS * 1000;
        // When opted in, hold a newly started run for at most 30 ms. A short run confirmed by silence becomes ONE
        // impact rather than several 20 ms motor starts; longer runs keep VIB/STOP semantics.
        if s.native_taps && !s.on && s.run_us < OA_LONG_MS * 1000 {
            return r;
        }
        if !s.on || renew || moved > OA_LEVEL_STEP {
            r.action = OA_VIB;
            r.ms = if s.run_us > OA_TAP_MS * 1000 { OA_SEGMENT_MS } else { OA_TAP_MS };
            r.intensity = intensity;
            r.hz = hz;
            s.on = true;
            s.until_us = s.now_us.wrapping_add(r.ms * 1000);
            s.level = intensity;
            s.hz = hz;
        }
    } else {
        s.quiet_us += period_us;
        if s.native_taps && !s.on && s.run_us > 0 && s.run_us < OA_LONG_MS * 1000 && s.quiet_us >= OA_QUIET_MS * 1000 {
            r.action = OA_IMPACT;
            r.intensity = (s.short_peak * 1000 / 32768).clamp(1, 1000);
            s.short_peak = 0;
            s.sign = 0;
        }
        if s.quiet_us >= OA_QUIET_MS * 1000 {
            if s.on && s.run_us >= OA_LONG_MS * 1000 {
                r.action = OA_STOP;
                s.on = false;
            }
            s.run_us = 0;
            s.run_crossings = 0;
            if s.native_taps {
                s.short_peak = 0;
                s.sign = 0;
            }
        }
    }
    // a session of hours: keep the clock away from the 32-bit wrap
    if !s.on && s.now_us > 0x4000_0000 {
        s.now_us = 0;
        s.until_us = 0;
    }
    r
}
