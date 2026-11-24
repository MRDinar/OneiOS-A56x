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

**What it is.** `userland/metal-amd`: Jesvi-iOS a13e406's Metal-on-Gallium layer, copied with its MIT notice
(`userland/metal-amd/NOTICE`: every file taken, what was changed, what was not taken) and moved onto the A56:

```
Metal (iOS 16.5) -> OneiOSRadeonMetalDevice (plugin/redwoodmetal.m, Objective-C, in OneiOSRadeonMetal.bundle)
   | dlopen of libOneiOSRadeonAir.dylib beside it; the rwa_* calls of air/air_metal.h (no Mesa header on this side)
libOneiOSRadeonAir: air_metal.c (pipelines, resources, draws and dispatches on one pipe_context)
   + the AIR frontend (air_bitcode, air_module, air_to_nir, air_intrinsics: Apple's shader bitcode -> NIR)
   + Mesa cecc0e9's Gallium; the screen: oneios_radeonsi_screen_create(why, n) (K3, the guest Mesa stage's, section 3)
```

- The screen (`air_metal.c` `rwa_device_open`): with `-Doneios-air-screen=radeonsi` the A56's radeonsi through K3;
  Jesvi's freedreno and Panfrost screens over its xf86drm stand-in are gone, and so is the plug-in's own GPU client
  (Jesvi opened its kernel's RedwoodGPU/RedwoodMali and made buffers there): every buffer, texture and fence is
  Mesa's, so the kernel services K3 names are reached by Mesa's Darwin vdrm only. Declared in
  `air/oneios_radeonsi_screen.h`; `build.sh MESA_SRC MESA_BUILD` sets the option when the tree defines the function.
- RDNA3 values, each commented where it is: `threadExecutionWidth` is the smallest size in radeonsi's
  `subgroup_sizes` mask (32: radeonsi compiles compute as wave32 on gfx10 and later); `maxThreadsPerThreadgroup`
  1024 (radeonsi's `max_threads_per_block`); linear and IOSurface row alignments 256 (Mesa `ac_surface.c`
  `ac_surface_get_pitch_align`, gfx9 to gfx11; Jesvi answered Panfrost's 64); threadgroup memory stays 16 KiB (the
  feature profile's table; RDNA3 has 64); the device's name is Mesa's renderer string with "(OneiOS)".
- Mesa 25.0.7 (Jesvi's) to cecc0e9 (26.3-devel): the API changes listed in the NOTICE; one reader fix (LLVM 22's
  `MODULE_CODE_GUIDLIST` record is skipped). Mesa wiring: `patches/mesa/5001-air-frontend.patch` (the guarded
  `subdir('air')`) and `5002-air-metal-library.patch` (`src/gallium/targets/air-metal`: `libOneiOSRadeonAir` and
  `air_metal_test`, read after every driver; options `oneios-air`, which also counts as a graphics and compute build
  (`with_gfx_compute`: without OpenGL or Vulkan Mesa builds NIR as headers only, and radeonsi's gfx part needs it
  too), and `oneios-air-screen`). Both apply to cecc0e9 with `git apply --check`.

**Built and tested (build laptop, 2026-11-24; [building.md](building.md) section 9).** `host-test.sh`: Mesa cecc0e9
with the 5xxx patches, softpipe, `air_metal_test` driving the library through `air_metal.h` alone. 8 of 8 pass:
the frontend (`phiconst.ll`: the edge constant survives the phi), a compute dispatch (`tid_store.ll`, ours: 16
threads write `tid * 3 + 5`; width 32), a render pipeline (`fragpos.ll` into RGBA32Float: pixel (0,0) is
`0.5 0.5 0 1`, Metal's top-left origin), argument buffers (`argbuf_set.ll`/`argbuf_direct.ll`: 0 of 20 checks
failed: members bound at their own indices, offsets, five wrong encodings refused by name), the IOSurface refusals,
and the open without a GPU screen. Inputs: four hand-written `.ll` of Jesvi's and one of ours, assembled with
llvm-as; no Apple metallib. Darwin: `build.sh mesa-ios` (the same Mesa for arm64-apple-ios16.5, softpipe only) and
`build.sh plugin`: `OneiOSRadeonMetal` and `libOneiOSRadeonAir.dylib` PASS `oneios-tool ios macho-check --arch arm64`
against `out/ios16-tbd` (171 and 149 symbols bound; Metal 306.6.4, iOS 16.5's), signed with 4 KiB pages; the
gpuaddon is 5 files. The `radeonsi` branch of `air_metal.c` compiles (host compiler; its one undefined symbol is
`oneios_radeonsi_screen_create`). **Not tested:** anything on radeonsi or ACO (the host test is softpipe's), the
bundle inside iOS, the guest Mesa stage's library (not built yet).

**Metal, area by area** (what the code does; on the A56 nothing has run):

| Area | State |
|---|---|
| Discovery, device | `-initWithAcceleratorPort:` makes the device, opens the screen; feature profile 1 (Apple1, Apple2, Common1: the lowest MPS accepts); `-name`, limits, `registryID` |
| Buffers | Mesa buffers, CPU-visible (shared storage); `newBufferWithBytesNoCopy:` as a mirror copied in and back; `gpuAddress` 0; no buffer without a screen |
| Textures | 42 pixel formats, 10 texture types, views, mipmaps, readback; multisample textures but **no resolve** (`supportsSampleCount:` answers 1 only); a texture over a buffer needs the driver's handle export (`resource_get_handle`, an FD): **not known to work on radeonsi over vdrm** |
| IOSurface textures (CAMetalLayer drawables, the compositor's surfaces) | **as copies**: a texture of ours filled from the surface when its seed moves, and written back after the GPU when it was a render target (not after a compute or blit write). Jesvi imported in place through its kernel (kRedwoodSurface); the A56 has no such import. A 720x1560 BGRA drawable is 4.5 MB each way a frame (a cost, not measured) |
| Compute | AIR bitcode -> NIR -> the driver; `dispatchThreadgroups`, `dispatchThreads`, threadgroup memory, argument buffers |
| Render | vertex + fragment (a generated vertex stage when none is named), vertex descriptors, blending, depth/stencil, indexed and non-indexed draws, per-attachment load and store actions, viewport depth range |
| Blit | buffer copy and fill, texture copy, mipmap generation |
| Argument buffers | a recorder: `MTLArgumentEncoder` records what is set and the draw binds it; a buffer written by the CPU or a kernel instead is refused by name |
| Not implemented (nil or a refusal by name) | heaps, fences, indirect command buffers, function tables, acceleration structures, mesh, tessellation, tile shading, stage-in, sparse textures, `newBufferWithIOSurface:`, shared textures, resource groups, counters; events and libraries are `_MTLDevice`'s own (not measured) |
| GL-on-Metal (iOS's OpenGL ES) | not served: Apple's GL hands Metal shaders as GLIR, which this layer does not read (Jesvi: such games stay black) |

**backboardd, and the UI's frame rate (decided).** The plug-in gives backboardd **no device** by default, so the
A56's UI is composited as today (gpu=2d, the CPU): **this stage does not raise the A56's UI frame rate.** Why:
Jesvi's own record (`docs/devices/x1slte/display.md`, 2026-11-14): CoreAnimation's render server on this Metal
"crashed it at start (stack overflow, pc 0x205aa4a8c): reverted", and the cause was not found; Jesvi's smooth UI is
CoreAnimation's **OpenGL ES** renderer (`CA::OGL::GLESContext`) on Mesa through its own EAGL (`userland/src/eagl.c`,
`inject.c`), never Metal; Jesvi's kernel hides the accelerator's discovery properties from "incompatible core
renderers". Nothing in the code shows the crash gone, and the drawables above are CPU copies, which the render
server would pay for every surface. How: `rm_offered` in `redwoodmetal.m` returns nil from `-initWithAcceleratorPort:`
in backboardd, mediaserverd, PosterBoard, AegirPoster and WeatherPoster (Jesvi's metaloff list) -- nil is "no
device", the road NativeAdrenoMetal takes in every process it does not serve; `OneiOSRadeonMetal.clients` beside the
bundle overrides the list ([userland.md](userland.md) 7.2); `oneios-radeon-backboardd=1` hands backboardd the device
for a test boot. The ways to a faster A56 UI, guesses until measured: CoreAnimation's GL renderer on Mesa (Jesvi's
road: an EAGL over Mesa's GL frontend on radeonsi, not built here) or backboardd on this Metal once the crash is
understood and IOSurfaces import in place (a Darwin vdrm import of the surface's wired pages, sections 3 and 5).

**The log lines that prove it** (the A56 has not run them). In `xnuhost.log` (`oneios-tool phone log`), from the
kernel's text rows (hosted 0039; 63 characters):

```
oneios-gpu-text: backboardd[NN] RM I DEV declined: backboardd is one of the processes kept off this Metal...
oneios-gpu-text: SomeApp[NN] RM I DEV offered: every process but backboardd and Jesvi's metaloff list...
oneios-gpu-text: SomeApp[NN] RM I DEV OneiOSRadeonMetal in SomeApp[NN], accelerator 0x...
oneios-gpu-text: SomeApp[NN] RM I DEV compiler library from /System/Library/Extensions/OneiOSRadeonMetal.bundle/...
oneios-gpu-text: SomeApp[NN] RM I DEV screen AMD Radeon ... (radeonsi, ...)      <- the GPU is this process's
oneios-gpu-text: SomeApp[NN] RM E DEV screen no radeonsi screen: <the guest Mesa stage's reason>   <- or not
oneios-gpu-text: SomeApp[NN] RM I PSO compute pipeline made: ... / pipeline 1: ...  <- the first pipeline compiled
```

The same records whole are in iOS's log (os_log subsystem `com.oneios.radeonmetal`, `oneios-tool phone pull-log`).
Before any of them: the kernel's `oneios-gpu-plugin` / `oneios-gpu-open` lines of the plug-in's load (section 5).
The renderer string is radeonsi's own (shape only here: `AMD Radeon ...`; a guess until the A56 prints it).

**Not done.** The bundle is placed by no tool (the image hook knows `NativeAdrenoMetal.bundle`; the kernel's addon
mount looks for that name: section 5). No in-place IOSurface import. The library has not been built with the guest
Mesa stage's radeonsi (its tree had no `oneios_radeonsi_screen_create` when this was written); the bundle of
2026-11-24 carries the softpipe-only `mesa-ios` library, whose device open fails by name. Panfrost's counters in
`air_metal.c` (`rwa_gpu_cache_stats`) compile out and answer -1.

**What the other stages must match.** Guest Mesa (section 3): define `struct pipe_screen
*oneios_radeonsi_screen_create(char *why, size_t why_size)` (`air/oneios_radeonsi_screen.h`) in a file under
`src/gallium` or `src/amd` reached through `driver_radeonsi` (5002 links `driver_radeonsi`), configure with
`-Doneios-air=true` (or OpenGL), and call `userland/metal-amd/air/build.sh MESA_SRC MESA_BUILD` after its Mesa step;
the screen is opened once per Metal device and destroyed with `screen->destroy`. Kernel (section 5): the accelerator's
class name starts with `AGX` (hosted 0031/0039 log the plug-in's opens and text rows only then) and answers opens of
types `0x4147xxxx` with an error; `MetalPluginName`/`MetalPluginClassName` per K3; the addon mount (0031) and its
plug-in log (`oneios-gpu-plugin`, which matches `NativeAdrenoMetal` and `libvulkan_freedreno` by name) need the A56's
names (`OneiOSRadeonMetal`, `libOneiOSRadeonAir`) and 20F66's sealed driver bundle.

## 5. Guest kernel: the accelerator on an amdgpu host

Built and host-tested (2026-11-24, branch `t3/a56-amdgpu-kernel-support`), not booted: the GPU archive
(`kernel/patches/oneios-hosted/gpu/`: `OneiOSGpu.cpp`, `OneiOSAdreno.cpp`, the new `OneiOSGpuChoice.h`; the staged
edits `staged/gpu.rs` `GPU`, `GPU3D` and the new `ACCELERATOR` of `oneios-tool kernel gen gpu`) and hosted
`0108-a56-gpu-plugin.patch`.

### 5.1 What the A56 release kernel did on an AMDGPU capset (read from the code at 39ad96e)

The kernel itself never sends an msm ccmd or reads an msm field outside `vgpu-selftest=1`, and nothing in it waits on
the capset, so an amdgpu host caused no panic and no hang. The fault was the publish:

- `OneiOSGpu.cpp:53-60` set gpu state 3 from the virtio features alone (VIRGL, RESOURCE_BLOB, CONTEXT_INIT,
  CREATE_GUEST_HANDLE); the capset was never read.
- `OneiOSAdreno.cpp:109-114` (`oneios_adreno_wanted`): state 3 published NativeAdreno's accelerator unless
  `oneios-gpu-metal=0`. So on an amdgpu host:
  - a 20F66 image with roottweak's `oneios-gpu-metal=0`: nothing published. Safe.
  - without it (an image whose `xnuhost.conf` lacks it; iOS 27 on an A56; `oneios-gpu-metal=1`): `AGXAccelerator` with
    `MetalPluginName` `NativeAdrenoMetal` (`DroidVMGpu.cpp:284`) on a host that is not msm. A wrong publish. On iOS 16.5,
    stock Metal opens it as an IOGPU device (type 1, refused `0xe00002c7` at `NativeAdrenoPublish.cpp:715`), and
    backboardd gets a Metal device that cannot draw ([a56.md](a56.md), "Why 16.5 needed each piece"). On iOS 27,
    NativeAdrenoMetal's Turnip asks capset 6 for an msm context [guess, not run: its vdrm then finds context type 2 and
    makes no device].
  - No KGSL anywhere: NativeAdreno's hardware classes are never attached (`DroidVMGpu.cpp:176`, a 0x4147 open fails
    not ready), and `native_adreno_publish` (`NativeAdrenoPublish.cpp:782`) touches no GPU.
- The user client's virtgpu methods do not depend on the driver (`DroidVMVirtioGpu3D.cpp`): `getCaps` (435) passes
  the host's capset through, `contextInit` (450) sends CTX_CREATE with the caller's capset id, `createBlob` (647) sends
  the caller's ccmd first and then RESOURCE_CREATE_BLOB with its blob_mem (HOST3D, or HOST3D_GUEST over wired guest
  pages), flags, blob_id and size. That is what K2 needs.
- `vgpu-selftest=1`: the staged selftest already branches on the capset (`staged/gpu.rs` `GPU3D`, port-a56 8f889d0):
  AMDGPU sends `AMDGPU_CCMD_QUERY_INFO(ACCEL_WORKING)`, MSM sends `GET_PARAM(GPU_ID)`, and any other type stops at
  stage 3 (unsupported).

### 5.2 The fix: which accelerator, from the capset

`OneiOSGpu.cpp` now reads the DRM capset's `context_type` once at the start when the state is 3 (GET_CAPSET_INFO by
index, then GET_CAPSET 6: two read-only queries the host answers as it does for every Metal process's vdrm). It puts
the result in `oneios_gpu3d_context_type`: 1 MSM, 2 AMDGPU, 0 not read. `OneiOSAdreno.cpp` decides once, before
launchd, with `oneios_gpu_accelerator_choice` (`gpu/OneiOSGpuChoice.h`, a pure function):

| Host and boot arguments | Published |
|---|---|
| MSM capset (or none read), no `oneios-gpu-amd=1` | as before: the Adreno accelerator when the state is 3, or as `oneios-gpu-metal` / `droidvm-gpu-metal` says |
| capset read and not MSM (AMDGPU, or any other), no `oneios-gpu-amd=1` | **nothing**, whatever `oneios-gpu-metal` says |
| `oneios-gpu-amd=1`, 3D with an AMDGPU capset | **the A56's accelerator** (5.3), with or without `oneios-gpu-metal=0` |
| `oneios-gpu-amd=1`, anything else (no 3D, MSM, not read) | nothing: never the Adreno one, because the host said its GPU is an AMD one |

How `oneios-gpu-amd=1` (K5) combines with 20F66's `oneios-gpu-metal=0`: `oneios-gpu-metal` applies only to the Adreno
accelerator. That word was written against NativeAdreno's publish (stock Metal's type-1 open), and roottweak writes it
into every 20F66 image's `xnuhost.conf`. The A56 keeps its image, so if `=0` also stopped the A56's accelerator, the
image would have to be made again. To turn the A56's accelerator off: leave out `oneios-gpu-amd` (or give `=0`), or
use `gpu=2d` on the host (no 3D, state 2).

The S24 Ultra and iOS 27 stay the same: their host's capset is MSM and xnuhost never passes `oneios-gpu-amd`, so the
first row applies. It is the old `oneios_adreno_wanted` rule (`test_choice.c` checks this for every state and
`oneios-gpu-metal` value), and every hook below answers what the Adreno code did before. The only difference on the
S24 is the two capset queries at the start. No line is logged for an MSM capset.

### 5.3 The A56's accelerator

**The class.** It is the same `AGXAccelerator` (NativeAdreno's `NativeAdrenoAccelerator.cpp`, a subclass of the
collection's `IOAcceleratorES`, with its chain adopted as `AGXAccelerator -> IOGPU -> IOAcceleratorES`) and the same user
client (`AGXDeviceUserClient -> IOGPUDeviceUserClient`). Only one accelerator is published in a boot. Why not a
sibling class:
- Metal's discovery on 20F66 (Jesvi-iOS `userland/src/metal_props.c`, `metal_dev.m`: `IOServiceMatching("IOAcceleratorES")`,
  then `MetalPluginName` -> `/System/Library/Extensions/<name>.bundle`, then `MetalPluginClassName`, then
  `[[class alloc] initWithAcceleratorPort:service]`) needs only an `IOAcceleratorES` with the two keys.
- The sandbox's profiles name these classes and their IOGPU chain, so the grants already made for them work here.
- A new class would need its own binding to `IOAcceleratorES` (the same 24A446 collection on the A56) and would gain
  nothing.

Its properties:
- `MetalPluginName` `OneiOSRadeonMetal` and `MetalPluginClassName` `OneiOSRadeonMetalDevice` (K3);
- `IOGLESBundleName` `AppleMetalGLRenderer`, as before (OpenGL ES on the process's Metal device; Jesvi-iOS names the
  same one on 20F66);
- `NativeSandboxGrantProcess`: the client list, as before;
- `OneiOSDrmContext` `"amdgpu"`, new, for the plug-in and for ioreg.

`DroidVMGpu.cpp`'s literal names are now `oneios_gpu_metal_plugin()` and `oneios_gpu_metal_class()` (staged `GPU`).
For the Adreno accelerator these return the old strings.

**The open types** (staged `ACCELERATOR`, `oneios_gpu_open_refused`):
- Served: 0x4149, `DROIDVM_VIRTGPU_OPEN_TYPE`. Each connection is one DRM context, with the methods of
  `DroidVMVirtgpuABI.h` through `{'NTNL', 0x7200 + op}` (`NativeAdrenoPublish.cpp:699`, `droidvm_virtgpu_call`).
- Passed on: NativeAdreno's progress markers 0x4147xxxx (xxxx nonzero). They make no connection; they are the
  plug-in's text rows, `oneios-gpu-text`.
- Refused with `kIOReturnUnsupported`: every other type. That includes 0x4147 (NativeAdreno's KGSL-shaped GPU core),
  0x4148 (the display hand-off) and 1 (iOS's IOGPU device). The first 16 refusals of a boot are logged.

The `NativeAdrenoGpu` service is still published (`native_adreno_publish`; the user client serves only after it), and
its own 0x4147 opens still fail not ready. There is no Adreno self-test. `vgpu-selftest=1` and
`oneios-adreno-selftest=1` (an in-kernel 0x4149 open and `GETPARAM(ABI_VERSION)`) are generic virtgpu and stay
available.

**Which processes may open it (the sandbox).** These rules are unchanged from the Adreno accelerator:
- 0033: the grant (`NativeSandboxGrantProcess` on the service) decides both the service open and the user client's
  MACF check for the processes it names (`0033-gpu-grant-decides.patch` lines 15 and 27). The rest are the sandbox
  profiles'.
- The grant's list is `droidvm-gpu-clients`. Without it the default is `*`, so every process may open.
- A 0x4149 connection that MACF or the filter resolver refuses gets `NativeSandboxGrant` (the message filter).
- Not known: which 20F66 processes the iPhone X's own profiles let open it without the grant (not run).

**CPU composition kept: Jesvi-iOS's filter, in the kernel (`oneios-gpu-amd-cpu`).** For the processes on that list,
the A56 accelerator answers its five discovery keys as absent:
- the keys: `MetalPluginName`, `MetalPluginClassName`, `IOGLESBundleName`, `IOGLESMetalBundleName`,
  `IOGLESDefaultUseMetal`;
- where: `AGXAccelerator::copyProperty`, all three forms, which IOKit's single-property and search reads call
  (`IOCopyPropertyCompatible`), and a new `serializeProperties` override for the whole-table read
  (`IORegistryEntryCreateCFProperties`). Each answer is for the process that reads; the registry is not changed;
- the effect: those processes find no plug-in and no GL driver, and draw on the CPU as with no accelerator.

The default list is `backboardd`, `SpringBoard`, `mediaserverd`, `PosterBoard`, `CollectionsPoster`, `WeatherPoster`,
`SBRendererService`, `MobileSlideShow` (`ONEIOS_GPU_AMD_CPU_DEFAULT`). This is Jesvi-iOS's
`redwood_graphics_policy.h` list; its 15- and 16-character command aliases are matched by the name matcher. **So
backboardd keeps CPU composition by default**: CoreAnimation's render server on Jesvi's Metal crashed at start and was
reverted (section 1), and nothing here shows it working. Forms of the boot argument:
- `oneios-gpu-amd-cpu=a,b`: another list (`X*` matches a prefix, `*` every process);
- `oneios-gpu-amd-cpu=` (empty): no process keeps the CPU. This is the test of backboardd on this Metal.

The plug-in's own allow-list (the Metal stage) still applies beside this filter: 0108 writes the grant's list as
`OneiOSRadeonMetal.clients` beside the bundle, as 0039 writes `NativeAdrenoMetal.clients`.

**0108's plug-in hooks.** In `kern_exec.c`:
- 0039's list file is named after the accelerator's `MetalPluginName`.
- One line says whether `/System/Library/Extensions/OneiOSRadeonMetal.bundle/Info.plist` is visible through 0031's
  union. 0031's stage-5 check looks for `NativeAdrenoMetal.bundle` and the sealed `AGXMetalG18P.bundle`, which the
  iPhone X image does not have, so on 20F66 its line says stage 3: mounted.

In `kern_descrip.c`, 0031's dyld lines also name `OneiOSRadeon*` files.

### 5.4 Tests

- `kernel/tests/host.sh`: the new `gpu/tests/test_choice.c` checks the choice for every state, capset,
  `oneios-gpu-metal` and `oneios-gpu-amd`, against the old rule on MSM. It also checks the CPU list's matcher, the
  discovery keys and the open types. It runs with ASan and UBSan.
- `cargo test -p oneios-kernel --test golden`: the three edit lists on made-up sources (`synthetic/gpu`, `gpu3d`,
  `accelerator`). The new `the_gpu_edits_fit_the_pinned_sources` checks them on the real pinned `DroidVMGpu.cpp`,
  `DroidVMVirtioGpu3D.cpp` and `NativeAdrenoAccelerator.cpp`: each anchor there once.
- `HOSTED_OUT=a56gpu ONEIOS_CRYPTO=original kernel/build-hosted.sh` on the build laptop (main's series to 0108, 192
  patches): `xnu-12377.121.6-ios-27.0.1-24A446-b14aa6445129`, releasable, no warning in the GPU archive. Its prelink
  `kernel.kc` `f090804d...`: audit 0 failures. The same on `release-kernel-a56` (branch `a56gpu/release-kernel-a56`):
  `...-f8def7dad866`, prelink audit 0 failures, prepared for the Mac to publish (`kernel/patches/oneios-hosted/README.md`
  "0108"). 0108 also applies on `release-kernel-a56`'s series
  (to 0092 and 0102-0107: `git apply` of all 183 patches in order on a scratch tree).
- **Not done:** any boot. There is no A56 here and no qemu boot (qemu has no 3D: none of these lines would show).
  The plug-in, Mesa and the host renderer are the other stages.

### 5.5 The lines that prove it on an A56

With the host's amdgpu renderer and `oneios-gpu-amd=1`:

```
oneios-gpu: virtio GPU with the DRM native context (features 0x...)
oneios-gpu: DRM capset (N bytes) context type 2 (AMDGPU): no Adreno accelerator; the A56's with oneios-gpu-amd=1 on AMDGPU
oneios-gpu-amd: the A56 accelerator: AGXAccelerator, MetalPluginName OneiOSRadeonMetal, MetalPluginClassName OneiOSRadeonMetalDevice, open type 0x4149 alone (DRM context type 2); CPU composition kept by backboardd,SpringBoard,...
droidvm-gpu: published for the virtio GPU
droidvm-gpu: Metal accelerator stage 5 status 0
oneios-gpu-addon: /System/Library/Extensions/OneiOSRadeonMetal.bundle/Info.plist (the accelerator's MetalPluginName): visible (0)
oneios-gpu-amd: backboardd[N] keeps CPU composition: MetalPluginName hidden (oneios-gpu-amd-cpu)
oneios-gpu-plugin: NAME[N] code signature of OneiOSRadeonMetal: error 0
oneios-gpu-open: NAME[N] AGXAccelerator type 0x4149 -> 0x0, AGXDeviceUserClient
oneios-gpu-amd: NAME[N] DRM context 16 on capset 6 (the host's context type 2, AMDGPU)
```

The safe refusals:
- An amdgpu host without `oneios-gpu-amd`: the capset line, then `oneios-adreno: no Metal accelerator: the host's DRM
  context type is 2 (AMDGPU), not MSM; the A56's accelerator is published with oneios-gpu-amd=1 alone (gpu state 3)`.
- `oneios-gpu-amd=1` on a host with no 3D or an MSM capset: `oneios-gpu-amd: no Metal accelerator: oneios-gpu-amd=1,
  but gpu state S and DRM context type T ...`.
- The plug-in is not in the image: `... OneiOSRadeonMetal.bundle/Info.plist (the accelerator's MetalPluginName): not
  there: Metal finds no plug-in and every process draws on the CPU (2)`.
- Another open type: `oneios-gpu-amd: NAME[N] open type 0x1 refused: the A56 accelerator serves type 0x4149 (virtgpu)
  alone`.

**For the other stages:**
- The host: CTX_CREATE's `context_init` carries the capset id in bits 0-7 and OneiOS's hint (role, QoS) in bits 8-23.
  SUBMIT_3D's three padding bytes carry the hint too (`staged/gpu.rs`). The host must mask the capset id with 0xff and
  ignore the rest.
- The host: the context name is `xnu:<process>`.
- The plug-in: it opens the port `initWithAcceleratorPort:` gives it, with type 0x4149. It may check
  `OneiOSDrmContext`.
- Mesa: `GETPARAM(DEFAULT_3D)` answers the boot argument `vgpu-3d`.

## 6. Safety, switches, and what is left

- `gpu=2d` (the command line, the image's `overlay/userland/xnuhost.conf`, or the `galaxy-a56` row) always gives the
  2D path of today. The way back to One UI is not touched by any part of this port.
- The Metal plug-in (section 4) gives backboardd no device unless `oneios-radeon-backboardd=1` is in the boot
  arguments (a test boot only); `OneiOSRadeonMetal.clients` beside the bundle names the processes that get one;
  `oneios-metal-profile=N` changes the feature profile (default 1). A process without a device keeps today's road.
