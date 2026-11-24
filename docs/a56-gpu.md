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

(Filled by the guest Mesa stage.)

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

(Filled by the kernel stage.)

## 6. Safety, switches, and what is left

- `gpu=2d` (the command line, the image's `overlay/userland/xnuhost.conf`, or the `galaxy-a56` row) always gives the
  2D path of today. The way back to One UI is not touched by any part of this port.
- The Metal plug-in (section 4) gives backboardd no device unless `oneios-radeon-backboardd=1` is in the boot
  arguments (a test boot only); `OneiOSRadeonMetal.clients` beside the bundle names the processes that get one;
  `oneios-metal-profile=N` changes the feature profile (default 1). A process without a device keeps today's road.
