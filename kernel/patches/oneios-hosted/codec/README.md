# codec/: the video encoder and decoder (hevc-encoder, hevc-guest, 2026-11-15)

Owner: the encoder agent. The host side: `runtime/bridges/encoder`. The whole path and the route: `docs/hardware.md`,
section 7g ("Video encoder").

**State (2026-11-15, hevc-guest):** the archive is **built into `kernel.kc`** by `oneios-tool kernel gen codec`
(`kernel/build-hosted.sh` runs it after `audio`), and a PC boot with `--set encoder=auto` publishes the service.
Built and booted on the Linux PC; on main (0081, 0082) built and prelinked on the Mac on 2026-11-22 (`kernel.kc`
`5add18fa...`). **On the phone (2026-11-22)** that kernel encoded nothing: iOS 27 writes HEIC through VideoToolbox's
**tile session**, which upstream's host does not have ("The tile session", below). The staged host now has it;
built and prelinked on the Mac, host-tested, **not yet booted on the phone**. The results of the builds and of the
boots are at the end of this file.

## Why this route

iOS's VideoToolbox in 24A446 carries a whole **paravirtualized** encoder and decoder
(`ParavirtualizedVideoEncoder_*`, read in the 24A446 shared cache) and uses them for every eligible codec once
`VTParavirtualizationIsRunningInGuest` — i.e. once the service **`AppleVideoToolboxParavirtualizationDriver`** is
published (string in `VideoToolboxParavirtualizationSupport` 71.0.14). CMPhoto's HEIC writer takes that path: its
`_CMPhotoSupportsHEVCHWEncode` block calls `VTSelectAndCreateVideoEncoderInstance` with
`kVTVideoEncoderSpecification_EnableHardwareAcceleratedVideoEncoder` true and reports HEVC hardware support when a
hardware encoder answers (disassembled, 2026-11-15). The paravirtualized encoder answers
`UsingHardwareAcceleratedVideoEncoder` true, so CMPhoto sees hardware HEVC and `CMPhotoCompressionSessionAddImage`
with `codec: 'hvc1'` stops failing.

So the camera's plug-in route is **not** used here: no AVE2 user-client emulation (AVE2's firmware command ABI is
Apple's and not serveable by another encoder), no `.videoencoder` bundle of ours. We publish Apple's own
paravirtualization service, exactly as the Metal and camera plug-ins are iOS-side code built on Apple's own loaders.
[F] the cache reads; [I] that iOS's encode graph takes the device as it is (a session on the phone confirms).

## The sources (UniXNU-droidvm f9121cd, `external/UniXNU-droidvm/package`)

| File | What |
|---|---|
| `DroidVMVirtioMedia.c` | the virtio-media transport (V4L2 stateful codec over virtio-mmio): `/arm-io/virtio-media-0`, `media-host-pool`. The device is xnuhost's (`runtime/bridges/encoder/media_device.c`), not crosvm's. Unchanged, `-DDROIDVM_DEVICE_DYNAMIC=1`, Mach universe, as its iPadOS 27 board builds it |
| `DroidVMVTHost.cpp` | the session logic: `encs`/`+enc`/`esta`/`efrm`/`Eemt`/`ecfr` (and, staged, iOS 27's tile session `ests`/`epti`/`etil`/`ecti` with `TEsa`/`TEcd`/`TEem`); the IOSurface lookup (IOCoreSurfaceRoot), NV12 copy into the OUTPUT buffer, hvcC/avcC assembly, the sample-buffer atoms. Runs in the guest kernel over the virtio-media device. **Staged with four edits** (below) |
| `DroidVMVTParavirt.cpp` | Apple's `AppleVideoToolboxParavirtualizationDriver` user client + the `IODataQueue` transport; publishes the service with `droidvm-vt-paravirt=1`. Unchanged; its static initializer is taken out of `__mod_init_func` and made global (`droidvm_vt_paravirt_attach` runs it in its own module load), as the audio archive's |

`OneiOSCodec.c` (this folder, OneiOS's own) is the start: `oneios_codec_start` → `droidvm_media_start()` +
`droidvm_vt_paravirt_attach()`, the start line, and the report thread.

**The edits** (`tools/oneios-tool/crates/oneios-kernel/src/staged/codec.rs`; the file in `external/` stays as it is):

1. The hvcC's twelve bytes of profile, tier and level (below).
2. iOS 27's **tile session** (2026-11-22, "The tile session" below): `ests`, `epti`, `etil`, `ecti` from the guest,
   `TEsa`, `TEcd`, `TEem` to it.
3. A **small HEVC session** is coded at 96x96 at least (c2.qti's smallest) and cropped back by its SPS's conformance
   window.
4. `jcap` (the paravirtualized JPEG session's capability query) is answered -12908: there is no JPEG codec here.

The hvcC's twelve bytes of profile, tier and level. Upstream copies them from the SPS as the stream has them
(`memcpy(out + at, s->sps + 3, 12)`). Every
HEVC SPS has emulation prevention bytes in exactly those bytes (Main's compatibility flags `60 00 00 00` are
`60 00 00 03 00` in the stream, and the constraint flags' zeros get two more), so the copy put `03` bytes into the
flags and never reached `general_level_idc`: the hvcC said level 0 for any real encoder's stream. The staged copy
takes the twelve bytes without them. [F] read from the code and the standard; the stand-in's own SPS has the same
three bytes (`runtime/bridges/encoder/tests/test_standin.c` checks its first two).

## The tile session (2026-11-22)

**The fault [F, the phone, main 203a9e7, `out/kernel/codec-24A446/kernel.kc`, `encoder=auto`]:** the host encoder
worked (`encctl hevc` and `encctl h264 640 480` passed; `encoder: on: mediacodec-helper backend`; `ONEIOS CODEC: device
0: oneios-encoder caps ... out: NV12 cap: HEVC H264`), and no frame was ever encoded. `xnuhost.log`
(`out/session-logs/2026-11-22/kbd-debug/live11.log`): `counters: ... messages 109 (refused 0, unknown 27) ...;
sessions 26, frames in 0 out 0, errors 0` at 12442 s, `messages 469 (unknown 117) ...; sessions 116` at 12472 s:
**3 sessions a second**, each `videocodecd '+enc' 281 bytes, 'ests' 175 bytes, 'einv' 48 bytes, '-enc' 48 bytes`.
iOS's log of that boot (`out/session-logs/2026-11-22/s7-ioslog`, `oneios-tool phone pull-log`): videocodecd's
`ParavirtualizedVideoEncoder_CreateInstanceWithSpecification: ... codecType hvc1 ... guestProtocolVersion 6
hostProtocolVersion 6`, then `ParavirtualizedVideoEncoder_StartTileSession: (encoder ...) StartTileSession failed
with error -12900 for 84 x 84`, again and again; chronod's 116 `CMPhotoCompressionSessionAddImage: err = unknown error
[-12900] (codec: 'hvc1')` (white widgets). SpringBoard's one `'jcap' 48 bytes` was the first unknown.

**The cause [F, read in 24A446's VideoToolbox, extracted from `out/dsc-24A446-full` with `ipsw dyld extract`]:**
upstream's exchange was recorded against macOS's host and its dispatch has no tile messages, so `ests` got the
unknown type's -12900; VideoToolbox gives such a session up (`einv`, `-enc`) and CMPhoto tries again. 24A446's own
host (`___VTParavirtualizationHostEncoderSessionDeliverMessageFromGuest_block_invoke`) dispatches `esta eset emul esup
ecop epfr efrm emif ecfr ebps eeps esav` and the tile session's `ests epti etil ecti`. The tile messages, as its host
(`vtParavirtualizationHostEncoderSession_HandleEncoder{StartTileSession,PrepareToEncodeTiles,EncodeTile,
CompleteTiles}`, `VTParavirtualizationHostEncoderSession{SetTileAttributes,CreateTileVideoFormatDescription,
EmitEncodedTile}`) and its guest (`ParavirtualizedVideoEncoder_*Tile*`, `paravirtualizedVideoEncoder_
HandleMessageFromHost`) write and read them:

| Message | Parameters | Answer |
|---|---|---|
| `ests` (StartTileSession) | `,twh` the tile size (VTInt32Size, type `=32s`: width, height), `,esp` the session's properties | `,err` |
| `TEsa` (host to guest) | `,twh`, `,pba` the tiles' pixel buffer attributes | the guest's `,err` (not waited for) |
| `epti` (PrepareToEncodeTiles) | `,ept` | `,err` |
| `etil` (EncodeTile) | `,til` the tile's ID (s64), `,tio` its origin in the surface (`=32p`: x, y), `,tia` its size, `,tip` options, `,uui` the pixel buffer's UUID, the IOSurface | `,err`, `,efl` (1: asynchronous) |
| `TEcd` (host to guest) | `,cod ,wdt ,hgt ,ext`, as `Efds` | `,err`, `=fds` |
| `TEem` (host to guest) | `,til ,err ,efl ,sbf ,uui`, as `Eemt` with `,til` for `,frm` (pieces flagged `0x10`) | `,err` |
| `ecti` (CompleteTiles) | none | `,err` once every tile is handed over |

`TEsr` (the host's tile encode requirements, `,cba ,ter`) is optional and not sent. The frame path's keys are the
same in 24A446 as upstream's (`efrm` answered `,err ,inf`).

**The fix (staged `DroidVMVTHost.cpp`, edits 2 to 4):**
- `ests` opens the encoder at the tile size and sends `TEsa` (420v, as `Epba`); `epti` is answered; `etil` looks the
  surface up as `efrm` does (one shared lookup now), takes its part (`,tio`, `,tia`; a 420v or 420f surface) and
  queues it as a frame with no times; the worker copies that part and the phone's encoder codes it (every frame of
  it is a key frame: `i-frame-interval` 0), then `TEcd` once per new parameter sets and `TEem` per tile; `ecti` is
  answered as `ecfr` (the drain, the 2 s deadline, a tile not coded by then handed over dropped).
- An HEVC session below 96x96 (chronod's tiles are 84x84; c2.qti's encoders code nothing smaller) is coded at 96 in
  that direction: the copy repeats the last column and row, and the codec's SPS gets the conformance window that
  crops the picture back to the session's size before it goes into the hvcC (`oneios_hevc_sps_crop`). The SPS is
  compared as the codec wrote it, so `TEcd` is sent only when it changes.
- `jcap` (`VTParavirtualizedJPEGSessionCreate`'s capability query, `,jca` the answer) gets -12908: no JPEG codec here,
  and the guest keeps its own JPEG code. 24A446's own host without CMPhoto's JPEG hardware answers nothing either.
- The counters line names the last unknown type (`the last 'TYPE'`), so a type of a later iOS shows at once.

The copy and the crop are `OneiOSCodecPicture.h` (this folder, plain C), which the staged file includes (`-I` this
folder) and `kernel/tests/host.sh` tests on the host: `tests/test_picture.c` (17 checks: tiles inside, at the edges,
coded larger, parts outside; SPSs built field by field with and without a window of the codec's own, with
sub-layers, with emulation prevention bytes, read back) and `tests/crop.sh` (ffmpeg 9.0.2's libx265 and
hevc_videotoolbox code 96x96 and 128x96, the SPS is cropped to 84x84 and 120x90, ffprobe reads the new size and
ffmpeg decodes, with `-err_detect explode`, exactly the top-left part of the picture: 4 streams on the Mac).

## The generator and the build

`oneios-tool kernel gen codec` (`tools/oneios-tool/crates/oneios-kernel/src/build_oneios_codec.rs`; written in Rust,
no Python original) is one entry of the hosted build's board archives, after `audio`:

- compiles `DroidVMVirtioMedia.c` and `OneiOSCodec.c` with the first kernel pass's Mach record, the staged
  `DroidVMVTHost.cpp` and `DroidVMVTParavirt.cpp` with its IOKit record (`-I` the package folder; the staged file also
  `-I` this folder, for `OneiOSCodecPicture.h`);
- makes `DroidVMVTParavirt.late.o` (no `__DATA,__mod_init_func`, `__GLOBAL__sub_I_DroidVMVTParavirt.cpp` global);
- archives `build/oneios-codec/liboneioscodec.a` (4 objects) and requires `_oneios_codec_start`,
  `_droidvm_media_start`, `_droidvm_vt_paravirt_attach`, `__GLOBAL__sub_I_DroidVMVTParavirt.cpp`,
  `_droidvm_vthost_available`, `_droidvm_vthost_message`, `_droidvm_media_ioctl`, `_droidvm_media_mmap`, and that
  neither C++ object still has a `__mod_init_func` section;
- writes `objects.json` (each source with its SHA-256, `OneiOSCodecPicture.h` among them).

`kernel/build-hosted.sh` adds `-Wl,-force_load,{tree}/build/oneios-codec/liboneioscodec.a` to
`CONFIG_XNU_EXTRA_LD_ARCHIVES` (after `audio`'s). `ONEIOS_CODEC_RECORDS` and `ONEIOS_CODEC_OUT` name another record
folder and output folder: the generator against a finished build's tree, without a kernel build.

Tests: `cargo test -p oneios-kernel --release --test golden` (`the_codec_edit_writes_its_frozen_text_and_fits_the_
pinned_source`: the edit list on a made-up source against `tests/kernelgen/golden/synthetic/codec.out`, and on the
real file: each tile message has its case, each helper is defined before its caller);
`kernel/tests/host.sh` (`OneiOSCodecPicture.h`: `tests/test_picture.c` and `tests/crop.sh`, above);
`tools/oneios-tool/tests/kernelgen/compare-golden.sh WORK` after a build (the staged file's hash, the generator's
line).

## Boot arguments and log lines

| Boot argument | What |
|---|---|
| `oneios-codec=0` | nothing of this archive starts (`ONEIOS CODEC: off (oneios-codec=0)`) |
| `droidvm-media=0` | no transport (no device is looked for) |
| `droidvm-vt-paravirt=1` | publish the service. xnuhost adds it when `encoder=auto` or `encoder=sim` gave the guest a device. Without it the classes are registered (stage 1) and root can publish later: `sysctl kern.droidvm_vt_paravirt=1` |
| `oneios-codec-report=0` | no report thread |

- At start, always: `ONEIOS CODEC: N virtio-media device(s), M up; VideoToolbox paravirtualization registered (stage
  2)` with a device and `droidvm-vt-paravirt=1`; `... not published (stage 1)` without the boot argument or without a
  device (a kernel booted with `encoder=off`: `0 virtio-media device(s), 0 up`).
- With a device, 3 s later: `ONEIOS CODEC: device 0: oneios-encoder caps 0x4004000 out: NV12 cap: HEVC H264` (the
  transport's own start-up check: a session opened, each queue's formats listed, the session closed).
- Every 30 s while a figure moves (nothing while nobody uses the codec): `ONEIOS CODEC: counters: service opened N
  closed N, messages N (refused N, unknown N, the last 'TYPE'), to the guest N (dropped N), replies N; sessions N,
  frames in N out N, errors N (last: step S value V); virtio-media commands N (timeouts N), events N errors N buffers
  N other`, then `ONEIOS CODEC: session: ...` for each line `DroidVMVTHost.cpp` wrote into its trace ring since (a
  session's size, `tiles` for a tile session, the size the device codes, the codec; the IOSurface lookups).
  `the last 'TYPE'` (2026-11-22) is the last message type the host has no case for (`droidvm_vthost_last_unknown` as
  four characters, `'none'` before the first): a type a new iOS sends shows on this line at once (iOS 27's `'ests'`
  was found from the message lines). `step S` is the stage `DroidVMVTHost.cpp` gives `failed()`: 90-96 the session's
  start (90 OPEN, 91/92 S_FMT, 93/94 REQBUFS, 95/96 STREAMON), 10-12 a buffer's QUERYBUF, MMAP and mapping, 40-42 and
  97 the IOSurface lookup, 84 the frame's copy, 85 its QBUF, 80/81 the format description, 86 a coded frame nobody
  waits for, 20 a message that does not fit; staged: 100 a tile's parameters (`etil` without `,til`/`,tio`/`,tia`/
  `,uui`, or larger than the tile size), 101 a tile's surface not 420v/420f, 102 an SPS whose conformance window
  could not be written (a small HEVC session: no description, so no tile is handed over).

## What the transport costs when idle

`DroidVMVirtioMedia.c`'s thread looks at the event queue's used ring every 50 ms (every 1 ms while a session
streams); that is a read of guest memory, no trap. xnuhost's side sleeps until a command or a kick with no session
open (`media_device.c`).

## Limits [F from the code]

- A session's buffers come from its half of the 48 MiB media pool (`MEDIA_SLOTS` 2): 4 NV12 frames and 6 coded
  buffers of width x height / 2 + 64 KiB each, about 9 bytes a pixel. A frame above about 2.7 megapixels is refused
  at REQBUFS (`errors`, step 93 or 94) and that session gets no encoder. CMPhoto encodes HEIC as tiles: a tile
  session is opened at the tile size (84x84 for chronod's widget snapshots on 2026-11-22, coded at 96x96), not the
  picture's.
- Two sessions at a time; a third `OPEN` fails (step 90) until one closes.
- No decoder: the device's card is `oneios-encoder`, so `decs` is empty and iOS keeps its own decoders.

## What a session on the phone must show

Kernel (`xnuhost.log`): `encoder: on: mediacodec-helper backend, media pool 48 MiB at ...`; `ONEIOS CODEC: 1
virtio-media device(s), 1 up; VideoToolbox paravirtualization registered (stage 2)`; `ONEIOS CODEC: device 0:
oneios-encoder ...`; then `ONEIOS CODEC: counters:` with `sessions` and `frames out` above 0 and `errors 0`, and
xnuhost's `encoder: session N: HEVC WxH on mediacodec-helper`.

With the tile session (2026-11-22): the message lines `'+enc'`, `'ests'`, `'epti'` (when VideoToolbox prepares the
tiles), `'etil'` (one per tile, with a surface: `1 surface(s)`), `'ecti'`, then `'einv'`, `'-enc'`; `sessions`
growing slowly (once per snapshot, not 3 a second); `frames in N out N` with the same N above 0; `unknown` not
growing and `the last` not `'ests'`;
`ONEIOS CODEC: session: encoder session N 84x84 tiles coded 96x96 hvc1`; xnuhost's `encoder: session N: HEVC 96x96
on mediacodec-helper` (the device's size).

iOS's log (`oneios-tool phone pull-log DIR`, then `oneios-tool ioslog health` and `checks`):
- **no** `StartTileSession failed with error` from videocodecd;
- **no** `CMPhotoCompressionSessionAddImage ... (codec: 'hvc1')` / `kCMBaseObjectError_UnsupportedOperation` /
  `unknown error [-12900]` errors: `encode-fail 0` in `ioslog-health.txt`, the `heic` row of `ioslog checks` 0;
- WidgetRenderer_Default **without** `Content load failed: unable to find or unarchive file for key: ...systemSmall...`;
- `oneios-gpu-pixels: WidgetRenderer_ ... drawn`;
- a screenshot saved as HEIC (its first bytes `....ftypheic`) [I: that iOS 27 saves a screenshot as HEIC].

## Built and booted on the Linux PC (2026-11-15)

- **Built [F]:** `origin/main` `fb4ec88` plus this work, `HOSTED_OUT=hevc XNU_FILESET_REFERENCE=1
  kernel/build-hosted.sh`, 716 s from an empty tree: `xnu.macho` `972b9132...` (24,961,392 bytes), the step
  `archive-13-oneios-tool` (`build-oneios-codec: .../liboneioscodec.a (4 objects: ...)`), `_oneios_codec_start` the
  archive's and called by `_oneios_drivers_start`. `kernel/prelink/build.sh`: `kernel.kc`
  `e4ffcabd1d95f25eb542b7226be3235ac2317743d667f22dcc094a5b59198d8b` (96,141,312 bytes), `audit: 2711 paired, 93
  functions on the stub, 9 data unpaired; 0 failures`; `oneios-tool prelink stubs-check`: 666 good, 0 bad.
- **Booted [F], the first 40 s only:** a PC boot (`qemu-aarch64`, `--mode bench --ram 4G --set wifi=sim --set
  encoder=auto`, a script that swipes Home every 2 minutes) printed

  ```
  xnuhost I encoder: on: stand-in backend, media pool 48 MiB at 0x20fbe04000 (2 session slots)
  xnuhost I encoder: the stand-in encoder (encoder=auto with the bridges off: no phone): no MediaCodec; every frame is coded as it is (I_PCM), no compression
  guest     ONEIOS CODEC: 1 virtio-media device(s), 1 up; VideoToolbox paravirtualization registered (stage 2)
  guest     ONEIOS CODEC: device 0: oneios-encoder caps 0x4004000 out: NV12 cap: HEVC H264
  ```

  (the last line is the guest transport's own check against xnuhost's device: a session opened, ENUM_FMT on both
  queues, the session closed). Then all work was stopped by the owner and the boot was ended.
- **Not done:** the rest of that boot (whether a process opens the service and encodes: the `ONEIOS CODEC: counters:`
  and `message` lines), the count of chronod's `CMPhotoCompressionSessionAddImage ... 'hvc1'` errors and of
  WidgetRenderer's `Content load failed` in iOS's log, and the same boot without `encoder=auto` to count against.
  Three finished PC boots of other work (main's kernel, no home script) have **none** of either line: without a GPU
  the PC's widgets may never reach the snapshot, so the PC may not be able to show that count at all [I]; the phone
  can. On 2026-11-20 (b5cf905's message) a 10-minute PC boot of the hevc-guest merge (`HOSTED_OUT=hevc`) with
  `encoder=auto` ran clean: the codec registered, cameracaptured's `encs` was answered, no panic; the `hvc1` count was
  not recorded. The next step is the phone, with the Mac build below (no PC boot was run for it).

## On main, built on the Mac (2026-11-22)

PR #59 (`hevc-guest`) merged onto main with 0081 and 0082 (`fix-dyld-cache-unnesting`). The archive is built after
`audio` and force-loaded last of the hosted archives (`oneios-tool kernel hosted-archives`, `FORCE_LOADED` in
`tools/oneios-tool/crates/oneios-kernel/src/hosted_device.rs`: gpu aes keystore net hid audio codec); it has no
numbered patch, so the series is unchanged and 0081 and 0082 apply as on main. b5cf905's 0080 change is not taken:
main's 65b7bd4 already anchors the same `vm_shared_region.c` hunk.

- **Built [F]:** `HOSTED_OUT=codec kernel/build-hosted.sh` (no reference kernel), 492 s on the Mac: `xnu.macho`
  `baad846266253a2f...` (25,630,144 bytes); the step `archive-13-oneios-tool` printed `build-oneios-codec:
  .../liboneioscodec.a (4 objects: ...)`; `_oneios_codec_start`, `_droidvm_media_start`,
  `_droidvm_vt_paravirt_attach` and 0082's `_hosted_dyld_flags_init` are in it.
- **Prelinked [F]:** `KERNEL=out/kernel/current-hosted-codec/xnu.macho OUT=out/kernel/codec-24A446
  kernel/prelink/build.sh`: `kernel.kc` `5add18fab1cb64ce5b6cea3f8349dde12aed125b88eaa4fe041a7366ac1a3a8a`
  (96,845,824 bytes), `audit: 2712 paired, 92 functions on the stub, 9 data unpaired; 0 failures`; `oneios-tool
  prelink stubs-check`: 666 good, 0 bad. The same symbols are in the collection.
- **Host tests [F]:** `runtime/host-tests.sh` all parts pass (xnuhost 14229 checks), `kernel/tests/host.sh` 7 of 7,
  `cargo test -p oneios-kernel` (golden 14 of 14).
- **Fixed with it:** xnuhost dropped the image set's `encoder=` (read, then not copied back, as `pen=` was): the OneOS
  app passes no `--set`, so a phone session could not turn the encoder on. `runtime/bridges/encoder/README.md` "On
  the phone" has how to set it now.
- **On the phone (2026-11-22 23:50, kernel `out/kernel/combo-24A446` `5baab993...` = main 23561aa with 0082, 0083 and
  the tile messages; `encoder=auto`):** `encctl hevc 96 96` ok before the session; `ONEIOS CODEC: 1 virtio-media device(s), 1
  up`; `counters: ... unknown 0, the last 'none'` (the earlier boot's ests/einv/-enc loop is gone: `sessions 1`, not 3 a
  second); iOS's log (`oneios-tool phone pull-log`): `encode-fail 0` (116 `CMPhotoCompressionSessionAddImage ... err =`
  the boot before), no `StartTileSession failed`; the owner: "widgets now render". Not yet seen: `frames in`/`out` above 0
  in the counters (no tile was sent in that boot), so the HEVC encode itself is still unproven on the phone.

## The tile session, built on the Mac (2026-11-22)

- **Built [F]:** `HOSTED_OUT=codecfix kernel/build-hosted.sh` on main 63f05a6 plus this work, 266 s: `xnu.macho`
  `1325991ff5d6a698...` (25,653,624 bytes); `archive-13-oneios-tool.log` has `build-oneios-codec: ... (4 objects:
  ...)`; the staged `DroidVMVTHost.cpp` `6a07f479...` (94,344 bytes; `tests/kernelgen/golden/staged.sha256`), no
  compiler warning; `droidvm_vthost_last_unknown` is in the archive.
- **Prelinked [F]:** `KERNEL=out/kernel/current-hosted-codecfix/xnu.macho OUT=out/kernel/codecfix-24A446
  kernel/prelink/build.sh`: `kernel.kc` `852adb8be21b525d7ba61dc857b120304f9f9d7f1081ff3d3a711e6eb3585dfb`
  (96,845,824 bytes), `audit: 2712 paired, 92 functions on the stub, 9 data unpaired; 0 failures`; `oneios-tool
  prelink stubs-check`: 666 good, 0 bad. No pairing record changes.
- **Host tests [F]:** `cargo test -p oneios-kernel -p oneios-ioslog` (golden 14 of 14, ioslog 15 of 15 with the
  `encode-fail` name test); `kernel/tests/host.sh` 8 of 8 (`test_picture` 17 checks, `crop.sh` 4 streams);
  `compare-golden.sh` on the build: the 11 staged hashes and the codec line the same (the Wi-Fi archive's
  `report.json` and line differ on the `codec` tree of before this work too: not this change).
- **Not done:** the phone boot: stage `out/kernel/codecfix-24A446/kernel.kc` and `kernel.kc.hashes` with
  `encoder=auto` in `xnuhost.conf` ("What a session on the phone must show", above). Not known before it [I]: that
  CMPhoto gives the tiles in 420v or 420f surfaces after `TEsa` (step 101 says the format if not), and that c2.qti
  takes 96x96 (`encctl hevc 96 96` tells beforehand).
