// SPDX-License-Identifier: MIT
//! The Makefile's `java -cp build/host/modes TestModes` as a cargo test: the choice among the panel's modes and the
//! Java half's names for the buffers and dataspaces (tests/TestModes.java against java/OneiOSDisplay.java) are plain
//! Java, run on the host's JVM. Its values are the library's (src/xhd: buffer_for, dataspace_parse), checked here as
//! well. Skips with a reason when there is no javac or java ($JDK/bin first when JDK is set, as build-android.sh does).
use oneios_display::xhd;
use std::path::{Path, PathBuf};
use std::process::Command;

fn tool(name: &str) -> PathBuf {
    match std::env::var_os("JDK") {
        Some(jdk) if !jdk.is_empty() => Path::new(&jdk).join("bin").join(name),
        _ => PathBuf::from(name),
    }
}

#[test]
fn java_modes() {
    let here = Path::new(env!("CARGO_MANIFEST_DIR"));
    let out = Path::new(env!("CARGO_TARGET_TMPDIR")).join("modes");
    let _ = std::fs::remove_dir_all(&out);
    std::fs::create_dir_all(&out).unwrap();
    let javac = Command::new(tool("javac"))
        .args(["--release", "11", "-Xlint:all", "-Werror", "-d"])
        .arg(&out)
        .arg(here.join("java/OneiOSDisplay.java"))
        .arg(here.join("tests/TestModes.java"))
        .status();
    let Ok(javac) = javac else {
        println!("skipped: no javac (a JDK on PATH, or JDK=...)");
        return;
    };
    assert!(javac.success(), "javac failed");
    let run = Command::new(tool("java")).arg("-cp").arg(&out).arg("TestModes").output().expect("java");
    let text = String::from_utf8_lossy(&run.stdout);
    print!("{text}");
    assert!(run.status.success() && text.contains(" checks, 0 failed"), "TestModes failed");
    // The same values on the native side: what TestModes expects of the Java half is what the library says.
    assert_eq!(xhd::buffer_for(1).map(|b| b.0), Some(5));
    assert_eq!(xhd::buffer_for(2).map(|b| b.0), Some(22));
    assert_eq!(xhd::buffer_for(3), None);
    assert_eq!(xhd::dataspace_parse("auto"), Some(-1));
    assert_eq!(xhd::dataspace_parse("srgb"), Some(142671872));
    assert_eq!(xhd::dataspace_parse("p3"), Some(143261696));
    assert_eq!(xhd::dataspace_parse("scrgb-linear"), Some(406913024));
    assert_eq!(xhd::dataspace_parse("rec709"), None);
}
