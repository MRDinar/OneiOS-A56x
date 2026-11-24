# iOS 27.2.1 (24B156) beside 27.0.1 (24A446)

The owner's goal (2026-11-25): iOS 27.2.1 (24B156, iPhone18,2, the only build Apple signs since 2026-11-09) as a
build the user can select on the S24 Ultra and every phone the app offers iOS 27 on (not the A56), with 27.0.1
(24A446) the default and first choice until a phone boot of 27.2.1 passes. One runtime release serves both.

## State at the hand-off of 2026-11-25

Branch `ios272/base` (worktree `/home/akshay/Documents/GitHub/OneiOS-wt/ios272` on the build laptop; `external`,
`inputs`, `out` are links to the main checkout's). Nothing is merged to main. Nothing booted.

Done:
- `profiles/ios-27.2.1-iPhone18,2.toml` (commit `9abcce7`): pins from Apple's ZIP directory and BuildManifest
  (`image check-ipsw`: ok, 2026-11-25), the SystemOS image's `sha256`/`plain_sha256` from a fetch. The OS image's
  are not pinned (not fetched: about 20 GiB of room at once; the laptop has about 12 GiB free).
- `inputs/24B156/` (git-ignored): every small member, and the SystemOS image decrypted
  (`141-10617-235.dmg`, 6.3 GB, the dyld shared cache). `out/kernel-inputs/24B156/kernelcache.macho`
  (78,512,128 bytes, SHA-256 `f6f31ac4...e9a5939`).
- **No newer XNU is needed**: 24A446's kernelcache is `xnu-13432.2.10~2`, 24B156's `xnu-13432.42.3~28`, the same
  branch; we already run xnu-13432 kexts on our xnu-12377 kernel. (The `xnu-13432-repin` branch's "blocked until
  Apple publishes xnu-13432" was wrong for this reason.) The same 301 fileset entries; 91 have another text size;
  the kernel's text is 32 KiB longer.
- **The class check, 8 of 11 the same**: `prelink classes` (one class per run: with all 11 in one run the first,
  IOAESAccelerator, fails "no static initializer with this class name found", a bug to look at) against
  `classes-24A446-v54.json`, comparing size, slot count and every slot's diversity. The same for IOAESAccelerator,
  IOCryptoAccelerator, IOHIDDevice, IOSurface, IOGPUDevice, IOAcceleratorES, IOGPUDeviceUserClient and
  AGXDeviceUserClient. Different: **IOMobileFramebufferLegacy** 0x1dd8 -> 0x1de0 bytes (379 slots both), so the
  display archive's field offsets (`legacy-display-24A446-v54.json` "display"/"request", 6720...7156) must be read
  again in 24B156's code before a 24B156 kernel; **IOGPU** 0x2e8/216 slots -> 0x2f0/217 (nothing of ours subclasses
  it; IOAcceleratorES, which NativeAdreno subclasses, is the same); **AGXAccelerator** 0xcdf8/474 -> 0xce00/475 (left
  out of the collection). The 24B156 records are in the scratchpad only (regenerate with the command below).
- **A 24B156 kernel is a second kernel build**: the board archives (hid, net, display, aes, gpu) compile 24A446's
  original addresses into `xnu.macho` (`<stub>.original` N_ABS, metaclass and vtable constants in
  `*-bindings.S`), so one `xnu.macho` cannot pair with both kernelcaches. The runtime has to carry two kernels
  (`runtime.json` `"kernels"`; the contract given to the app helper is below).
- `kernel/unixnu/profiles/os/ios-27.2.1-24B156/` (a copy of 24A446's with the IPSW and kernelcache pin). Not used
  by any build yet.
- `oneios-tool prelink port` (`tools/oneios-tool/crates/oneios-prelink/src/port.rs`, uncommitted until this note;
  **compiles, never run**): the cross-build matcher that carries the address-keyed records (pairs, stubs, the three
  reviewed files, the layouts) from 24A446 to 24B156 and reads every check again from 24B156's bytes. Rules in the
  file's header (shape, walk, slot, order; inside-function addresses by the same offset or the shapes' LCS).

Helpers started 2026-11-25 (T3 delegate tasks, each on its own branch; they report to the agent that started them):
- app: fork akshaynexus/OneOS, worktree `/home/akshay/Documents/GitHub/OneiOS-wt/ios272-app`, branch
  `ios272/app`: Make iOS image offers both builds (sizes 10.59 / 10.91 GiB, room 30 / 32 GiB), Update the kernel
  pairs the staged set's build's kb, names follow the staged set.
- userland: worktree `/home/akshay/Documents/GitHub/OneiOS-wt/ios272-user`, branch `ios272/userland`: per-build
  userland tables (`scale_fixups`, the Metal plug-in's staged edits, the camera plug-in, both targets in the
  package, make-image's target pick), from 24B156's SystemOS image.

The contract both were given: `runtime.json` keeps `"kernel"` (24A446) and adds `"kernels"`, an array of kernel
objects with `"build"` (24B156's at `kernel/24B156/xnu.macho`, profile `ios-27.2.1-24B156`); the app's
`assets/kb/` stays 24A446 and `assets/kb-24B156/` is added (format 2), staged at `<stage>/kb-24B156`; make-image
gets `profile`, `xnu` and `kb` of the chosen build; `serve prelink` takes the staged set's build's kb and kernel.

Left (in order):
1. Run the matcher and look at its counts:
   `tools/oneios-tool/target/release/oneios-tool prelink port --from out/kernel-inputs/24A446/kernelcache.macho
   --to out/kernel-inputs/24B156/kernelcache.macho --map out/kernel/port-24A446-24B156.json --report
   out/kernel/port-24A446-24B156.report.json --record kernel/prelink/pairs-24A446.json=kernel/prelink/pairs-24B156.json
   --record kernel/prelink/stubs-24A446.json=kernel/prelink/stubs-24B156.json --record
   kernel/prelink/reviewed-24A446-v54.json=kernel/prelink/reviewed-24B156-v54.json --record
   kernel/prelink/lead/reviewed-lead-24A446-v54.json=kernel/prelink/lead/reviewed-lead-24B156-v54.json --record
   kernel/prelink/user/reviewed-user-24A446-v54.json=kernel/prelink/user/reviewed-user-24B156-v54.json`
   (check `early_security_init` lands on `0xfffffe000927b430`, the wrapper derived by hand on 2026-11-20).
2. The layouts per build: `prelink classes` / `kernel layout hid|net` / the aes and gpu records against 24B156
   (one class per run; `--prelink-report out/kernel/24A446/kernel.kc.prelink.json` only gives the slide
   0x1f000000000, to be checked again on 24B156's kernel.kc), the display record by `prelink port` plus reading the
   fields again (the 8 bytes moved them); the generators (`tools/oneios-tool/crates/oneios-kernel/src/build_oneios_*.rs`)
   read `<Class>-<build>-v54.json` by the build of the OS profile instead of the fixed 24A446 names.
3. `kernel/lib/stage.sh`: `OS_PROFILE=${ONEIOS_OS_PROFILE:-ios-27.0.1-24A446}`, and build-hosted.sh writes it into
   the device copy's `board.toml`; then `HOSTED_OUT=ios272 ONEIOS_OS_PROFILE=ios-27.2.1-24B156 ONEIOS_CRYPTO=original
   kernel/build-hosted.sh` in tmux; then `kernel/prelink/build.sh` with `BUILD=24B156` and the ported records: audit
   0 failures, `prelink stubs-check` 0 bad; review every record the port marked.
4. kb for 24B156 (the fork's `assets/kb-24B156/`), `runtime.json` `"kernels"` (runtime/package.sh), serve's
   make-image default kernel and kb by the profile's build; the second release kernel (`kernel/release-kernel.txt`,
   docs/releases.md); review and merge the two helpers' branches.
5. The Mac's phone boot of 27.2.1 (make the image on the phone: this laptop has no room for the OS image).

No tmux job of this work is running (`tmux ls | grep ios272`: none). Logs: `logs/ios272-*.log` in the worktree.
