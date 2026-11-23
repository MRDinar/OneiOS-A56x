//! `ioslog haptics`: the path of a haptic in one boot, from iOS's own log, step by step (2026-11-23: the phone boot
//! that was read as "the Actuator plays silence" had asked for no haptic at all: one prewarm, no feedback played, so
//! its zero periods said nothing about the DSP chain). The same counter and report as `ioslog health` (health.rs),
//! with its own table of kinds, in the order a haptic travels. The first kind whose count is 0 is where to look:
//!
//! - `feedback`: UIKit's feedback generators, by app: `asked` (a generator plays a feedback), then the engine that
//!   played it: `core haptics` (Core Haptics, on to the Actuator) or `system sound` (a sound, no haptic).
//! - `keyboard-mode`: the keyboard's feedback mode, by app: `haptics on`, `off in Settings` (iOS's default: Settings >
//!   Sounds & Haptics > Keyboard Feedback > Haptic) or `unsupported` (no closed-loop answer: no `/product/haptics`).
//! - `engine`: the app's Core Haptics engine: `made`, `prewarm`, `start`, `events` (an event array sent to the server)
//!   and `timed out` (the server did not answer: hosted 0078).
//! - `server`: audiomxd's haptic server: `up`, `prewarm`, `start` (a client started with haptics), `haptics muted`
//!   (an audio-only client), `preset missing` and `gave up` (hosted 0078's faults).
//! - `chain`: VirtualAudio's DSP chain of the Actuator's stream: `made`, `no strips file` (hosted 0086's fault) and
//!   `no volume command`.
//! - `level`: audiomxd's meter (RTAID) at the Actuator's nodes: `silent` (-120 dB) or `with a level`.
//! - `device`: the guest's Actuator device (the kernel's `ONEIOS HAPTIC:` lines): `opened`, `I/O started`, `stopped
//!   with requests` (an I/O run that sent requests to the motor) and `stopped silent` (a run with no loud period: a
//!   prewarm, or samples lost on the way).
//! - `system-sound`: the system sound server's actions: `with vibration` and `sound only`.
//!
//! How to read it: `feedback` 0 asked: nothing was asked for (touch a switch, press an icon long). `feedback` only
//! `system sound`, or `engine` with no `events`: UIKit took the sound path. `events` above 0 with `level` only
//! `silent` and `device` only `stopped silent`: the samples are lost between the synth and the device (the chain).
//! As in health, a count is a number of log lines.

use crate::health::{read_with, By, Kind, EXAMPLES, TOP};
use crate::How;
use anyhow::{Context, Result};
use std::path::PathBuf;

#[cfg(test)]
#[path = "haptics_tests.rs"]
mod tests;

/// The Actuator's nodes of audiomxd's meter: the HAL's output and the port's two sides of the DSP chain.
macro_rules! actuator_node {
    () => {
        r"RTAID \[.*node=(?:Actuator-Output|P(?:re|ost)DSP - \{ \[ type: pact)"
    };
}

/// The kinds, in the order a haptic travels.
pub const HAPTICS: &[Kind] = &[
    Kind {
        name: "feedback",
        what: "UIKit's feedback generators (`[com.apple.UIKit:Feedback]`), by app: `asked` a generator plays a feedback, `core haptics` it was played by a Core Haptics engine (on to the Actuator), `system sound` by the system sound engine (a sound, no haptic)",
        texts: &[
            ("asked", r"generator <\w+: 0x[0-9a-fA-F]+> playing feedback"),
            ("core haptics", r"played feedback .* with engine <_UIFeedbackCoreHaptics"),
            ("system sound", r"played feedback .* with engine <_UIFeedbackSystemSoundEngine"),
        ],
        by: By::Logger,
        names: "",
        who: &[],
        over: 0.0,
        texts_in_line: true,
    },
    Kind {
        name: "keyboard-mode",
        what: "the keyboard's feedback mode (`_UIKBFeedbackGenerator: ... Updating mode`), by app: `haptics on`, `off in Settings` (iOS's default: Settings > Sounds & Haptics > Keyboard Feedback > Haptic), `unsupported` (MobileGestalt has no closed-loop answer: no /product/haptics)",
        texts: &[
            ("haptics on", r"Updating mode\. Haptics: supported\. Haptics: enabled"),
            ("off in Settings", r"Updating mode\. Haptics: supported\. Haptics: disabled"),
            ("unsupported", r"Updating mode\. Haptics: unsupported"),
        ],
        by: By::Logger,
        names: "",
        who: &[],
        over: 0.0,
        texts_in_line: true,
    },
    Kind {
        name: "engine",
        what: "the app's Core Haptics engine (`[com.apple.corehaptics:...]`), by app: `made`, `prewarm`, `start`, `events` an event array sent to the server (a haptic to play), `timed out` the server did not answer the first call (hosted 0078)",
        texts: &[
            ("made", r"\[CHHapticEngine initWith[^\]]*\]: Creating engine"),
            ("prewarm", r"Prewarming the underlying Haptic Player"),
            ("start", r"Starting underlying Haptic Player"),
            ("events", r"sendEvents:withImmediateParameters:atTime:error:\]: sending event array"),
            ("timed out", r"Initial XPC call to server timed out"),
        ],
        by: By::Logger,
        names: "",
        who: &[],
        over: 0.0,
        texts_in_line: true,
    },
    Kind {
        name: "server",
        what: "audiomxd's haptic server (`[com.apple.coreaudio:hsrv]`, `hmut`): `up` its start, `prewarm` a client's prewarm, `start` a client started with haptics, `haptics muted` an audio-only client, `preset missing` and `gave up` the synth preset not found (hosted 0078)",
        texts: &[
            ("up", r"=== Initializing Haptic 2\.0 Engine ==="),
            ("prewarm", r"\[AVHapticServer doPrewarm:\]: Starting prewarm"),
            ("start", r"start: client ID: \S+ audio: \d haptic: 1"),
            ("haptics muted", r"Muting haptics on client ID"),
            ("preset missing", r"Unable to load the configuration plist"),
            ("gave up", r"server bailing out|Haptic Engine & Server Failure"),
        ],
        by: By::Logger,
        names: "",
        who: &[],
        over: 0.0,
        texts_in_line: true,
    },
    Kind {
        name: "chain",
        what: "VirtualAudio's DSP chain of the Actuator's stream, in audiomxd: `made` the virtual stream with its chain, `no strips file` the chain's settings not found (hosted 0086), `no volume command` the aggregate device has no software volume command for the Actuator's port",
        texts: &[
            ("made", r"Created virtual stream for \[vzzz\].* with DSP chain"),
            ("no strips file", r"Cannot find strips file \S*actuator"),
            ("no volume command", r"failed to fetch software volume command.*type: pact"),
        ],
        by: By::Logger,
        names: "",
        who: &[],
        over: 0.0,
        texts_in_line: true,
    },
    Kind {
        name: "level",
        what: "audiomxd's meter (RTAID) at the Actuator's nodes (`Actuator-Output`, the port's `PreDSP` and `PostDSP`): `silent` -120 dB, `with a level` anything above",
        texts: &[("silent", concat!(actuator_node!(), r".*rms:\[-120\.")), ("with a level", concat!(actuator_node!(), r".*rms:\[-?\d"))],
        by: By::Logger,
        names: "",
        who: &[],
        over: 0.0,
        texts_in_line: true,
    },
    Kind {
        name: "device",
        what: "the guest's Actuator device (the kernel's `ONEIOS HAPTIC:` lines): `opened` a user client, `I/O started`, `stopped with requests` an I/O run that sent requests to the motor, `stopped silent` a run with no loud period (a prewarm, or samples lost on the way)",
        texts: &[
            ("opened", r"ONEIOS HAPTIC: Actuator: user client \d+ opened"),
            ("I/O started", r"ONEIOS HAPTIC: Actuator: I/O started"),
            ("stopped with requests", r"ONEIOS HAPTIC: Actuator: I/O stopped: \d+ periods, [1-9]\d* loud"),
            ("stopped silent", r"ONEIOS HAPTIC: Actuator: I/O stopped: \d+ periods, 0 loud"),
        ],
        by: By::Logger,
        names: "",
        who: &[],
        over: 0.0,
        texts_in_line: true,
    },
    Kind {
        name: "system-sound",
        what: "the system sound server's actions in audiomxd (`SSSActionLists`: `mShouldVibe`): `with vibration` (the legacy vibrator's path or the Actuator), `sound only`",
        texts: &[("with vibration", r"mSSID \d+, .*mShouldVibe 1"), ("sound only", r"mSSID \d+, .*mShouldVibe 0")],
        by: By::Logger,
        names: "",
        who: &[],
        over: 0.0,
        texts_in_line: true,
    },
];

#[derive(clap::Args)]
pub struct Args {
    /// The Data store's image, a .logarchive folder, a folder with db/diagnostics, or a text file that `ioslog
    /// show` wrote. Without it: STAGE/data/data.img, the staged Data store (the form for the phone itself)
    source: Option<String>,
    #[command(flatten)]
    how: How,
    /// The boot to count: last (default), all, or the start of a boot UUID
    #[arg(long, default_value = "last")]
    boot: String,
    /// How many processes (and names) a kind's line lists
    #[arg(long, default_value_t = TOP)]
    top: usize,
    /// Under each kind: what it counts, the first and last time, each text's count, every process and name, examples
    #[arg(long)]
    detail: bool,
    /// How many example lines per kind (with --detail, and in --output's file)
    #[arg(long, default_value_t = EXAMPLES)]
    examples: usize,
    /// Also write the report with its details to FILE (an existing file is replaced)
    #[arg(short, long, value_name = "FILE")]
    output: Option<PathBuf>,
    /// Without SOURCE: the stage whose data/data.img is read
    #[arg(long, value_name = "DIR", default_value = "/data/local/tmp/oneios")]
    stage: String,
}

pub(crate) fn run(args: Args) -> Result<()> {
    let source = match &args.source {
        Some(source) => source.clone(),
        None => crate::health::staged(&args.stage)?,
    };
    let report = read_with(HAPTICS, &source, &args.how, &args.boot, args.examples)?;
    print!("{}", report.text(args.top, args.detail));
    if let Some(path) = &args.output {
        std::fs::write(path, report.text(args.top, true)).with_context(|| path.display().to_string())?;
        eprintln!("ioslog: the report with its details is in {}", path.display());
    }
    Ok(())
}
