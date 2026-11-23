// SPDX-License-Identifier: MIT
//! Builds Droid-VM's drm2kgsl renderer from its C, as runtime/xnuhost/Makefile did: the renderer and the few
//! virglrenderer and mesa-util sources under it from external/virglrenderer (the commit
//! scripts/setup/get-references.sh pins; `VIRGL=DIR` another checkout), not virglrenderer.c (GL, libdrm).
//! drm2kgsl_renderer.c is compiled from a copy in OUT_DIR with drm2kgsl/patches applied in name order: the checkout
//! stays at its pin. `-imacros config.h` stands for what virglrenderer's meson.build would have found. Then our glue
//! (glue/oneios_glue.c: the structures and va_lists Rust cannot reach). For Android the NDK's clang (runtime/ndk-cc)
//! and llvm-ar; for the build host `$CC` (cc) and `$AR` (ar), with host_c11.h (mesa's C11 threads shim beside a new
//! glibc) and glue/oneios_sys.h (every open and ioctl of the renderer through a table the tests set: no -Wl,--wrap).
//!
//! Without the checkout nothing of it is built: cfg `drm2kgsl` stays unset, `drm2kgsl_sys::AVAILABLE` is false, and
//! xnuhost is the 2D-only one, which says so when it starts ("this build has no drm2kgsl").
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};

const SOURCES: &[&str] = &[
    "src/drm/drm2kgsl/drm2kgsl_renderer.c",
    "src/drm/drm_context.c",
    "src/drm/drm_fence.c",
    "src/drm/drm_util.c",
    "src/virgl_context.c",
    "src/virgl_resource.c",
    "src/virgl_util.c",
    "src/virgl_fence.c",
    "src/gallium/auxiliary/util/u_hash_table.c",
    "src/mesa/util/hash_table.c",
    "src/mesa/util/ralloc.c",
    "src/mesa/util/os_file.c",
    "src/mesa/util/anon_file.c",
    "src/mesa/util/u_debug.c",
    "src/mesa/util/u_math.c",
    "src/mesa/util/u_cpu_detect.c",
];
const INCLUDES: &[&str] = &[
    "src",
    "src/drm",
    "src/mesa",
    "src/mesa/util",
    "src/mesa/compat",
    "src/mesa/pipe",
    "src/gallium/include",
    "src/gallium/auxiliary",
    "src/gallium/auxiliary/util",
];

struct Toolchain {
    cc: PathBuf,
    ar: PathBuf,
    host: bool,
}

fn toolchain(manifest: &Path, target: &str) -> Toolchain {
    if target.contains("android") {
        // runtime/ndk-cc finds the NDK ($NDK, $ANDROID_NDK_HOME, the SDK's 28.2.13676358); llvm-ar is beside its clang.
        let darwin = cfg!(target_os = "macos");
        let home = env::var("HOME").unwrap_or_default();
        let default = if darwin {
            format!("{home}/Library/Android/sdk/ndk/28.2.13676358")
        } else {
            format!("{home}/Android/Sdk/ndk/28.2.13676358")
        };
        let ndk = env::var("NDK").or_else(|_| env::var("ANDROID_NDK_HOME")).unwrap_or(default);
        let host = if darwin { "darwin-x86_64" } else { "linux-x86_64" };
        let bin = Path::new(&ndk).join("toolchains/llvm/prebuilt").join(host).join("bin");
        Toolchain { cc: manifest.join("../../ndk-cc"), ar: bin.join("llvm-ar"), host: false }
    } else {
        let cc = env::var("CC").unwrap_or_else(|_| "cc".into());
        let ar = env::var("AR").unwrap_or_else(|_| "ar".into());
        Toolchain { cc: cc.into(), ar: ar.into(), host: true }
    }
}

fn run(cmd: &mut Command, what: &str) {
    let status = cmd.status().unwrap_or_else(|e| panic!("drm2kgsl: {what}: {e}"));
    assert!(status.success(), "drm2kgsl: {what} failed ({status})");
}

fn main() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let out = PathBuf::from(env::var("OUT_DIR").unwrap());
    let target = env::var("TARGET").unwrap();
    println!("cargo:rustc-check-cfg=cfg(drm2kgsl)");
    println!("cargo:rerun-if-env-changed=VIRGL");
    println!("cargo:rerun-if-env-changed=NDK");
    println!("cargo:rerun-if-env-changed=ANDROID_NDK_HOME");
    println!("cargo:rerun-if-env-changed=CC");
    println!("cargo:rerun-if-changed=glue");
    let tc = toolchain(&manifest, &target);
    let mut objects = Vec::new();

    // The hooks of glue/oneios_sys.h: on every host build (also a 2D-only one), since drm2kgsl_sys::sys calls them.
    if tc.host {
        let o = out.join("oneios_sys.o");
        run(
            Command::new(&tc.cc)
                .args(["-std=gnu11", "-O2", "-g", "-fPIC", "-Wall", "-Wextra", "-Werror", "-c"])
                .arg(manifest.join("glue/oneios_sys.c"))
                .arg("-o")
                .arg(&o),
            "oneios_sys.c",
        );
        objects.push(o);
    }

    let virgl =
        env::var("VIRGL").map(PathBuf::from).unwrap_or_else(|_| manifest.join("../../../external/virglrenderer"));
    let renderer = virgl.join(SOURCES[0]);
    println!("cargo:rerun-if-changed={}", renderer.display());
    if renderer.is_file() {
        build_renderer(&manifest, &out, &virgl, &tc, &mut objects);
        println!("cargo:rustc-cfg=drm2kgsl");
    } else {
        println!(
            "cargo:warning=drm2kgsl: no GPU renderer: {} is missing (scripts/setup/get-references.sh virglrenderer): \
             the 2D-only xnuhost",
            renderer.display()
        );
    }

    if objects.is_empty() {
        return;
    }
    let lib = out.join("liboneios_drm2kgsl.a");
    let _ = fs::remove_file(&lib);
    run(Command::new(&tc.ar).arg("rcs").arg(&lib).args(&objects), "the archive");
    println!("cargo:rustc-link-search=native={}", out.display());
    println!("cargo:rustc-link-lib=static=oneios_drm2kgsl");
    println!("cargo:rustc-link-lib=m");
}

fn build_renderer(manifest: &Path, out: &Path, virgl: &Path, tc: &Toolchain, objects: &mut Vec<PathBuf>) {
    let d2k = manifest; // runtime/xnuhost/drm2kgsl: config.h, host_c11.h, oneios_*.h, patches
    for f in ["config.h", "host_c11.h", "oneios_drm2kgsl.h", "oneios_gpu_policy.h", "patches"] {
        println!("cargo:rerun-if-changed={}", d2k.join(f).display());
    }
    for s in &SOURCES[1..] {
        println!("cargo:rerun-if-changed={}", virgl.join(s).display());
    }

    // drm2kgsl/patches, in name order, on a copy.
    let mut patches: Vec<PathBuf> = fs::read_dir(d2k.join("patches"))
        .expect("drm2kgsl/patches")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "patch"))
        .collect();
    patches.sort();
    let patched = out.join("patched");
    fs::create_dir_all(&patched).unwrap();
    let copy = patched.join("drm2kgsl_renderer.c");
    fs::copy(virgl.join(SOURCES[0]), &copy).expect("copying drm2kgsl_renderer.c");
    for p in &patches {
        let input = fs::File::open(p).unwrap();
        run(
            Command::new("patch").args(["-s", "--forward", "--no-backup-if-mismatch", "-F0"]).arg(&copy).stdin(input),
            &format!("patch {}", p.display()),
        );
    }

    // The flags of the Makefile's VIRGL_INC, then the target's.
    let mut flags: Vec<String> = vec![
        "-std=gnu11".into(),
        "-g".into(),
        "-fPIC".into(),
        "-imacros".into(),
        d2k.join("config.h").display().to_string(),
        "-DHAVE_CONFIG_H=1".into(),
        "-D_GNU_SOURCE".into(),
        format!("-I{}", d2k.display()),
        format!("-I{}", manifest.join("glue").display()),
    ];
    flags.extend(INCLUDES.iter().map(|i| format!("-I{}", virgl.join(i).display())));
    if tc.host {
        flags.extend(["-O1".into(), "-U_FORTIFY_SOURCE".into(), "-include".into()]);
        flags.push(d2k.join("host_c11.h").display().to_string());
        flags.push("-include".into());
        flags.push(manifest.join("glue/oneios_sys.h").display().to_string());
    } else {
        flags.push("-O2".into());
    }

    let mut jobs: Vec<(String, Child)> = Vec::new();
    let mut compile = |src: PathBuf, own: bool| {
        let name = src.file_stem().unwrap().to_string_lossy().into_owned();
        let o = out.join(format!("{name}.o"));
        let mut cmd = Command::new(&tc.cc);
        cmd.args(&flags);
        if own {
            cmd.args(["-Wall", "-Wextra", "-Werror", "-Wno-unused-parameter"]);
        } else {
            cmd.arg("-w");
        }
        cmd.arg("-c").arg(&src).arg("-o").arg(&o);
        jobs.push((name, cmd.spawn().unwrap_or_else(|e| panic!("drm2kgsl: {}: {e}", tc.cc.display()))));
        objects.push(o);
    };
    compile(copy, false);
    for s in &SOURCES[1..] {
        compile(virgl.join(s), false);
    }
    compile(manifest.join("glue/oneios_glue.c"), true);
    if tc.host {
        // msm_kgsl.h's layout, for the test that checks xnuhost's stand-in for KGSL mirrors it
        compile(manifest.join("glue/oneios_kgsl_layout.c"), false);
    }
    for (name, mut child) in jobs {
        let status = child.wait().unwrap();
        assert!(status.success(), "drm2kgsl: compiling {name}.c failed ({status})");
    }
}
