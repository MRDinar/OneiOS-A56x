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

(Filled by the Metal stage.)

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
