// SPDX-License-Identifier: MIT
// tests/test_xnuhost.c's test_config and test_host_device: the settings, the image set's xnuhost.conf under the
// command line, ram=auto and what fits, the display's mode, the boot arguments' helpers, the host phone's layer.

use super::*;
use std::path::PathBuf;

const M: u64 = 1 << 20;
const G: u64 = 1 << 30;
const PHONE: u64 = 11_746_000 << 10; // a 12 GB phone's MemTotal (about 11.2 GiB)

/// A fresh folder of its own for one test.
fn testdir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("xnuhost-config-{}-{name}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("test folder");
    dir
}

fn write(path: impl AsRef<Path>, text: &str) {
    fs::write(path, text).expect("test file");
}

/// An image set with a kernel, two volumes and a Data seed (one byte each).
fn image_set(dir: &Path, name: &str) -> String {
    let image = dir.join(name);
    fs::create_dir_all(&image).unwrap();
    for f in ["kernel.kc", "os.img", "systemos.img", "data.seed"] {
        write(image.join(f), "x");
    }
    image.to_str().unwrap().to_owned()
}

fn argv(args: &[&str]) -> Vec<String> {
    std::iter::once("xnuhost").chain(args.iter().copied()).map(str::to_owned).collect()
}

/// xh_config_args with no environment.
fn args(c: &mut Config, a: &[&str]) -> Result<(), ConfigError> {
    c.args_with_env(&argv(a), |_| None)
}

/// The command line, finished, with the lock's answer `can_lock`.
fn run_lock(a: &[&str], can_lock: bool) -> Result<Config, ConfigError> {
    let mut c = Config::new();
    args(&mut c, a)?;
    c.finish_with(&|| can_lock)?;
    Ok(c)
}

/// As on the PC: no CAP_IPC_LOCK, RLIMIT_MEMLOCK 8 MiB.
fn run(a: &[&str]) -> Result<Config, ConfigError> {
    run_lock(a, false)
}

#[test]
fn settings_and_sizes() {
    // Explicit, disabled-by-default touchscreen exclusivity is not confused with the existing input= boolean. Live
    // EVIOCGRAB requires the phone.
    let mut c = Config::new();
    assert!(!c.touch_exclusive && c.input);
    assert!(c.set("touch-exclusive", "1").is_ok() && c.touch_exclusive);
    assert!(c.set("touch-exclusive", "wrong").is_err());
    assert!(c.set("touch-exclusive", "0").is_ok() && !c.touch_exclusive);
    assert_eq!(parse_size("4G"), Some(4 << 30));
    assert_eq!(parse_size("512m"), Some(512 << 20));
    assert_eq!(parse_size("0x1000"), Some(4096));
    assert_eq!(parse_size("12q"), None);
    assert_eq!(parse_size("010"), Some(8)); // strtoull's base 0: octal
    assert_eq!(parse_size(""), None);
    let mut c = Config::new();
    assert!(
        c.set("ram-giveback", "2").is_err() && c.set("ram-lock", "x").is_err() && c.set("ram-prefault", "").is_err()
    );
    assert!(c.set("ram-session-reserve", "lots").is_err() && c.set("ram-unlock-below", "2T").is_err());
    assert!(c.set("ram-fit", "x").is_err() && c.set("ram-pagetable-percent", "51").is_err());
    assert!(c.set("ram-gpu-reserve", "?").is_err());
    assert!(c.set("ram-percent", "5").is_err() && c.set("ram-percent", "101").is_err() && c.set("ram", "0").is_err());
    assert!(
        c.set("ram", "lots").is_err() && c.set("ram-host-floor", "x").is_err() && c.set("host-mem-log", "-1").is_err()
    );
    assert!(c.set("ram-huge", "0").is_ok() && !c.ram_huge && c.set("ram-huge", "x").is_err());
    assert!(c.set("haptics", "off").is_ok() && c.haptics == Haptics::Off && c.set("haptics", "evdev").is_err());
    assert!(c.set("encoder", "sim").is_ok() && c.encoder == Encoder::Sim);
    assert!(c.set("encoder", "off").is_ok() && c.encoder == Encoder::Off && c.set("encoder", "mediacodec").is_err());
    assert!(c.set("touch-sleep", "0").is_ok() && !c.touch_sleep && c.set("touch-sleep", "maybe").is_err());
    assert!(c.set("haptics-native-impacts", "maybe").is_err());
    assert!(c.set("cpu-pin", "2").is_ok() && c.cpu_pin == CpuPin::Always);
    assert!(c.set("cpu-pin", "0").is_ok() && c.cpu_pin == CpuPin::Off && c.set("cpu-pin", "3").is_err());
    assert!(
        c.set("clusters", "EEPPPPPP").is_ok()
            && c.set("clusters", "EEPPPPPPP").is_err()
            && c.set("clusters", "EX").is_err()
    );
    let mut c = Config::new();
    assert!(c.set("pen-mirror", "maybe").is_err() && !c.given("pen-mirror")); // a bad value refused
    assert!(c.set("encoder", "mediacodec").is_err() && !c.given("encoder") && c.encoder == Encoder::Off);
    // display-format: the two names, nothing else
    let mut f = Config::new();
    assert!(f.set("display-format", "rgba16f").is_ok() && f.display_format == DisplayFormat::Rgba16f);
    assert_eq!(f.display_format.pixels(), 2);
    assert!(f.set("display-format", "bgrx").is_ok() && f.display_format == DisplayFormat::Bgrx8);
    assert!(f.set("display-format", "rgb565").is_err());
    // ram-view=
    let mut vc = Config::new();
    assert!(vc.ram_view == 0 && vc.set("ram-view", "12G").is_ok() && vc.ram_view == 12 << 30 && vc.given("ram-view"));
    assert!(vc.set("ram-view", "off").is_ok() && vc.ram_view == 0);
    assert!(vc.set("ram-view", "auto").is_ok() && vc.ram_view == 12 << 30);
    assert!(vc.set("ram-view", "lots").is_err());
    // disks
    let mut d = Config::new();
    assert!(
        d.set("disk", "a.img").is_ok() && d.set("disk", "a.img,seed").is_err() && d.set("disk-rw", ",seed").is_err()
    );
    assert!(d.set("data-store", "data.img,seed.img").is_ok());
    assert_eq!(
        d.disks[1],
        DiskConfig { path: "data.img".into(), writable: true, data_store: true, seed: "seed.img".into() }
    );
    for _ in 0..4 {
        assert!(d.set("disk-rw", "x").is_ok());
    }
    assert!(d.set("disk", "one-too-many").is_err());
    // unknown keys
    assert!(Config::new().set("no-such-key", "1").is_err());
}

#[test]
fn command_line_and_image_set() {
    let dir = testdir("cmdline");
    let dirs = dir.to_str().unwrap();
    let image = image_set(&dir, "image");
    let path = format!("{image}/xnuhost.conf");
    // the OneOS fork's command line (oneos/interface.md, section 4)
    let fork = [
        "--session-abi",
        "1",
        "--mode",
        "takeover",
        "--runtime",
        "/data/runtime",
        "--image",
        &image,
        "--data",
        dirs,
        "--session",
        dirs,
    ];
    let c = run(&fork).unwrap();
    assert!(c.kernel.ends_with("/image/kernel.kc") && c.disks.len() == 4);
    assert!(c.disks[3].path.contains("data.seed") && !c.disks[3].writable && !c.disks[0].writable);
    assert!(c.disks[1].path.contains("systemos.img"));
    assert!(c.disks[2].data_store && c.disks[2].writable && c.disks[2].path.ends_with("/data.img"));
    assert!(c.disks[2].seed.contains("data.seed"));
    assert!(c.ncpu >= 1 && c.ncpu as usize <= MAX_CPUS && c.display == DisplayBackend::Shm && c.input && c.tasks);
    assert!(c.bridges && c.bluetooth == Bluetooth::Auto && c.wifi == Wifi::Auto); // on in a session
    assert!(c.touch_sleep);
    // The exclusive physical touchscreen may only be grabbed in a non-takeover bench using the SHM display and no
    // hardware bridges. All other modes fail.
    let c = run(&[
        "--image",
        &image,
        "--mode",
        "bench",
        "--display",
        "shm",
        "--set",
        "bridges=0",
        "--set",
        "touch-exclusive=1",
    ])
    .unwrap();
    assert!(c.touch_exclusive && c.input && !c.bridges && c.display == DisplayBackend::Shm);
    assert!(run(&["--image", &image, "--set", "touch-exclusive=1"]).is_err());
    let no_input =
        ["--image", &image, "--mode", "bench", "--set", "bridges=0", "--set", "touch-exclusive=1", "--no-input"];
    assert!(run(&no_input).is_err());
    let bad_display =
        ["--image", &image, "--mode", "bench", "--display", "none", "--set", "bridges=0", "--set", "touch-exclusive=1"];
    assert!(run(&bad_display).is_err());
    let bridges =
        ["--image", &image, "--mode", "bench", "--display", "shm", "--set", "bridges=1", "--set", "touch-exclusive=1"];
    assert!(run(&bridges).is_err());
    // the image set's own settings, and what the command line overrides
    write(&path, "# settings of this image\nboot-args=-v debug=0x8\n");
    let c = run(&["--image", &image, "--cpus", "6", "--ram", "6G", "--set", "display=none", "--no-input"]).unwrap();
    assert!(c.boot_args == "-v debug=0x8" && c.ncpu == 6 && c.ram_bytes == 6 << 30);
    assert!(c.display == DisplayBackend::None && !c.input && c.disks.len() == 3);
    // Bluetooth, haptics, the encoder, Wi-Fi from the command line
    let bench_hidl = ["--image", &image, "--mode", "bench", "--bluetooth", "hidl"];
    let c = run(&bench_hidl).unwrap();
    assert!(!c.bridges && c.bluetooth == Bluetooth::Hidl);
    assert_eq!(run(&["--image", &image, "--set", "bluetooth=virtual"]).unwrap().bluetooth, Bluetooth::Virtual);
    let c = run(&["--image", &image, "--mode", "bench", "--set", "bluetooth=sim"]).unwrap(); // the PC's stand-in
    assert!(!c.bridges && c.bluetooth == Bluetooth::Sim);
    assert_eq!(Config::new().haptics, Haptics::Auto);
    let c = run(&["--image", &image, "--mode", "bench", "--set", "haptics=sim"]).unwrap(); // the PC's stand-in actuator
    assert!(!c.bridges && c.haptics == Haptics::Sim);
    // encoder: off by default; auto (the phone's MediaCodec in a session, the stand-in with the bridges off); sim (the
    // stand-in always); anything else refused.
    assert_eq!(Config::new().encoder, Encoder::Off);
    let c = run(&["--image", &image, "--mode", "bench", "--set", "encoder=auto"]).unwrap(); // a PC boot
    assert!(!c.bridges && c.encoder == Encoder::Auto);
    assert_eq!(run(&["--image", &image, "--set", "wifi=off"]).unwrap().wifi, Wifi::Off);
    assert_eq!(run(&["--image", &image, "--set", "wifi=sim"]).unwrap().wifi, Wifi::Sim);
    // refused
    assert!(args(&mut Config::new(), &["--image", &image, "--set", "wifi=nat"]).is_err());
    assert!(args(&mut Config::new(), &["--image", &image, "--bluetooth", "usb"]).is_err());
    let e = run(&["--image", &image, "--session-abi", "2"]).unwrap_err();
    assert_eq!(e.lines, ["session ABI 2 is not supported (this build: 1)"]);
    assert!(args(&mut Config::new(), &["--image", &image, "--cpus", "9"]).is_err());
    assert!(args(&mut Config::new(), &["--image", &image, "--scale", "4"]).is_err());
    assert!(args(&mut Config::new(), &["--image", &image, "--hz", "500"]).is_err());
    assert!(args(&mut Config::new(), &["--image", &image, "--text-fixup", "file,10,aa"]).is_err());
    let e = run(&["--image", dirs]).unwrap_err(); // no kernel there
    assert_eq!(e.lines, [format!("no kernel.kc in {dirs} (and no --kernel)")]);
    // what the command line says when it is wrong, as C said it
    let e = args(&mut Config::new(), &["--image"]).unwrap_err();
    assert!(e.usage && e.lines == ["unknown or incomplete argument --image"]);
    let e = args(&mut Config::new(), &["--set", "novalue"]).unwrap_err();
    assert_eq!(e.lines, ["--set takes KEY=VALUE"]);
    let e = args(&mut Config::new(), &["--cpus", "x"]).unwrap_err();
    assert_eq!(e.lines, ["bad value for --cpus"]);
    let e = args(&mut Config::new(), &["--set", "cpus=x"]).unwrap_err();
    assert_eq!(e.lines, ["bad setting cpus"]);
    let e = args(&mut Config::new(), &["-h"]).unwrap_err();
    assert!(e.usage && e.lines.is_empty());
    let e = run(&["--image", &image, "--mode", "shell"]).unwrap_err();
    assert_eq!(e.lines, ["mode shell is not supported"]);
    let e = run(&["--image", &image, "--display", "vnc"]).unwrap_err();
    assert_eq!(e.lines, ["display vnc is not supported"]);
    let e = run(&[]).unwrap_err();
    assert_eq!(e.lines, ["--image DIR is required"]);
    // a bad line of a --config file: both lines, as C printed them
    let bad = format!("{dirs}/bad.conf");
    write(&bad, "cpus=6\nwidth=1\n\n# c\ncpus=99\n");
    let e = args(&mut Config::new(), &["--config", &bad]).unwrap_err();
    assert_eq!(e.lines, [format!("{bad}:5: bad setting cpus"), format!("cannot use {bad}")]);
    let mut c = Config::new();
    write(&bad, "  cpus=3  \n");
    assert!(args(&mut c, &["--config", &bad]).is_ok() && c.ncpu == 3);
    // the environment (ONEIOS_*) first, then argv
    let mut c = Config::new();
    let env = |name: &str| match name {
        "ONEIOS_MODE" => Some("bench".to_owned()),
        "ONEIOS_IMAGE" => Some(image.clone()),
        "ONEIOS_DATA" => Some(String::new()), // empty: not used
        _ => None,
    };
    assert!(c.args_with_env(&argv(&["--mode", "takeover"]), env).is_ok());
    assert!(c.mode == Mode::Takeover && c.image == image && c.data.is_empty());
    let e = Config::new().args_with_env(&argv(&[]), |n| (n == "ONEIOS_SESSION_ABI").then(|| "1".repeat(600))).err();
    assert!(e.is_none()); // session-abi takes any text (atoi), refused at the finish
    let e = Config::new().args_with_env(&argv(&[]), |n| (n == "ONEIOS_IMAGE").then(|| "x".repeat(600))).unwrap_err();
    assert_eq!(e.lines, ["bad ONEIOS_IMAGE"]);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn image_set_settings() {
    let dir = testdir("imageset");
    let image = image_set(&dir, "image");
    let path = format!("{image}/xnuhost.conf");
    let meminfo = dir.join("meminfo");
    write(&meminfo, "MemTotal:       11746000 kB\nMemFree:          401234 kB\nMemAvailable:    7340032 kB\nBuffers:            1234 kB\n");
    let set_meminfo = format!("ram-meminfo={}", meminfo.display());
    let img1 = ["--image", image.as_str(), "--mode", "takeover"];
    // The image set's bridges=, wifi=, bluetooth=, haptics= apply when the command line has none (they were read, then
    // ignored)
    write(&path, &format!("boot-args=-v debug=0x8\nbridges=0\nwifi=sim\nbluetooth=sim\nhaptics=sim\n{set_meminfo}\n"));
    let c = run(&img1).unwrap();
    assert!(!c.bridges && c.wifi == Wifi::Sim && c.bluetooth == Bluetooth::Sim && c.haptics == Haptics::Sim);
    let c = run(&[&img1[..], &["--set", "bridges=1", "--set", "wifi=auto", "--set", "haptics=off"]].concat()).unwrap();
    assert!(c.bridges && c.wifi == Wifi::Auto && c.bluetooth == Bluetooth::Sim && c.haptics == Haptics::Off); // the command line wins, auto too
                                                                                                              // The image set's pen=, pen-rotate=, pen-mirror= apply too (until 2026-11-22 they were read, then dropped: the
                                                                                                              // phone's pen=auto never turned the pen on); the command line wins.
    write(&path, &format!("boot-args=-v debug=0x8\npen=auto\npen-rotate=90\npen-mirror=1\n{set_meminfo}\n"));
    let c = run(&img1).unwrap();
    assert!(c.pen == Pen::Auto && c.pen_rotate == PenRotate::R90 && c.pen_rotate.degrees() == Some(90) && c.pen_mirror);
    let c =
        run(&[&img1[..], &["--set", "pen=off", "--set", "pen-rotate=auto", "--set", "pen-mirror=0"]].concat()).unwrap();
    assert!(c.pen == Pen::Off && c.pen_rotate == PenRotate::Auto && !c.pen_mirror);
    // The image set's encoder= applies (until 2026-11-22 it was read, then dropped: encoder=auto in the phone's
    // xnuhost.conf never gave the guest its encoder); the command line wins.
    write(&path, &format!("boot-args=-v debug=0x8\nencoder=auto\n{set_meminfo}\n"));
    assert_eq!(run(&img1).unwrap().encoder, Encoder::Auto);
    assert_eq!(run(&[&img1[..], &["--set", "encoder=off"]].concat()).unwrap().encoder, Encoder::Off);
    // hw-backend: samsung by default for every bridge; xnuhost.conf, --set and --hw-backend*; the command line wins per
    // setting over the image set's file; a bad value or an unknown bridge refused.
    let c = Config::new();
    for b in Bridge::ALL {
        assert_eq!(c.hw.choose(b, Backend::Unset).0, Backend::Samsung);
    }
    assert!(!c.hw.fallback_allowed());
    write(&path, "boot-args=-v debug=0x8\nhw-backend=treble\nhw-backend-keys=samsung\nhw-backend-fallback=1\n");
    let c = run(&["--image", &image, "--hw-backend-battery", "samsung", "--set", "hw-backend-fallback=0"]).unwrap();
    assert_eq!(c.hw.choose(Bridge::Backlight, Backend::Unset).0, Backend::Treble);
    assert_eq!(c.hw.choose(Bridge::Keys, Backend::Unset).0, Backend::Samsung);
    assert!(c.hw.choose(Bridge::Battery, Backend::Unset).0 == Backend::Samsung && !c.hw.fallback_allowed());
    let c = run(&["--image", &image, "--hw-backend", "samsung"]).unwrap();
    assert_eq!(c.hw.choose(Bridge::Thermal, Backend::Unset).0, Backend::Samsung);
    assert!(c.hw.choose(Bridge::Keys, Backend::Unset).0 == Backend::Samsung && c.hw.fallback_allowed());
    assert!(args(&mut Config::new(), &["--image", &image, "--hw-backend", "pixel"]).is_err());
    assert!(args(&mut Config::new(), &["--image", &image, "--set", "hw-backend-toaster=treble"]).is_err());
    // the panel's fallback from the image set's xnuhost.conf; the command line's wins
    write(&path, "# settings of this image\nboot-args=-v debug=0x8\npanel-fallback=1080x2340@120\n");
    let c = run(&["--image", &image]).unwrap();
    assert_eq!(c.panel_fallback, Some(PanelMode { width: 1080, height: 2340, hz: 120 }));
    let c = run(&["--image", &image, "--set", "panel-fallback=1260x2800@60"]).unwrap();
    assert_eq!(c.panel_fallback, Some(PanelMode { width: 1260, height: 2800, hz: 60 }));
    // boot-args-add (YumikoHayamato/OneiOS 440abb2): added to the boot arguments, whichever gave them, each unless they
    // name it; the command line's wins over the image set's.
    write(&path, "# settings of this image\nboot-args=-v debug=0x8\nboot-args-add=oneios-disable-jobs=a,b -x\n");
    assert_eq!(run(&["--image", &image]).unwrap().boot_args, "-v debug=0x8 oneios-disable-jobs=a,b -x");
    let c = run(&["--image", &image, "--set", "boot-args-add=oneios-disable-jobs=c"]).unwrap();
    assert_eq!(c.boot_args, "-v debug=0x8 oneios-disable-jobs=c");
    let c = run(&["--image", &image, "--set", "boot-args=-x oneios-disable-jobs=z"]).unwrap();
    assert_eq!(c.boot_args, "-x oneios-disable-jobs=z"); // both named already
    write(&path, "boot-args-add=oneios-disable-jobs=a,b\n");
    let c = run(&["--image", &image]).unwrap();
    assert!(c.boot_args.contains("vgpu-3d=1 ") && c.boot_args.ends_with(" oneios-disable-jobs=a,b")); // after the default
    assert!(c.boot_args.starts_with(DEFAULT_BOOT_ARGS));
    let long = format!("boot-args=-{}", "a".repeat(999));
    let e = run(&["--image", &image, "--set", &long]).unwrap_err(); // 1000 + 24 do not fit
    assert_eq!(e.lines, ["boot-args-add does not fit beside the guest boot args (1023 bytes at most)"]);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn native_impacts() {
    // haptics-native-impacts (docs/hardware.md 5): unset by default (the environment decides, no boot argument added);
    // =1 in the image set's xnuhost.conf, the OneOS fork's command line (which passes no environment) turns on both
    // halves: the HAL's presets and oneios_native_impacts=1 after the default boot arguments, kept whole.
    let dir = testdir("impacts");
    let dirs = dir.to_str().unwrap();
    let image = image_set(&dir, "image");
    let path = format!("{image}/xnuhost.conf");
    let fork = [
        "--session-abi",
        "1",
        "--mode",
        "takeover",
        "--runtime",
        "/data/runtime",
        "--image",
        &image,
        "--data",
        dirs,
        "--session",
        dirs,
        "--ram",
        "4G",
    ];
    write(&path, "hw-backend-haptics=treble\n");
    let c = run(&fork).unwrap();
    assert!(c.haptics_native_impacts.is_none() && !c.boot_args.contains("oneios_native_impacts"));
    write(&path, "haptics-native-impacts=1\nhw-backend-haptics=treble\nhw-backend-fallback=0\n");
    let c = run(&fork).unwrap();
    assert_eq!(c.haptics_native_impacts, Some(true));
    assert_eq!(c.hw.choose(Bridge::Haptics, Backend::Unset).0, Backend::Treble);
    assert!(c.boot_args.contains("native-source-storage=1 ") && c.boot_args.ends_with(" oneios_native_impacts=1"));
    assert_eq!(c.boot_args.matches("oneios_native_impacts").count(), 1);
    // the image set's own boot arguments: kept, and an oneios_native_impacts=1 already there not doubled
    write(
        &path,
        "haptics-native-impacts=1\nhw-backend-haptics=treble\nboot-args=-v oneios_native_impacts=1 debug=0x8\n",
    );
    assert_eq!(run(&fork).unwrap().boot_args, "-v oneios_native_impacts=1 debug=0x8");
    write(&path, "haptics-native-impacts=1\nhw-backend-haptics=treble\nboot-args=-v debug=0x8\n");
    assert_eq!(run(&fork).unwrap().boot_args, "-v debug=0x8 oneios_native_impacts=1");
    // refused: the guest told otherwise (=0, a longer value, twice), the evdev backend, a fallback to it, no motor bridge
    for refused in [
        "haptics-native-impacts=1\nhw-backend-haptics=treble\nboot-args=-v oneios_native_impacts=0\n",
        "haptics-native-impacts=1\nhw-backend-haptics=treble\nboot-args=-v oneios_native_impacts=10\n",
        "haptics-native-impacts=1\nhw-backend-haptics=treble\nboot-args=oneios_native_impacts=1 oneios_native_impacts=1\n",
        "haptics-native-impacts=1\nhw-backend-haptics=treble\nhw-backend-fallback=1\n",
        "haptics-native-impacts=1\nhw-backend-haptics=samsung\n",
        "haptics-native-impacts=1\n",
    ] {
        write(&path, refused);
        assert!(run(&fork).is_err(), "{refused}");
    }
    // no motor bridge: haptics=sim or off (command line), or bench mode (the bridges off unless asked for)
    write(&path, "haptics-native-impacts=1\nhw-backend-haptics=treble\n");
    assert!(run(&["--image", &image, "--ram", "4G", "--set", "haptics=sim"]).is_err());
    assert!(run(&["--image", &image, "--ram", "4G", "--set", "haptics=off"]).is_err());
    assert!(run(&["--image", &image, "--ram", "4G", "--mode", "bench"]).is_err());
    // the command line's =0 wins over the image set's =1: off, no boot argument
    let c = run(&["--image", &image, "--ram", "4G", "--set", "haptics-native-impacts=0"]).unwrap();
    assert!(c.haptics_native_impacts == Some(false) && !c.boot_args.contains("oneios_native_impacts"));
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn ram_arithmetic() {
    // ram=auto (docs/booter.md 1): min(90% of MemTotal, MemTotal - 1.5 GiB) in 256 MiB steps, at least 1 GiB, at most
    // 64 GiB. 11,746,000 kB stands for a 12 GB phone's MemTotal (about 11.2 GiB; the phone's own number is read at the
    // start).
    assert_eq!(ram_auto(PHONE, 90, RAM_AUTO_FLOOR), 9728 * M); // the floor decides: 9.5 GiB
    assert_eq!(ram_auto(PHONE, 50, RAM_AUTO_FLOOR), 5632 * M); // the share decides
    assert_eq!(ram_auto(PHONE, 90, 4 * G), 7168 * M);
    assert!(ram_auto(64 * G, 90, RAM_AUTO_FLOOR) == 58880 * M && ram_auto(128 * G, 90, RAM_AUTO_FLOOR) == RAM_MAX);
    assert!(ram_auto(2560 * M, 90, RAM_AUTO_FLOOR) == G && ram_auto(2 * G, 90, RAM_AUTO_FLOOR) == 0);
    assert_eq!(ram_auto(G, 100, 0), G);
    assert!(ram_auto(RAM_AUTO_FLOOR, 90, RAM_AUTO_FLOOR) == 0 && ram_auto(0, 90, 0) == 0);
    // What fits (2026-11-14): G + 8% of G + 256 MiB (GPU) + 1 GiB (headroom) <= MemAvailable at the start.
    let (r, gr, pt) = (RAM_SESSION_RESERVE, RAM_GPU_RESERVE, RAM_PAGETABLE_PERCENT);
    assert_eq!(ram_fit(7 * G, r, gr, pt) / M, 5451); // (7168 - 1280) / 1.08
    assert!(
        ram_fit(r + gr + 108 * M, r, gr, pt) == 100 * M
            && ram_fit(r + gr, r, gr, pt) == 0
            && ram_fit(0, r, gr, pt) == 0
    );
    assert_eq!(ram_fit(6 * G, 0, 0, 0), 6 * G);
    let floor = RAM_AUTO_FLOOR;
    assert_eq!(ram_fit_auto(PHONE, 90, floor, Some(ram_fit(7 * G, r, gr, pt))), (5376 * M, Some(RamBound::Available)));
    assert_eq!(ram_fit_auto(PHONE, 90, floor, None), (9728 * M, Some(RamBound::Floor))); // ram-fit=0
    assert_eq!(ram_fit_auto(PHONE, 90, floor, Some(10000 * M)), (9728 * M, Some(RamBound::Floor))); // fits: the floor
    assert_eq!(ram_fit_auto(PHONE, 50, floor, Some(10000 * M)), (5632 * M, Some(RamBound::Share)));
    assert_eq!(ram_fit_auto(PHONE, 90, floor, Some(1279 * M)).0, G);
    assert_eq!(ram_fit_auto(PHONE, 90, floor, Some(1023 * M)), (0, None));
    // The phone of 2026-11-14 (MemTotal 11,084 MiB): the old ram=auto gave 9472 MiB and Linux swapped 2126 MiB of it.
    // What fits for a start MemAvailable of 8.5 GiB [assumed: that session's start line is not in the notes] is 6656.
    assert_eq!(ram_auto(11084 * M, 90, floor), 9472 * M);
    assert_eq!(ram_fit_auto(11084 * M, 90, floor, Some(ram_fit(8704 * M, r, gr, pt))).0, 6656 * M);
    // The host swaps the guest's RAM: a line once past 64 MiB, then at each further 256 MiB (320, 576, ...).
    let mut next = 0;
    assert!(
        !swap_note(10 * M, &mut next)
            && !swap_note(64 * M, &mut next)
            && swap_note(65 * M, &mut next)
            && next == 320 * M
    );
    assert!(!swap_note(300 * M, &mut next) && !swap_note(320 * M, &mut next) && swap_note(321 * M, &mut next));
    assert_eq!(next, 576 * M);
    assert!(
        swap_note(2126 * M, &mut next)
            && next == 2368 * M
            && !swap_note(2200 * M, &mut next)
            && !swap_note(0, &mut next)
    );
    // The fit with the lock (fix-ram-locked, 2026-11-15), the phone's session of main 3233fed: MemAvailable 8561 MiB at
    // the start. With the lock the headroom is 768 MiB: (8561 - 768 - 256) / 1.08 = 6978 MiB -> 6912 MiB; without it
    // 6741 -> 6656 MiB, as that session.
    assert_eq!(ram_fit(8561 * M, RAM_SESSION_RESERVE_LOCKED, gr, pt) / M, 6978);
    let locked = ram_fit(8561 * M, RAM_SESSION_RESERVE_LOCKED, gr, pt);
    assert_eq!(ram_fit_auto(11084 * M, 90, floor, Some(locked)), (6912 * M, Some(RamBound::Available)));
    assert_eq!(ram_fit_auto(11084 * M, 90, floor, Some(ram_fit(8561 * M, r, gr, pt))).0, 6656 * M);
    // What that leaves the host when the session is up (page tables at 8 % of the guest, the GPU's 256 MiB): 8561 -
    // 6912 - 552 - 256 = 841 MiB of MemAvailable, the valve's 256 MiB and 585 MiB more for what else grows.
    assert!(8561 * M - 6912 * M - 6912 * M * pt as u64 / 100 - gr > RAM_UNLOCK_BELOW + 512 * M);
}

#[test]
fn proc_reads() {
    let dir = testdir("proc");
    let meminfo = dir.join("meminfo");
    let partial = dir.join("meminfo-partial");
    write(&meminfo, "MemTotal:       11746000 kB\nMemFree:          401234 kB\nMemAvailable:    7340032 kB\nBuffers:            1234 kB\n");
    write(&partial, "MemTotal:        2097152 kB\nMemFree:          1048576 kB\n");
    assert_eq!(meminfo_fn(&meminfo), Some((PHONE, 7 * G)));
    assert!(meminfo_fn(&partial).is_none() && meminfo_fn("/nonexistent/meminfo").is_none());
    assert_eq!(meminfo_value(&meminfo, "MemFree"), Some(401234 << 10));
    assert_eq!(meminfo_value(&meminfo, "Buffers"), Some(1234 << 10));
    assert!(meminfo_value(&meminfo, "Mem").is_none() && meminfo_value(&meminfo, "SwapTotal").is_none());
    assert!(meminfo_value("/nonexistent/meminfo", "MemFree").is_none());
    // Several values in one read (host_memory_tick's one /proc/meminfo; vmstat's "key N" lines too).
    let mv = proc_values(&meminfo, &["MemAvailable", "Buffers", "SwapFree", "MemTotal"]);
    assert_eq!(mv, [Some(7 * G), Some(1234 << 10), None, Some(PHONE)]);
    assert_eq!(proc_values("/nonexistent/meminfo", &["MemAvailable"]), [None]);
    let vmstat = dir.join("vmstat");
    write(&vmstat, "nr_free_pages 12345\npswpin 777\npswpout 888888\npswpout_zero 1\n");
    assert_eq!(proc_values(&vmstat, &["pswpin", "pswpout"]), [Some(777), Some(888888)]);
    let _ = fs::remove_dir_all(&dir);
}

/// config::meminfo (the module's function, beside the tests' own `meminfo` paths).
fn meminfo_fn(path: impl AsRef<Path>) -> Option<(u64, u64)> {
    super::meminfo(path)
}

#[test]
fn ram_config() {
    let dir = testdir("ram");
    let image = image_set(&dir, "image");
    let path = format!("{image}/xnuhost.conf");
    let meminfo = dir.join("meminfo");
    let small = dir.join("meminfo-small");
    write(&meminfo, "MemTotal:       11746000 kB\nMemFree:          401234 kB\nMemAvailable:    7340032 kB\nBuffers:            1234 kB\n");
    write(&small, "MemTotal:        2097152 kB\nMemAvailable:    1048576 kB\n");
    let set_meminfo = format!("ram-meminfo={}", meminfo.display());
    let set_small = format!("ram-meminfo={}", small.display());
    let set_none = format!("ram-meminfo={}/none", dir.display());
    let with = |extra: &[&str]| -> Vec<String> {
        let mut a: Vec<String> = vec!["--image".into(), image.clone(), "--set".into(), set_meminfo.clone()];
        for s in extra {
            a.push("--set".into());
            a.push((*s).into());
        }
        a
    };
    let go = |a: &[String], lock: bool| run_lock(&a.iter().map(String::as_str).collect::<Vec<_>>(), lock);
    // The default since fix-ram-locked (2026-11-15): ram-giveback=0, prefaulted and locked, with what fits: MemAvailable
    // 7 GiB here. Where the lock cannot work (here) the headroom stays 1 GiB.
    let c = go(&with(&[]), false).unwrap();
    assert!(c.ram_bytes == 5376 * M && c.ram_auto_total == PHONE && c.ram_auto_available == 7 * G);
    assert!(c.ram_percent == 90 && c.ram_host_floor == RAM_AUTO_FLOOR && c.host_mem_log == 30 && c.usb_log == 3);
    assert!(
        c.ready_content_wait == READY_CONTENT_WAIT && c.ram_auto_bound == Some(RamBound::Available) && c.ram_fit_on
    );
    assert!(c.ram_fit / M == 5451 && c.ram_fit_available == 7 * G && !c.ram_giveback && c.ram_prefault && c.ram_lock);
    assert!(!c.ram_lock_expected && c.ram_lock_fit == 0 && c.ram_session_reserve == RAM_SESSION_RESERVE);
    assert!(
        c.ram_unlock_below == RAM_UNLOCK_BELOW && c.ram_gpu_reserve == RAM_GPU_RESERVE && c.ram_pagetable_percent == 8
    );
    assert!(c.return_graceful_ms == 0);
    // Where the lock can work (the phone: root with CAP_IPC_LOCK): the headroom is 768 MiB, (7168 - 1024) / 1.08 = 5688
    // -> 5632 MiB.
    let c = go(&with(&[]), true).unwrap();
    assert!(c.ram_bytes == 5632 * M && c.ram_lock_expected && c.ram_session_reserve == RAM_SESSION_RESERVE_LOCKED);
    assert!(c.ram_fit / M == 5688 && !c.ram_giveback && c.ram_lock);
    assert!(c.ram_lock_fit == 5632 * M && c.ram_huge); // the locked size that fits; 2 MiB pages asked
                                                       // The owner's boot of 2026-11-15 says the default explicitly: the same.
    let c = go(&with(&["ram-giveback=0", "ram-prefault=1", "ram-lock=1"]), true).unwrap();
    assert!(c.ram_bytes == 5632 * M && c.ram_lock_expected && c.ram_prefault);
    // One setting flips it back: ram-giveback=1 is 0058's way, nothing prefaulted or locked, the 1 GiB headroom.
    let c = go(&with(&["ram-giveback=1"]), true).unwrap();
    assert!(c.ram_bytes == 5376 * M && c.ram_giveback && !c.ram_prefault && !c.ram_lock && !c.ram_lock_expected);
    assert_eq!(c.ram_session_reserve, RAM_SESSION_RESERVE);
    // The valve stays on with the lock: ram-unlock-below=0 or 64M becomes 128 MiB; a headroom under the valve's level
    // and 256 MiB is raised to that (else the valve would open as soon as the session is up).
    let c = go(&with(&["ram-unlock-below=0", "ram-session-reserve=200M"]), true).unwrap();
    assert!(c.ram_unlock_below == RAM_UNLOCK_MIN && c.ram_session_reserve == RAM_UNLOCK_MIN + RAM_VALVE_MARGIN);
    let c = go(&with(&["ram-unlock-below=512M", "ram-session-reserve=1G"]), true).unwrap();
    assert!(c.ram_unlock_below == 512 * M && c.ram_session_reserve == G);
    // The RAM held fixed (ram-giveback=0 said): the same size; prefaulted and locked.
    let c = go(&with(&["ram-giveback=0"]), false).unwrap();
    assert!(
        c.ram_bytes == 5376 * M
            && c.ram_auto_bound == Some(RamBound::Available)
            && !c.ram_giveback
            && c.ram_prefault
            && c.ram_lock
    );
    let c = go(&with(&["ram-giveback=0", "ram-fit=0"]), false).unwrap(); // what fits off: the old formula
    assert!(c.ram_bytes == 9728 * M && c.ram_auto_bound == Some(RamBound::Floor) && c.ram_prefault && c.ram_lock);
    let c = go(&with(&["ram-giveback=1", "ram-fit=0"]), false).unwrap(); // 0058's way as before 2026-11-14
    assert!(c.ram_bytes == 9728 * M && c.ram_giveback && !c.ram_prefault && !c.ram_lock);
    let no_prefault =
        ["ram-prefault=0", "ram-lock=1", "ram-session-reserve=512M", "ram-unlock-below=0", "ram-giveback=0"];
    let c = go(&with(&no_prefault), false).unwrap(); // no prefault: no lock either
    assert!(c.ram_bytes == 5888 * M && !c.ram_prefault && !c.ram_lock && c.ram_unlock_below == 0 && !c.ram_giveback);
    let c = go(&with(&["ram-percent=50", "host-mem-log=0"]), false).unwrap();
    assert!(c.ram_bytes == 5376 * M && c.host_mem_log == 0 && c.ram_auto_bound == Some(RamBound::Available));
    let c = go(&with(&["ram-host-floor=4G", "ram-fit=0"]), false).unwrap();
    assert_eq!(c.ram_bytes, 7168 * M);
    // a host too small for auto: say so, start nothing
    assert!(run(&["--image", &image, "--set", &set_small]).is_err());
    // no meminfo: the old 4 GiB
    let c = run(&["--image", &image, "--set", &set_none]).unwrap();
    assert!(c.ram_bytes == 4 * G && c.ram_auto_total == 0);
    // a size given: no auto
    let c = run(&["--image", &image, "--set", &set_small, "--ram", "1536M"]).unwrap();
    assert!(c.ram_bytes == 1536 * M && c.ram_auto_total == 0);
    let e = run(&["--image", &image, "--ram", "65G"]).unwrap_err();
    assert_eq!(e.lines, ["--ram must be a multiple of 2 MiB, at least 256 MiB, at most 64 GiB"]);
    // The image set's xnuhost.conf: its ram= and ram-percent= apply when the command line has none; --ram auto wins.
    write(&path, &format!("boot-args=-v debug=0x8\nram=3G\nram-percent=80\nram-fit=0\n{set_meminfo}\n"));
    let c = run(&["--image", &image]).unwrap();
    assert!(c.ram_bytes == 3 * G && c.ram_percent == 80);
    let c = run(&["--image", &image, "--ram", "auto"]).unwrap();
    assert!(c.ram_bytes == 8960 * M && c.ram_auto_total == PHONE); // 80% of 11.2 GiB
                                                                   // The image set's own ram-giveback=0 holds the RAM fixed, at what fits (5376 MiB of MemAvailable 7 GiB)
    write(&path, &format!("boot-args=-v debug=0x8\nram-percent=80\nram-giveback=0\n{set_meminfo}\n"));
    let c = run(&["--image", &image, "--ram", "auto"]).unwrap();
    assert!(c.ram_bytes == 5376 * M && !c.ram_giveback && c.ram_lock);
    let c = run(&["--image", &image, "--set", "ram-giveback=1"]).unwrap(); // the command line wins
    assert!(c.ram_bytes == 5376 * M && c.ram_giveback && !c.ram_lock);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn boot_argument_helpers() {
    // owner-hide-vmm naming bluetoothd (a crash loop in 24A446): found in any position of the list, not as a prefix
    assert!(boot_args_hides_vmm_from("-v owner-hide-vmm=bluetoothd,BlueTool wdt=-1", "bluetoothd"));
    assert!(boot_args_hides_vmm_from("owner-hide-vmm=audiomxd,bluetoothd", "bluetoothd"));
    assert!(!boot_args_hides_vmm_from("owner-hide-vmm=audiomxd,com.apple.WebKit.GPU bluetoothd=1", "bluetoothd"));
    assert!(!boot_args_hides_vmm_from("owner-hide-vmm=bluetoothd2 xowner-hide-vmm=bluetoothd", "bluetoothd"));
    assert!(!boot_args_hides_vmm_from("", "bluetoothd"));
    // bt-transport-override read as bluetoothd reads it: any word that contains the name, the value after its '=',
    // under 13, the last word wins
    assert_eq!(boot_args_bt_transport("-v bt-transport-override=10 wdt=-1"), Some(10));
    assert_eq!(boot_args_bt_transport("bt-transport-override=10 bt-transport-override=1"), Some(1));
    assert_eq!(boot_args_bt_transport("xbt-transport-override=10"), Some(10));
    assert_eq!(boot_args_bt_transport("bt-transport-override=13"), None);
    assert_eq!(boot_args_bt_transport("bt-transport-override=10 bt-transport-override=99"), Some(10));
    assert_eq!(boot_args_bt_transport("bt-transport-override"), None);
    assert_eq!(boot_args_bt_transport("-v wdt=-1 transport=10"), None);
    assert_eq!(boot_args_bt_transport(""), None);
    // ram-view=: the compressor's room for the view (the view over the RAM, rounded up), added once; a given value stays
    let mut v = "-v".to_owned();
    assert!(view_boot_args(&mut v, 96, 0, 7 << 30) == Ok(0) && v == "-v"); // off
    assert!(view_boot_args(&mut v, 96, 6 << 30, 7 << 30) == Ok(0) && v == "-v"); // not over the RAM
    assert!(view_boot_args(&mut v, 96, 12 << 30, 7 << 30) == Ok(2) && v == "-v kern.compressor_pool_multiplier=2");
    assert!(view_boot_args(&mut v, 96, 12 << 30, 7 << 30) == Ok(2) && v == "-v kern.compressor_pool_multiplier=2");
    let mut v = "kern.compressor_pool_multiplier=4".to_owned();
    assert!(view_boot_args(&mut v, 96, 12 << 30, 4 << 30) == Ok(3) && v == "kern.compressor_pool_multiplier=4");
    assert_eq!(view_boot_args(&mut String::new(), 20, 12 << 30, 4 << 30), Err(NoRoom)); // no room
                                                                                        // the display driver's switches on the kernel's command line: added once, a given value stays
    let mut a = "-v debug=0x8".to_owned();
    assert!(display_boot_args(&mut a, 128, 3, 120, DisplayFormat::Bgrx8).is_ok());
    assert_eq!(a, "-v debug=0x8 offline-legacy-unit-scale=3 sheng-vsync=120");
    assert!(display_boot_args(&mut a, 128, 2, 60, DisplayFormat::Bgrx8).is_ok());
    assert_eq!(a, "-v debug=0x8 offline-legacy-unit-scale=3 sheng-vsync=120");
    let mut a = "sheng-vsync=90 not-sheng-vsync=1".to_owned();
    assert!(display_boot_args(&mut a, 128, 2, 120, DisplayFormat::Bgrx8).is_ok());
    assert_eq!(a, "sheng-vsync=90 not-sheng-vsync=1 offline-legacy-unit-scale=2");
    let mut a = String::new();
    assert!(display_boot_args(&mut a, 128, 2, 120, DisplayFormat::Bgrx8).is_ok());
    assert_eq!(a, "offline-legacy-unit-scale=2 sheng-vsync=120");
    assert!(display_boot_args(&mut a, 44, 2, 120, DisplayFormat::Bgrx8).is_ok());
    assert_eq!(display_boot_args(&mut String::new(), 40, 2, 120, DisplayFormat::Bgrx8), Err(NoRoom)); // no room
                                                                                                      // a half-float frame: the guest is told to hand its half-float surfaces over as they are; a given value stays
    let mut a = String::new();
    assert!(display_boot_args(&mut a, 128, 3, 120, DisplayFormat::Rgba16f).is_ok());
    assert_eq!(a, "oneios-display-wide=1 offline-legacy-unit-scale=3 sheng-vsync=120");
    let mut a = "oneios-display-wide=0".to_owned();
    assert!(display_boot_args(&mut a, 128, 3, 120, DisplayFormat::Rgba16f).is_ok());
    assert_eq!(a, "oneios-display-wide=0 offline-legacy-unit-scale=3 sheng-vsync=120");
}

#[test]
fn display_mode() {
    let dir = testdir("display");
    let image = image_set(&dir, "image");
    let path = format!("{image}/xnuhost.conf");
    write(&path, "# settings of this image\nboot-args=-v debug=0x8\n");
    let pm = |width, height, hz| PanelMode { width, height, hz };
    // The display's mode: the panel's native (largest) mode, not the one One UI runs; overrides; the defaults.
    let modes = [
        pm(1080, 2340, 120),
        pm(1440, 3120, 60),
        pm(1440, 3120, 120),
        pm(720, 1560, 120),
        pm(1440, 3120, 24),
        pm(0, 0, 120),
    ];
    assert_eq!(panel_native(&modes), Some(pm(1440, 3120, 120)));
    assert_eq!(panel_native(&modes[..1]), Some(pm(1080, 2340, 120))); // an FHD+ panel is its own native
    assert!(panel_native(&modes[5..]).is_none() && panel_native(&[]).is_none());
    let bench_hidl = ["--image", image.as_str(), "--mode", "bench", "--bluetooth", "hidl"];
    let mut c = run(&bench_hidl).unwrap();
    assert!(c.scale == 3 && c.hz == 0 && c.scale_fixup);
    assert_eq!(c.display_mode(None), pm(1440, 3120, 120)); // nothing readable: the S24 Ultra's
    assert_eq!(c.display_mode(Some(&pm(1260, 2800, 144))), pm(1260, 2800, 144)); // another panel: its own
    assert_eq!(c.display_mode(Some(&pm(1260, 2800, 0))).hz, 120); // a mode without a rate
                                                                  // panel-fallback= (the Galaxy S25's and A56's 1080x2340): only where the panel is not read; a read panel still wins
    assert_eq!(c.panel_fallback(), pm(1440, 3120, 120));
    assert!(c.set("panel-fallback", "1080x2340@120").is_ok() && c.panel_fallback == Some(pm(1080, 2340, 120)));
    assert_eq!(c.display_mode(None), pm(1080, 2340, 120));
    assert_eq!(c.display_mode(Some(&pm(1260, 2800, 144))), pm(1260, 2800, 144));
    assert!(c.set("panel-fallback", "1080x2340").is_ok() && c.panel_fallback == Some(pm(1080, 2340, 0)));
    assert_eq!(c.display_mode(None).hz, 120);
    assert!(c.set("panel-fallback", "1080x2340@60").is_ok() && c.display_mode(None).hz == 60);
    let rateless = pm(1260, 2800, 0);
    assert!(c.display_mode(Some(&rateless)).width == 1260 && c.display_mode(Some(&rateless)).hz == 60); // a rate-less report: the fallback's
    for bad in [
        "",
        "1080",
        "1080x",
        "x2340",
        "0x2340",
        "1080x0",
        "1080x2340@",
        "1080x2340@20",
        "1080x2340@241",
        "1080x2340 ",
        "20000x2340",
        "1080x2340@120x",
        "-1080x2340",
        "123456x10",
    ] {
        assert!(c.set("panel-fallback", bad).is_err(), "{bad:?}");
    }
    assert_eq!(c.panel_fallback, Some(pm(1080, 2340, 60))); // a refused value changes nothing
    let mode1 = [
        "--image",
        &image,
        "--width",
        "1320",
        "--height",
        "2868",
        "--hz",
        "60",
        "--scale",
        "2",
        "--set",
        "scale-fixup=0",
    ];
    let c = run(&mode1).unwrap();
    assert!(c.scale == 2 && c.hz == 60 && !c.scale_fixup);
    assert_eq!(c.display_mode(Some(&pm(1440, 3120, 120))), pm(1320, 2868, 60));
    // the connector of the panel: not the Virtual (writeback, 5120x2560) ones of the S24 Ultra, not DisplayPort
    assert!(
        panel_connector(16)
            && panel_connector(14)
            && !panel_connector(15)
            && !panel_connector(10)
            && !panel_connector(11)
    );
    // the helper's report of the panel, and the mode it gives a guest with nothing configured
    let report = |magic: &[u8; 4], a: u32, b: u32, c: u32| -> Vec<u8> {
        let mut r = magic.to_vec();
        for w in [a, b, c, 0] {
            r.extend_from_slice(&w.to_ne_bytes());
        }
        r
    };
    assert_eq!(panel_report(&report(b"XHDP", 1440, 3120, 120)), Some(pm(1440, 3120, 120)));
    assert!(panel_report(&report(b"XHDP", 1440, 3120, 120)[..19]).is_none());
    assert!(panel_report(&report(b"XHDP", 0, 3120, 120)).is_none());
    assert!(panel_report(&report(b"XHDP", 16385, 3120, 120)).is_none());
    assert!(panel_report(&report(b"XHDQ", 1440, 3120, 120)).is_none());
    // the scale in the image set (the userland hook's file), under the command line's
    let overlay = format!("{image}/overlay/userland");
    fs::create_dir_all(&overlay).unwrap();
    let overlay = format!("{overlay}/xnuhost.conf");
    write(&overlay, "scale=2\nwidth=1440\nheight=3120\nhz=120\n");
    let c = run(&bench_hidl).unwrap();
    assert!(c.scale == 2 && c.width == 1440 && c.height == 3120 && c.hz == 120); // the image's mode, over any panel
    assert_eq!(c.display_mode(Some(&pm(5120, 2560, 60))), pm(1440, 3120, 120));
    let c = run(&mode1).unwrap();
    assert!(c.width == 1320 && c.hz == 60); // the command line's wins
    let mut c = run(&["--image", &image, "--set", "scale=3", "--text-fixup", "file.01,10,aa,bb"]).unwrap();
    assert!(c.scale == 3 && c.text_fixups.len_bytes() == 17);
    // the fixups of QuartzCore's preferred scale: for @3x on the build they were read in, after the configured ones
    scale_fixups(Some("24A446"), 3, &mut c.text_fixups);
    let f = c.text_fixups.entries();
    assert!(c.text_fixups.len_bytes() == 17 + 54 + 70 && f[0] == "file.01,10,aa,bb");
    assert_eq!(f[1], "dyld_shared_cache_arm64e.01,40b17a8,49008052,69008052");
    assert!(f[2].ends_with(",40b17bc,280080520895889a,680080521f2003d5"));
    let bytes = c.text_fixups.to_bytes();
    assert!(bytes.len() == 141 && bytes[140] == 0 && bytes[16] == 0);
    let one = |fixups: &mut TextFixups| {
        *fixups = TextFixups::default();
        fixups.push_within("file.01,10,aa,bb", TEXT_FIXUPS_SIZE);
    };
    one(&mut c.text_fixups);
    scale_fixups(Some("24A446"), 2, &mut c.text_fixups); // @2x is QuartzCore's own answer
    assert_eq!(c.text_fixups.len_bytes(), 17);
    scale_fixups(Some("24A447"), 3, &mut c.text_fixups);
    scale_fixups(None, 3, &mut c.text_fixups);
    assert_eq!(c.text_fixups.len_bytes(), 17);
    let _ = fs::remove_dir_all(&dir);
}

// ---- the host phone (onebridges::hw, host_device_layer; docs/hardware.md "The host device") -------------------------
// Each known phone's defaults under the command line and the image set, an unknown phone, a forced one, detection off;
// and the proof that the S24 Ultra's entry and an unknown phone finish to exactly the config of detection off (today's).

fn host_props(dir: &Path, name: &str, model: &str, soc: &str) -> String {
    let path = dir.join(format!("{name}.props"));
    write(&path, &format!("[ro.product.model]: [{model}]\n[ro.soc.model]: [{soc}]\n[ro.soc.manufacturer]: [QTI]\n"));
    path.to_str().unwrap().to_owned()
}

struct HostRig {
    image: String,
    meminfo: String,
    root: String,
}

impl HostRig {
    /// The command line of the OneOS fork's session plus the host settings, finished.
    fn finish(&self, props: Option<&str>, device: Option<&str>, extra: Option<&str>) -> Result<Config, ConfigError> {
        let ram_meminfo = format!("ram-meminfo={}", self.meminfo);
        let host_root = format!("host-root={}", self.root);
        let mut a: Vec<String> = ["--image", &self.image, "--ram", "4G", "--set", &ram_meminfo, "--set", &host_root]
            .map(String::from)
            .to_vec();
        if let Some(p) = props {
            a.extend(["--set".into(), format!("host-props={p}")]);
        }
        if let Some(d) = device {
            a.extend(["--set".into(), format!("host-device={d}")]);
        }
        if let Some(e) = extra {
            a.extend(["--set".into(), e.to_owned()]);
        }
        run(&a.iter().map(String::as_str).collect::<Vec<_>>())
    }
}

/// The config's host fields cleared (so two configs compare whole).
fn host_clear(mut c: Config) -> Config {
    c.host_device.clear();
    c.host_props.clear();
    c.host_root.clear();
    c.host_line.clear();
    c.host = None;
    c.given.remove("host-device");
    c
}

#[test]
fn host_device() {
    let dir = testdir("host");
    let image = image_set(&dir, "host-image");
    let root = dir.join("host-root");
    fs::create_dir_all(&root).unwrap();
    let meminfo = dir.join("host-meminfo"); // a fixed MemAvailable: two finishes agree
    write(&meminfo, "MemTotal:       11746000 kB\nMemAvailable:    9000000 kB\n");
    let rig = HostRig {
        image: image.clone(),
        meminfo: meminfo.to_str().unwrap().into(),
        root: root.to_str().unwrap().into(),
    };
    let find = |name| bhw::host_find(name).map(Host);

    // Today's defaults: detection off.
    let off = rig.finish(None, Some("none"), None).unwrap();
    assert!(off.host.is_none() && off.host_line == "host: detection off (host-device=none): today's defaults");
    assert!(off.gpu == Gpu::Auto && off.hw == HwSettings::default());
    let off = host_clear(off);
    // The S24 Ultra (the home device) and an unknown phone: byte for byte today's config.
    let props = host_props(&dir, "s24u", "SM-S928B", "SM8650");
    let c = rig.finish(Some(&props), None, None).unwrap();
    assert!(c.host == find("galaxy-s24-ultra"));
    assert!(c.host_line.contains(
        "host: galaxy-s24-ultra (Galaxy S24 Ultra (SM-S928, SM8650)), matched by ro.product.model=SM-S928B; read: "
    ));
    assert!(c.host_line.contains("; defaults: none (today's)"));
    assert_eq!(host_clear(c), off);
    let c = rig.finish(Some(&props), Some("galaxy-s24-ultra"), None).unwrap();
    assert!(c.host_line.contains("host-device=galaxy-s24-ultra (forced)"));
    assert_eq!(host_clear(c), off);
    let props = host_props(&dir, "pixel", "Pixel 11 Pro", "Tensor G5");
    let c = rig.finish(Some(&props), None, None).unwrap();
    assert!(c.host.is_none());
    assert!(c.host_line.contains("host: unknown phone (read: ro.product.model=Pixel 11 Pro ro.product.device=(none) "));
    assert!(c.host_line.contains("ro.soc.model=Tensor G5 ro.soc.manufacturer=QTI): today's defaults (host-device=NAME forces one of galaxy-s24-ultra, "));
    assert_eq!(host_clear(c), off);
    // No property area at all (this PC: no bionic, no file) is an unknown phone too.
    let c = rig.finish(None, None, None).unwrap();
    assert!(c.host.is_none() && c.host_line.contains("unknown phone (read: no property area)"));
    assert_eq!(host_clear(c), off);
    // The S25 Ultra: the SoC rule on its own trips and on 3 readings (environment defaults, set by the session, not
    // here); no xnuhost setting of its own (docs/s25.md: everything else is [U]).
    let props = host_props(&dir, "s25u", "SM-S938B", "SM8750");
    let c = rig.finish(Some(&props), None, None).unwrap();
    assert!(c.host == find("galaxy-s25-ultra"));
    assert!(c.host_line.contains("; defaults: ONEIOS_THERMAL_SOC_TRIPS=1, ONEIOS_THERMAL_SOC_SAMPLES=3"));
    assert!(std::env::var_os("ONEIOS_THERMAL_SOC_TRIPS").is_none());
    assert_eq!(host_clear(c), off);
    // The S25: its 1080x2340 panel as the fallback mode only (a panel that is read still wins); nothing else changes.
    let props = host_props(&dir, "s25", "SM-S931B", "SM8750");
    let c = rig.finish(Some(&props), None, None).unwrap();
    assert!(c.host == find("galaxy-s25") && c.panel_fallback == Some(PanelMode { width: 1080, height: 2340, hz: 120 }));
    assert!(c.host_line.contains(
        "; defaults: panel-fallback=1080x2340@120, ONEIOS_THERMAL_SOC_TRIPS=1, ONEIOS_THERMAL_SOC_SAMPLES=3"
    ));
    let read_panel = PanelMode { width: 1440, height: 3120, hz: 120 };
    assert!(c.display_mode(Some(&read_panel)).width == 1440 && c.display_mode(None).width == 1080);
    let c2 = rig.finish(Some(&props), None, Some("panel-fallback=1260x2800")).unwrap();
    assert!(
        c2.panel_fallback.unwrap().width == 1260 && c2.host_line.contains("panel-fallback=1080x2340@120 not taken")
    );
    let mut c = host_clear(c);
    c.panel_fallback = None;
    assert_eq!(c, off);
    // The S26 Ultra: its brightness cap is an environment default (the session sets it); the config is today's.
    let props = host_props(&dir, "s26u", "SM-S948B", "SM8850");
    let c = rig.finish(Some(&props), None, None).unwrap();
    assert!(c.host == find("galaxy-s26-ultra") && c.host_line.contains("matched by ro.product.model=SM-S948B"));
    assert!(c.host_line.contains("; defaults: ONEIOS_BRIGHT_MAX=2300"));
    assert_eq!(host_clear(c), off);
    // The A56: gpu=2d, under the command line and the image set.
    let props = host_props(&dir, "a56", "SM-A566E", "s5e8855");
    let c = rig.finish(Some(&props), None, None).unwrap();
    assert!(c.host == find("galaxy-a56") && c.gpu == Gpu::TwoD);
    assert!(c.host_line.contains("; defaults: gpu=2d") && !c.host_line.contains("not taken"));
    let c = rig.finish(Some(&props), None, Some("gpu=auto")).unwrap();
    assert!(
        c.gpu == Gpu::Auto && c.host_line.contains("gpu=2d not taken (the command line or the image set gives it)")
    );
    assert_eq!(rig.finish(Some(&props), Some("none"), None).unwrap().gpu, Gpu::Auto);
    let c = rig.finish(Some(&props), Some("NONE"), None).unwrap();
    assert!(c.host.is_none() && c.gpu == Gpu::Auto);
    let conf = format!("{image}/xnuhost.conf");
    write(&conf, "gpu=3d\n");
    let c = rig.finish(Some(&props), None, None).unwrap();
    assert!(c.gpu == Gpu::ThreeD && c.host_line.contains("gpu=2d not taken"));
    assert_eq!(rig.finish(Some(&props), None, Some("gpu=2d")).unwrap().gpu, Gpu::TwoD); // the command line over the image set
    write(&conf, "host-device=none\n"); // the image set may turn it off
    let c = rig.finish(Some(&props), None, None).unwrap();
    assert!(c.host.is_none() && c.gpu == Gpu::Auto);
    assert!(rig.finish(Some(&props), Some("auto"), None).unwrap().host == find("galaxy-a56")); // and the command line on
    fs::remove_file(&conf).unwrap();
    // Forced: a name wins over the phone's properties; a name that is not in the table is refused, and so is a
    // property file that cannot be read.
    let props = host_props(&dir, "s24u", "SM-S928B", "SM8650");
    let c = rig.finish(Some(&props), Some("galaxy-a56"), None).unwrap();
    assert!(c.host == find("galaxy-a56") && c.gpu == Gpu::TwoD);
    assert!(c.host_line.contains("host-device=galaxy-a56 (forced); read: ro.product.model=SM-S928B"));
    assert!(rig.finish(Some(&props), Some("pixel-11"), None).is_err());
    let mut c = Config::new();
    assert!(c.set("host-device", "pixel-11").is_err() && c.set("host-device", "galaxy-s26-ultra").is_ok());
    assert!(c.set("host-device", "auto").is_ok() && c.set("host-device", "none").is_ok());
    let missing = dir.join("no-such.props");
    assert!(rig.finish(Some(missing.to_str().unwrap()), None, None).is_err());
    // Every entry of the table: forced, it finishes, and each of its xnuhost defaults is what the finished config holds.
    for entry in bhw::host_table() {
        let c = rig.finish(None, Some(entry.name), None).unwrap();
        assert!(c.host == Some(Host(entry)));
        for pair in entry.config {
            let (key, value) = pair.split_once('=').expect("KEY=VALUE");
            let mut want = Config::new();
            assert!(want.set(key, value).is_ok(), "{pair}");
            match key {
                "gpu" => assert_eq!(c.gpu, want.gpu),
                "panel-fallback" => assert_eq!(c.panel_fallback, want.panel_fallback),
                _ => {
                    want.hw.merge(&off.hw);
                    assert_eq!(c.hw, want.hw);
                }
            }
        }
    }
    // The selftest leaves no gpu unset.
    let c = run(&["--selftest"]).unwrap();
    assert!(c.gpu == Gpu::Auto && c.host_line.is_empty());
    let _ = fs::remove_dir_all(&dir);
}
