//! `ioslog keyboard`: the keyboard's path in one boot, from iOS's own log (the owner's request, 2026-11-22: typing
//! lags, a key comes out twice, the keyboard goes away in Spotlight, keys do nothing in the captive portal's sheet).
//! The same counter and report as `ioslog health` (health.rs), with its own table of kinds:
//!
//! - `key-taps`: UIKit's `KeyboardTouch` `touch down` and `touch up`, by the app that had the keyboard. More ups than
//!   downs in one app is the first thing to look at for doubled keys.
//! - `hangs`: HangTracer's main-thread hangs (as in health): a key waits for the app's main thread.
//! - `asset-lock`, `asset-query`: MobileAsset's short-term lock of an asset set that failed (errno 2: the set's
//!   `atomic_instance_latest.locker` link is not there), and the UnifiedAssetFramework queries that ask for it; by the
//!   process, the names are the asset sets (kbd asks for com.apple.linguisticdata again and again).
//! - `language-model`: the keyboard's and Spotlight's language models that are not there yet or did not load.
//! - `kb-screen`: UIKit's `Keyboard screen not found for window`: a keyboard placed for no screen.
//! - `kb-placement`: the input view set placed again (`endPlacementForInputViewSet`) and the keyboard scene's geometry
//!   sent again (`Notifying observers of geometry update`): many per tap is the keyboard laid out again and again.
//! - `text-input`: TextInputUI's session requests (`TUIInputSessionProxy handleRequest`, the keyboard input manager's
//!   forwarded calls): the app's side of each key.
//!
//! As in health, a count is a number of log lines; compare it with the same count of another session.
//! The phone's log times can run slow against the wall (the guest's clock): read orders and counts, not gaps.

use crate::health::{read_with, By, Kind, EXAMPLES, TOP};
use crate::How;
use anyhow::{Context, Result};
use std::path::PathBuf;

#[cfg(test)]
#[path = "keyboard_tests.rs"]
mod tests;

/// The kinds, in the order of the report.
pub const KEYBOARD: &[Kind] = &[
    Kind {
        name: "key-taps",
        what: "UIKit's keyboard touches (`[com.apple.UIKit:KeyboardTouch] touch down` / `touch up`), by the app that had the keyboard; more ups than downs is where doubled keys start",
        texts: &[("down", r"^touch down\b"), ("up", r"^touch up\b")],
        by: By::Logger,
        names: "",
        who: &[],
        over: 0.0,
    },
    Kind {
        name: "hangs",
        what: "HangTracer's `Hang detected: N s`: a main thread that did not answer for N s (keys wait for it), by the process that hung",
        texts: &[("Hang detected", r"Hang detected: (?P<s>[0-9]+(?:\.[0-9]+)?) ?s\b")],
        by: By::Logger,
        names: "",
        who: &[],
        over: 1.0,
    },
    Kind {
        name: "asset-lock",
        what: "MobileAsset's short-term shared lock of an asset set that failed (`_shortTermOpenSharedLockFile} (SYMLINK) ... unable to obtain shared lock`, errno 2: the set's atomic_instance_latest.locker is not there), by the process that asked; the names are the asset sets",
        texts: &[("shared lock", r"_shortTermOpenSharedLockFile\}.*lock of (?P<who>[A-Za-z0-9_.-]+) for")],
        by: By::Logger,
        names: "asset sets",
        who: &[],
        over: 0.0,
    },
    Kind {
        name: "asset-query",
        what: "UnifiedAssetFramework's asset queries (`UAF.retrieveAssetSetWithToken`, `assetsForQuery: <query: NAME`), by the process that asked; the names are the asset types",
        texts: &[("retrieve", r"UAF\.retrieveAssetSetWithToken"), ("query", r"assetsForQuery: <query: (?P<who>[A-Za-z0-9_.-]+)")],
        by: By::Logger,
        names: "asset types",
        who: &[],
        over: 0.0,
    },
    Kind {
        name: "language-model",
        what: "language models that are not there yet or did not load (`LM model is not valid yet`, `Unable to load ... model`, `embedding generation timeout`), by the process",
        texts: &[
            ("not valid", r"LM model is not valid yet"),
            ("unable to load", r"Unable to load .*model"),
            ("embedding timeout", r"embedding generation timeout"),
        ],
        by: By::Logger,
        names: "",
        who: &[],
        over: 0.0,
    },
    Kind {
        name: "kb-screen",
        what: "UIKit's `Keyboard screen not found for window`: the keyboard placed for no screen, by the app",
        texts: &[("not found", r"Keyboard screen not found for window")],
        by: By::Logger,
        names: "",
        who: &[],
        over: 0.0,
    },
    Kind {
        name: "kb-placement",
        what: "the keyboard laid out again: `endPlacementForInputViewSet` (the input view set placed) and `Notifying observers of geometry update` (the keyboard scene's geometry sent), by the app",
        texts: &[("placement", r"endPlacementForInputViewSet"), ("geometry", r"Notifying observers of geometry update")],
        by: By::Logger,
        names: "",
        who: &[],
        over: 0.0,
    },
    Kind {
        name: "text-input",
        what: "TextInputUI's requests of the app's input session (`TUIInputSessionProxy handleRequest`, `TUIKeyboardInputManagerMux forwardInvocation`), by the app",
        texts: &[("session request", r"TUIInputSessionProxy handleRequest"), ("forwarded", r"TUIKeyboardInputManagerMux forwardInvocation")],
        by: By::Logger,
        names: "",
        who: &[],
        over: 0.0,
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
    let report = read_with(KEYBOARD, &source, &args.how, &args.boot, args.examples)?;
    print!("{}", report.text(args.top, args.detail));
    if let Some(path) = &args.output {
        std::fs::write(path, report.text(args.top, true)).with_context(|| path.display().to_string())?;
        eprintln!("ioslog: the report with its details is in {}", path.display());
    }
    Ok(())
}
