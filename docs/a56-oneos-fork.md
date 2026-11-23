# A56 work that belongs to the OneOS app fork (proposal, not files here)

> **Applied 2026-11-23 (OneiOS issue 73)**, section 1 only, in the fork's `oneios` branch: the iOS kind offered on an
> SM-A566, Make iOS image with `profiles/ios-16.5-a56.toml`, the switch's A56 lines (docs/a56.md "In the OneOS fork").
> The 20F66 session settings went into this tree instead of the switch (the host table, the image's `xnuhost.conf`),
> so the switch passes nothing of the A56's own. Sections 2 and 3 (the bootstrap, the in-app download of a prebuilt
> image) are not taken: the A56 makes its own image from Apple's file, as the S24 Ultra does. Untested on an A56.

`app/` in this repo is a submodule of the separate OneOS app fork — `port-a56` touches nothing in it.
Everything below lives in `Musicalista/OneiOS-A56` and needs the owner's decision before it (or its idea)
moves to the OneOS fork. Structural note first: the A56 "app" is a **standalone side-app** (`app/` is a plain
directory there: `AndroidManifest.xml`, `src/Manager.java`, `build.py`, icons), not a patch to the OneOS fork's
app. Taking it as-is would fork the app; the proposal below says per item what to lift and where.

## 1. `oneios_switch_a56.sh` → the fork's `switcher/assets/oneios_switch.sh`

Full text: `Musicalista/OneiOS-A56:scripts/phone/oneios_switch_a56.sh` (1382 lines, modelled on the old
in-repo `oneos_switch.sh`). This repo has no switch script anymore (it lives in the fork), so there is no
base here to diff against — port by cherry-picking the hunks below into the fork's script:

- **Device gate**: `SM-S928*:e3q:SM8650|SM-A566*:a56x:s5e8855` on `ro.product.model:ro.product.device:ro.soc.model`.
  (At the `app/` pin bdf3db0 the fork already warns instead of refusing on other phones, since 8b27d35; `Profiles.kt` has an
  `a56` profile. What is left there: `PROFILE_IDS` is the S24 Ultra only, and `OneiosPlain.kt` still says "It runs on the
  Galaxy S24 Ultra (SM-S928) only".)
  The fork decides whether A56 joins the supported list or stays refused (OneOS #12's warned-not-refused).
- **20F66 session block** (`--inner`, when `conf /data/image.conf build` is `20F66`): `--display shm --gpu 2d`,
  `--width/--height` from `ONEIOS_FRAME` (default `1080x2340`), `--hz 120 --scale 3`, the full 20F66 boot-args
  line (`vgpu-3d=0 droidvm-gpu-metal=0 … oneios-gpu-metal=0`, overridable via `ONEIOS_GPU_METAL=0|1`),
  `oneios-mac-exec-skip=mediaserverd,TVRemoteConnectionService`, the two `--set text-fixup=` entries, and
  `ONEIOS_AMDGPU_RENDER_NODE=/dev/dri/renderD128` in the environment. Trace boot-args stay out unless
  `ONEIOS_DIAG_BOOTARGS` names them. `ONEIOS_FRAME=panel` takes the panel's mode.
- **Do NOT take**: the A56 hardcodes `ram 4096M --set ram-giveback=0 --set ram-prefault=1 --set ram-lock=1
  --set wifi=sim` and `renice`/performance-governor pinning in the script — the owner's rule wants those as
  session policy, not A56-only lines; the fork should pass the same sets for every session or make them
  per-device config.
- **Do NOT take**: `oneos/interface.md`'s 600→1800 s return-timer bump was A56-only. If 20F66 sessions need
  longer, that is a fork session-policy call, not a guest-repo change.

## 2. Self-extracting bootstrap → the fork's app + stage scripts

`Manager.java` (~860 lines) + `app/build.py` + `app/assets/bootstrap.tar.gz`: on every start the app compares
the asset tarball's SHA-256 with `/data/local/tmp/oneios/.bootstrap` (as root) and, when they differ and no
session is open, replaces `bin/`, `runtime/`, `runtime.conf`, `runtime.sha256` and the three scripts; the
image set and session data are never touched. `build.py` step 0 refuses a bundle whose
`bin/oneios_root.sh` differs from the `ROOT_SHA` pin in `Manager.java`, whose scripts lack `#!`/have BOM/CRLF.
What to lift: the pin check + the compare-and-replace flow into the fork's `app stage-scripts` path
(`scripts/phone/stage.sh` stages through `oneios_root.sh begin/receive/finish` here); the A56 asset layout
(`bootstrap.tar.gz` with the scripts inside) differs from the fork's, so this is an idea + protocol, not a patch.

## 3. In-app download / import / staging → the fork's app

Download: part list from the GitHub API (`releases/tags/v1.0.0`, assets `ios16.5-a56-image.tar.gz.partNN`),
HTTP `Range` resume into the app's own folder, skip complete parts; import: package list read as root from
`/sdcard/Download`; extract: parts streamed through `cat | tar -xz` into `image.new/` (no merged copy),
single-top-folder moved up, then replace `image/`; then the stage script writes `image.conf`/`image.sha256`/
`image.ids` (`tools/stage_ios16.sh` in the A56 tree — shell, runs on the phone). Note the A56 staging writes
`image.conf` (`format=1`, `version`, `build`), which only the A56 switch script reads; the fork's session
contract (`docs/releases.md`, `oneos/interface.md`) has no `image.conf` yet — adding one is the fork's call.
`oneios-tool phone stage image` + ` sums` already cover the sha/ids half from a PC.

## 4. Do not lift

- `tools/transfer_apfs.py`: hardcoded Windows paths of the proposer's PC (`C:\Users\Usuario\...`), plain
  `adb push` chunks; covered by `oneios-tool phone` pull/push. Left out.
- Icons, launcher art, `REDDIT_ANNOUNCEMENT.md`, Portuguese UI strings, `test.jks`, the built APKs and
  `bootstrap.tar.gz` binaries: A56 product/marketing, not guest-repo material. No Apple files either way.
- `oneios_switch_a56.sh`'s `renice -20`/governor pinning and the 1125x2436 experiment (`docs/a56.md`): phone
  policy and an open question, respectively — recorded, not proposed.

## Applying this proposal

1. Owner decides: standalone A56 manager vs ideas-into-the-OneOS-app (recommended: the latter; one app).
2. Fork PR 1: device gate + 20F66 session block in `oneios_switch.sh` (hunks above), with its stand-in tests.
3. Fork PR 2: bootstrap pin + compare-and-replace in the app's stage path.
4. Fork PR 3 (optional): in-app download/import/staging; needs the `image.conf` contract decision first.
5. This repo's side is done when those land: `docs/forks.md` A56 row flips to "in the OneOS fork".
