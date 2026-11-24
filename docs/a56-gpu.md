# The Galaxy A56's GPU: Metal on the Xclipse 540 (iOS 16.5, 20F66)

Owner's ask (2026-11-24): "port it fully and give it for the next update". GPU-drawn Metal for iOS 16.5 on the
Galaxy A56 (SM-A566E, Exynos 1580, Samsung Xclipse 540 = AMD RDNA3, host render node `/dev/dri/renderD128`, Linux
`amdgpu`/`sgpu`). Until this port the A56 runs `gpu=2d`: the CPU composites a 720x1560 @2x frame (v0.1.24,
[a56.md](a56.md) "The second port").

**State, plainly.** Nothing of this file has run on an A56: there is none here and no qemu boot (owner's rule). Each
section says what is built, what its host tests cover, and which A56 log lines would prove it. The S24 Ultra, iOS 27
and the Adreno path are unchanged by all of it: every part is behind the `galaxy-a56` host row, an amdgpu render
node, the image's build (`20F66`) or the A56's own Metal plug-in.

## 1. The design and its references

```
iOS 16.5 process: Metal -> the A56 plug-in (the AIR Metal layer of Jesvi-iOS, on Mesa Gallium)   section 4
   | Mesa radeonsi (ACO) -> amdgpu winsys -> amdgpu virtio native context (src/amd/common/virtio) section 3
   | vdrm on Darwin: IOKit AGXDeviceUserClient open type 0x4149, DROIDVM_VIRTGPU_* methods
hosted XNU: the accelerator and DroidVMVirtioGpu3D.cpp (virtio-gpu, capset 6, DRM context)      section 5
   | virtio-mmio /arm-io/virtio-gpu
xnuhost: src/virtio_gpu.rs -> src/gpu3d.rs -> src/gpu3d_amdgpu.rs (Rust)                         section 2
   | ioctls on /dev/dri/renderD128 (amdgpu), guest pages as udmabufs
```

The Adreno path is the same shape with Turnip, metal2vulkan and drm2kgsl ([gpu.md](gpu.md) 1); here there is no
Vulkan: Metal goes to Gallium directly, as in Jesvi-iOS.

| Reference | Pin (`scripts/setup/get-references.sh`) | Licence | What is taken |
|---|---|---|---|
| `jesvi-eulah/Jesvi-iOS` (iOS 16.5 on the Galaxy S20, Exynos 990 Mali-G77; HakiOS-based) | `a13e406` (main table) | MIT outside `xnu/` (its `LICENSE`, `NOTICE`); APSL-2.0 in `xnu/` | `tools/mesa/air/*.c,*.h` (the AIR frontend: AIR bitcode into NIR; `air_metal.c`, Metal's needs of Gallium), the Mesa patches that wire them (`air-frontend`, `air-metal`), `userland/src/redwoodmetal.m` and its plug-in plist (the Metal device classes): copied with their notices into our tree and credited (section 4). Not taken: `prebuilt/` (binaries), every `.py` tool (no Python here), the Panfrost and freedreno patches (another GPU), `xnu/` |
| `Musicalista/OneiOS-A56` (the other A56 fork) | `c83b9ca` (main table; the GPU work is its `e7d56f2`) | per file: `amdgpu_renderer.c` keeps AMD's MIT notice (virglrenderer's), `gpu3d_xclipse.c` is `SPDX MIT`; the repository states none | the protocol and the self-test's steps of `runtime/xnuhost/amdgpu/src/amdgpu_renderer.c` and `src/gpu3d_xclipse.c`, rewritten in Rust (section 2); the guest probe's steps (`userland/metal/amdgpu/`). No file copied as is |
| Mesa (`Droid-VM/mesa` `cecc0e9`, `mesa-droidvm`) | main table | MIT | radeonsi, ACO, the amdgpu winsys, `src/amd/common/virtio` (the guest half of the amdgpu native context, wire `amdgpu_virtio_proto.h`), `src/virtio/vdrm` |
| `Droid-VM/NativeAdreno` `627836d` | main table | MIT | `DroidVMVirtgpuABI.h`: the user client's virtgpu methods the guest's vdrm calls |
| libdrm 2.4.133 (the release Mesa cecc0e9's `subprojects/libdrm.wrap` pins) | sha256 `fc68f9d0ba2ea63c9432a299e14fea09fad7a8a66e8039fcd7802ca59f77b4f5`, fetched by `userland/metal-amd/build.sh` into `out/metal-amd/dl` (not `get-references.sh`) | MIT | `xf86drm.h`, `include/drm`, the amdgpu library (its VA manager) built for Darwin (section 3) |
| `Droid-VM-UniXNU` `patches/mesa/turnip-darwin-virtgpu.patch` | kernel base (`kernel/pins.sh`) | MIT | the model of a Darwin vdrm backend (`tu_knl_darwin_virtgpu.cc`) |
| `PixOS-Project/pixelios-kernel`, `pixelios-userland` (Pixel 6a, Mali via Panfrost) | `f084ce5`, `d5b2079` (extra table) | MIT | ideas only so far |
| `oosexgang/hakios-xnu-kernel` | `62e09f8` (extra table) | none stated | ideas only, read only |
| `open-mapple/mapple_gpu_mali` | `f893fed` (extra table) | GPL-3.0 | ideas only: no code into our MIT or APSL files |

**A limit of the design, found in Jesvi-iOS's own record (read 2026-11-24).** Jesvi-iOS's Metal runs per process:
CoreImage, Photos, Maps, posters and games through GL-on-Metal (`docs/SHIPPING.md`, 2026-11-14 and 11-16), with
"metaloff" for PosterBoard, mediaserverd and others, and its kernel "filters discovery for incompatible core
renderers". CoreAnimation's render server in backboardd on it crashed at start and was reverted
(`docs/devices/x1slte/display.md` line 72: "Trying CoreAnimation on Metal in backboardd live ... crashed it at start
(stack overflow ...): reverted"). The A56's low frame rate is backboardd's CPU composition, so this layer alone does
not raise it: backboardd on this Metal is open work (section 6).

## 2. Host: the Xclipse native context in xnuhost (`src/gpu3d_amdgpu.rs`)

(Filled by the host stage.)

## 3. Guest: Mesa radeonsi for Darwin over the virtio amdgpu context

**What is built (2026-11-24, build laptop):** Mesa `mesa-droidvm` cecc0e9 with `userland/metal-amd/patches/mesa`
(0001-0006; 0001-4999 are this stage's, `5xxx-air-*.patch` the Metal layer's, applied after them), cross-built by
`userland/metal-amd/build.sh` for `arm64-apple-ios16.5` (K4: the 20F66 stubs in `out/ios16-tbd`, SDK 26.5 from xtool,
ld64-956.6, clang 23): `-Dgallium-drivers=radeonsi -Damdgpu-virtio=true -Dllvm=disabled -Damd-use-llvm=false
-Doneios-radeonsi-screen=true`, release, no GL, EGL, GBM or Vulkan. Out of it, in `out/metal-amd/ios`:

| File | What |
|---|---|
| `liboneios_radeonsi.dylib` | radeonsi with ACO, its amdgpu winsys, Mesa's amdgpu virtio context and the Darwin vdrm backend in one dylib (17,368,448 bytes, not stripped) that exports one symbol, K3's `oneios_radeonsi_screen_create(why, why_size)` (`src/gallium/targets/oneios-radeonsi/oneios_radeonsi.h`); install name `@loader_path/liboneios_radeonsi.dylib` (it goes beside the plug-in that links it) |
| `oneios-amdgpu-probe` | the lasting guest tool (below), 17,405,216 bytes |
| `oneios-radeonsi.build.txt` | the SDK, target, stubs, linker, compiler, the Mesa and libdrm pins, each patch's `cksum`, sizes, CDHashes |

Both are arm64, minos 16.5, signed by `oneios-tool ios sign` (4 KiB pages) and pass `oneios-tool ios macho-check --arch
arm64 --tbd out/ios16-tbd` (205 and 206 imports, every one exported by a 20F66 library: libSystem, libc++, libz, IOKit).

**ACO without LLVM works at this pin**, with two patches: radeonsi's gfx/compute code is built only with a GL, Vulkan,
CL or video-encode frontend (`with_gfx_compute`), so the new `-Doneios-radeonsi-screen=true` counts as one (0001); and
radeonsi wanted libelf whenever it had gfx/compute, though with ACO it makes raw binaries (`SI_SHADER_BINARY_RAW`) and
opens no ELF: libelf is now required only with LLVM, and `ac_rtld.c` without libelf only says it cannot read ELF (0002).
No LLVM build is needed.

**Darwin has no DRM (0001, `src/oneios`).** The build takes libdrm's own headers and its amdgpu library from the libdrm
release Mesa's wrap pins (`subprojects/libdrm.wrap`: 2.4.133, sha256 `fc68f9d0...`, MIT; `build.sh` fetches it once
into `out/metal-amd/dl` and checks the hash), built for Darwin: its VA manager is what the amdgpu virtio context uses,
its ioctl paths answer ENODEV. `src/oneios/oneios_drm.c` implements the xf86drm calls Mesa and libdrm_amdgpu make. The
GPU's descriptor is a descriptor of `/dev/null` (`oneios_drm_open()`; the approach of Jesvi-iOS's
`tools/mesa/redwood-drm/redwood_drm.c`, no code taken): `drmGetVersion` says `virtio_gpu` for it, so
`radeonsi_screen_create` takes its virtio path, `drmGetPrimaryDeviceNameFromFd` names one device (the amdgpu virtio
context's dedup keeps one connection per process), `drmGetDevice2` a PCI device 0000:00:00.0. PRIME and syncobj calls
answer ENOSYS. AMD's addrlib reads `__APPLE__` as Apple's own driver build (a private `addrExtDef.h`): `HAVE_TSERVER` is
its switch for the standard headers (0001). ACO's check for the CLRX disassembler calls `system()`, which iOS lacks
(0006).

**The Darwin vdrm backend (0003, `src/virtio/vdrm/vdrm_darwin.c`)**, chosen by `vdrm_device_connect` when
`HAVE_VDRM_DARWIN` (the Darwin build) is set; Linux keeps `vdrm_virtgpu.c` and `vdrm_vpipe.c` unchanged. It follows
Droid-VM-UniXNU's `patches/mesa/turnip-darwin-virtgpu.patch` (`tu_knl_darwin_virtgpu.cc`, MIT): `IOServiceMatching
("IOAcceleratorES")` (or the class `ONEIOS_VDRM_SERVICE` names), `IOServiceOpen` type 0x4149, the tunnel selector found by
the probe record `{'NTNL', 0xffffffff}`, then each method as `{'NTNL', 0x7200 + op}` and its record of
`DroidVMVirtgpuABI.h` (NativeAdreno 627836d, copied into the patch unchanged):

| vdrm | Method | Notes |
|---|---|---|
| connect | GETPARAM (ABI_VERSION, CREATE_GUEST_HANDLE), GET_CAPS capset 6, CONTEXT_INIT {6, 64 rings}, CREATE_BLOB + MAP of the shmem | refuses a kernel without CREATE_GUEST_HANDLE (K2 needs it); a capset of another context type gives no device, silently, as on Linux |
| shmem | CREATE_BLOB HOST3D, MAPPABLE, 16 KiB, blob_id 0, no ccmd; MAP (the host's pool) | K2; responses at the host's `rsp_mem_offset` |
| bo_create | CREATE_BLOB HOST3D_GUEST, flags as Mesa asks + CREATE_GUEST_HANDLE, size, blob_id, followed by the driver's ccmd (amdgpu's GEM_NEW) | K2 for every BO with blob_id != 0; handle and resource id from the reply (no RESOURCE_INFO call) |
| bo_map | MAP, once per BO; kept until GEM_CLOSE | 0004: amdgpu virtio's `munmap` is not called on Darwin (the kernel's mapping object would point at a hole); a placed map is refused |
| bo_close, close | GEM_CLOSE (after flushing buffered ccmds); the shmem's; IOServiceClose | the kernel frees a BO's pages once the context's submissions are done |
| execbuf, flush | EXECBUFFER {flags, ring, size} + commands; FENCE when the caller has out-syncobjs or waits | ccmds on ring 0; a CS_SUBMIT on its virtio ring (GFX 1, then compute, SDMA ... as amdgpu virtio numbers them) |
| wait_fence, syncobj waits | WAIT_FENCE {ring, fence, timeout} | sleeps in the kernel; a fence known retired asks nothing; no polling of ours. `vdrm_host_sync`'s seqno check after a fenced ccmd finds the host done (the ring-0 fence retires after the host processed it) |
| (`vdrm_darwin_bo_create_user`) | CREATE_BLOB_USER over the caller's 4 KiB-aligned pages, with its GEM_NEW | for an IOSurface import later; nothing in the winsys calls it yet |

Syncobjs (0003, `vdrm_darwin_get_sync`; 0004 hands it to the winsys in place of `util_sync_provider_drm`): a syncobj is
unsignalled, signalled, or holds the (ring, fence) of the EXECBUFFER that names it as an out-syncobj. Waits follow DRM:
a syncobj with no fence is -EINVAL unless WAIT_FOR_SUBMIT, which waits on a condition until a submission gives it one;
the timeout is absolute (os_time_get_nano); -ETIME. An EXECBUFFER's in-syncobjs on another ring are waited for (in
WAIT_FENCE) before it is sent; on its own ring the host keeps the order. Binary only: no `timeline_wait`, so
`has_timeline_syncobj` is false; no syncobj or sync-file descriptors, no dma-bufs (import and export answer ENOTSUP).
`vdrm_bo_wait` (the winsys's wait for a BO it did not fence itself) waits for everything submitted on every ring:
the ABI names no BOs in a submission.

**amdgpu winsys and screen (0004, 0005).** The amdgpu winsys on Darwin compares two GPU descriptors as equal only when
they are the same descriptor (Darwin cannot compare file descriptions; it logged a warning for each screen).
`oneios_radeonsi_screen_create` opens the GPU descriptor, parses Gallium's and radeonsi's option tables (as the pipe
loader merges them), calls `radeonsi_screen_create` (the virtio path) and closes its descriptor (the winsys keeps a
duplicate); NULL with the reason in `why`, from `vdrm_darwin_last_error()` when the connection failed. On Linux the
same file opens the first `virtio_gpu` render node instead (for the host build). The library's sources and exported
names are lists in `src/gallium/targets/oneios-radeonsi/meson.build` (`oneios_radeonsi_sources`,
`oneios_radeonsi_exports`) that the Metal layer's 5xxx patches may extend; `build.sh mesa` then runs
`userland/metal-amd/air/build.sh MESA_SOURCE BUILD_DIR` when it exists.

**`oneios-amdgpu-probe`** (0005, `oneios_amdgpu_probe.c`), on the A56 from a root shell, prints one `PASS` or `FAIL`
line a step and exits with the number of FAILs. The steps are the other A56 fork's guest probe and host self-test, over
Mesa's own amdgpu virtio context (`amdvgpu_*`) and the Darwin backend: the device, the capset's facts (marketing name,
family, device id, revisions, GFX IP version and rings, VA range, CUs), a 4 KiB GTT guest BO and its CPU view, its GPU
address (BO_VA_OP), a context, eight GFX10+ type-2 NOPs (0x80000000, the dwords the fork's A56 self-test ran) submitted
with an out-syncobj, the syncobj waited (WAIT_FENCE) and the host's CS_QUERY_FENCE_STATUS, the reset state. With
`--screen`: `oneios_radeonsi_screen_create`, the screen's name and caps, a context, a 64 KiB buffer cleared with
`clear_buffer` and a 64x64 RGBA8 texture with `clear_texture`, each flushed, fenced and read back word for word.

**Host tests** (`userland/metal-amd/host-test.sh`; `METAL_AMD=1 runtime/host-tests.sh` runs it as its `metal-amd`
part), on the build laptop, 2026-11-24: `patches` pass (0001-0006 apply in order to a fresh clone of cecc0e9); `linux`
pass (the same Mesa builds for Linux with radeonsi, the amdgpu virtio context and ACO alone; `liboneios_radeonsi.so`
exports `oneios_radeonsi_screen_create` and nothing else; the probe links); `vdrm` pass, 51 of 51 checks of
`tests/vdrm_darwin_test.c`: `vdrm_darwin.c` and Mesa's `amdgpu_virtio*.c` compiled for Linux with `HAVE_VDRM_DARWIN`,
the IOKit calls a fake of the kernel's user client (`tests/fake-iokit`) that plays a minimal amdgpu host for the
ccmds (QUERY_INFO, CREATE_CTX, CS_SUBMIT, CS_QUERY_FENCE_STATUS). It checks the open type, the selector probe, the
capset and context records, the shmem blob, that a GTT BO is HOST3D_GUEST with CREATE_GUEST_HANDLE | MAPPABLE and its
GEM_NEW ccmd (same blob_id, alloc_size, heap) and a NO_CPU_ACCESS VRAM BO HOST3D_GUEST with CREATE_GUEST_HANDLE alone,
one MAP per BO, sync ccmds as fenced ring-0 EXECBUFFERs and their WAIT_FENCE, a GFX CS_SUBMIT fenced on ring 1, a
compute one on ring 2 whose in-syncobj on ring 1 is waited for first, syncobj waits (one WAIT_FENCE, then none;
-EINVAL; -ETIME with WAIT_FOR_SUBMIT; WAIT_ANY), CREATE_BLOB_USER, the closes. The connect line the test prints:
`vdrm-darwin: connected: IOAcceleratorES open type 0x4149, tunnel selector 5, ABI 1, capset 6 context type 2 (drm
3.61.0), context 16, shmem handle 1 (16 KiB, responses at 256), guest-allocated BOs`.

**Not done, exactly.** Nothing of it has run on an A56 or in iOS (there is none here; no qemu boot). Untested beyond
the fake: everything past the ccmd records (the host's answers, radeonsi's screen creation over a real capset, ACO's
shaders on the Xclipse, the clears). Paths that are stubbed or answer "unsupported" on Darwin: dma-buf import and
export, PRIME, sync files and syncobj descriptors (`fence_import_sync_file`, `fence_export_sync_file`, the
`export_signalled_sync_file` of the winsys fail), timeline syncobjs (off), placed BO maps, BO lists in EXECBUFFER
(amdgpu virtio sends none), the drm-ioctl (non-virtio) amdgpu paths, `drmGetCap` (no modifiers), the shader cache
(`-Dshader-cache=disabled`: every process compiles its shaders again), video (radeonsi's VCN code is linked but no
video frontend is built). `vdrm_darwin_bo_create_user` exists but the winsys has no import that calls it (IOSurfaces:
the Metal layer's). The binaries are not stripped and not placed in any image. WAIT_ANY over several syncobjs waits
for the first that has a fence (it returns when one is signalled, perhaps later than DRM would).

**The A56 lines that prove it** (once the kernel publishes the accelerator with `oneios-gpu-amd=1` and xnuhost serves
capset 6 with amdgpu): `oneios-amdgpu-probe` prints `PASS device: amdgpu virtio context, host amdgpu DRM 3.N`,
`PASS facts: "<the Xclipse's marketing name from the capset>" family N device 0x....`, `PASS bo: 4096 bytes,
resource N`, `PASS va: ...`, `PASS submit: GFX NOP IB of 8 dwords, host seqno 1`, `PASS fence: signalled N us after the
submit; host says seqno 1 expired`, `PASS reset: none`, then with `--screen` `PASS screen: "<radeonsi's name: the
marketing name and the LLVM-free ACO>" ...`, `PASS clear buffer: 65536 bytes of 0x5a0e1a56 read back`, `PASS clear
texture: 64x64 RGBA8 of 0xff0e1a56 read back`, and `PASS: 0 failed`. iOS's log (unified log, `phone pull-log`) gets
`vdrm-darwin: connected: IOAcceleratorES open type 0x4149, tunnel selector N, ABI 1, capset 6 context type 2 (drm 3.N),
context N, shmem handle N (16 KiB, responses at N), guest-allocated BOs` from every process that opens the GPU, and
`oneios-radeonsi: screen "..." (AMD, ...), N MiB of memory, compute 1` for every screen; a failure gives
`vdrm-darwin: <why>` or `oneios-radeonsi: radeonsi: no screen: <why>`. xnuhost's side of the same submit is section 2's.

**What the other stages must match:** the kernel's user client answers GETPARAM `CREATE_GUEST_HANDLE` = 1 (or nothing
connects), GET_CAPS of capset 6 with context type 2 and `virgl_renderer_capset_drm` as Mesa cecc0e9 lays it out
(`drm_hw.h`, with `ENABLE_DRM_AMDGPU`: the amdgpu union holds `amdgpu_buffer_size_alignments` and `amdgpu_gpu_info` of
libdrm 2.4.133), CONTEXT_INIT with 64 rings, the shmem's `rsp_mem_offset` set by the host, EXECBUFFER FENCE on any
ring below 64 (amdgpu virtio numbers ring 0 for ccmds and 1.. for the hardware rings in the order of `AMD_IP_*`), and a
service whose class matches `IOAcceleratorES`. The Metal layer links `liboneios_radeonsi.dylib` (or adds its sources to
it) and calls `oneios_radeonsi_screen_create`.

**RADV over the same backend (read, not built).** RADV's winsys initialises through the same `ac_drm_device_initialize(fd,
is_virtio)` and so the same amdgpu virtio context and vdrm backend; its physical device probe opens a render node by
path and takes the virtio path when `drmGetVersion` says `virtio_gpu` (`radv_physical_device.c`). What it would need:
(1) a device to enumerate: `vk_instance.c` lists devices with `drmGetDevices2`, which `oneios_drm.c` answers with none
(it could return one device whose render node is `/dev/null`, the GPU descriptor); (2) its syncobj type from the amdgpu
device's sync provider: `radv_amdgpu_winsys_query_info` calls `vk_drm_syncobj_get_type(fd)`, which makes a DRM provider
on the descriptor (ENOSYS here) where `vk_drm_syncobj_get_type_from_provider(ac_drm_device_get_sync_provider(dev))`
would take the Darwin one (binary; RADV emulates timelines then); (3) a WSI: Mesa's `VK_EXT_metal_surface` layer, as
Turnip's Darwin build uses. A guess, not tried: three small patches and the Vulkan runtime's build for Darwin would give
NativeMetal/metal2vulkan a RADV path later.

## 4. Guest: the AIR Metal layer and the A56's Metal plug-in

(Filled by the Metal stage.)

## 5. Guest kernel: the accelerator on an amdgpu host

(Filled by the kernel stage.)

## 6. Safety, switches, and what is left

- `gpu=2d` (the command line, the image's `overlay/userland/xnuhost.conf`, or the `galaxy-a56` row) always gives the
  2D path of today. The way back to One UI is not touched by any part of this port.
