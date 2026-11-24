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

**State (2026-11-24): written, host-tested on stand-ins, built for Android; not run on an A56** (none here). The
amdgpu native context of virglrenderer, rewritten in Rust with raw ioctls (no libdrm, no C renderer linked), behind
the same `Gpu3d` trait as drm2kgsl (`src/gpu3d.rs`), on `/dev/dri/renderD128`.

| File (`runtime/xnuhost/`) | What |
|---|---|
| `src/gpu3d_amdgpu.rs` | The renderer: the device's figures and the capset (`probe`), the contexts (each its own DRM file and GPU VM), the response shmem in the pool, every `AMDGPU_CCMD_*`, the resources and their udmabuf chunks, the 10 s line, the choice (`choose`). Credits AMD's MIT notice of virglrenderer's `amdgpu_renderer.c` in its head |
| `src/gpu3d_amdgpu/uapi.rs` | The kernel's structs and ioctl numbers (drm.h, amdgpu_drm.h as Mesa cecc0e9 has them) and the guest's wire as offsets (amdgpu_virtio_proto.h, drm_hw.h, libdrm's `struct amdgpu_gpu_info`) |
| `src/gpu3d_amdgpu/layout.c` | Host builds only (`build.rs`): every size, offset and number of `uapi.rs` from those headers (Mesa's from `external/mesa-droidvm`, libdrm's `amdgpu.h` from `pkg-config libdrm_amdgpu` or `/usr/include/libdrm`); test `the_layout_is_the_headers` compares them one by one |
| `src/gpu3d_amdgpu/fences.rs` | The virtio fences: one thread (`amdgpu-fences`) blocks in `poll(2)` on the oldest sync_file of each ring and an eventfd; no timeout, no sleep |
| `src/gpu3d_amdgpu/addon.rs` | Whether the kernel's gpuaddon sidecar (NGPUADD2) carries `addons/OneiOSRadeonMetal.bundle/OneiOSRadeonMetal` and its `oneios-target`, and which build that names (read with `xh_core::inflate`, a zlib decoder added for it) |
| `src/gpu3d_amdgpu/selftest.rs` | The self-test (below), and the guest-side encoder of Mesa's requests the tests reuse |
| `src/gpu3d_amdgpu/tests.rs`, `tests/standin.rs` | 21 host tests on a stand-in amdgpu behind the renderer's `Dev` table (below) |
| `tools/gpuctl.rs` | `gpuctl amdgpu [NODE] [-v]`: the self-test alone on the phone |

**Sources, and where this follows upstream.** Written from virglrenderer's `src/drm/amdgpu/amdgpu_renderer.c`
(`external/virglrenderer` 7e17c8e has upstream's file; the A56 fork's `runtime/xnuhost/amdgpu/src/amdgpu_renderer.c`
is that file plus its responses in xnuhost's pool, and its `gpu3d_xclipse.c` the self-test's steps), its
`drm_context.c` and `drm_fence.c`, and the libdrm 2.4.133 calls the C makes (Mesa's own `libdrm.wrap` tarball,
read for `amdgpu_query_sw_info`, `amdgpu_va_manager_init2`, `amdgpu_query_buffer_size_alignment`,
`amdgpu_bo_query_info`, `amdgpu_cs_query_fence_status`), as ioctls. Upstream's semantics are kept (the copy of each
request zero-extended to its size, the response shadowed and copied up to the guest's length, the seqno after each
command, a failing command ending the stream, the checks of each command) except where K2 or the cost says
otherwise:

- **GEM_NEW allocates nothing** (K2): it records blob_id, size, alignment, heap and flags. The `RESOURCE_CREATE_BLOB`
  that follows (HOST3D_GUEST, the same blob_id, **the same size**: another size is refused) brings the guest's
  pages; `gpu3d::udmabufs` makes one or more udmabufs ("chunks", up to 64 MiB and 1024 runs each), each imported with
  `PRIME_FD_TO_HANDLE`. BO_VA_OP maps each chunk the range covers at its place (`va + chunk offset - range offset`;
  a map that fails half way unmaps the pieces before it); CLEAR, PRT and sparse (`res_id` 0) ranges go through once.
  A CS_SUBMIT's BO list names every chunk of each resource with its priority; a user fence (`CHUNK_ID_FENCE`) names
  the chunk that holds its offset. A HOST3D blob with blob_id != 0 is refused (EINVAL, one line).
- **The capset says `has_vm_always_valid = 0`** (upstream 1). With 1, Mesa's amdgpu winsys makes its BOs
  `AMDGPU_GEM_CREATE_VM_ALWAYS_VALID` and leaves them out of the BO lists (`amdgpu_bo.c` 639, `amdgpu_cs.cpp` 1968);
  an imported BO cannot be per-VM always valid, and the kernel would not make it resident. **The guest Mesa stage
  needs nothing for this**: it reads the bit from the capset.
- **BO_QUERY_INFO answers from the GEM_NEW and the SET_METADATA the guest sent** (upstream asks the kernel, which knows
  its import: GTT, one chunk's size). A display surface (GUEST blob, no GEM_NEW) answers GTT and its size.
- **The shmem's heap figures are written once**, when the shmem is made (upstream re-queries them after every BO:
  about five ioctls per BO, and the host's heaps hold none of the guest's BOs).
- **The response shmem is in the pool** (as the fork and as drm2kgsl's msm shmem): first-fit from the 8 MiB
  drm-host pool, page-granular, zeroed when handed out, its first 2 MiB never used (drm2kgsl's guard: the same
  guest-visible offsets); MAP_BLOB answers its offset with MAP_CACHE_CACHED. Freed at its unref.
- **One syncobj per ring of a context**, made at its first submission and kept (upstream one per CS_SUBMIT); the CS
  puts its fence there (`SYNCOBJ_OUT`), and `submit_fence` exports it as a sync_file only when the ring had work
  since its last fence. A fence with no work since the ring's previous fence retires after that one, in order
  (upstream retires it at once even with earlier fences of the ring running); ring 0 and an idle ring at once. No
  in-fence (the device has none to give; the fork has none either).
- **The rings** are the HW IP rings counted once at the open (instance 0, each IP type's `available_rings`, in type
  order: the guest's `virtio_ring_mapping`), not per context.
- **Each context opens the node again** (as upstream: its own DRM file and GPU VM per guest process; the kernel's
  implicit sync covers what two share). A display surface is imported into a context when it is attached; unref
  closes every context's handles of it.
- **The capset's gpu_info** is Mesa's `ac_drm_query_gpu_info` (libdrm's `amdgpu_query_gpu_info_init`): DEV_INFO, then
  GB_ADDR_CONFIG by `AMDGPU_INFO_READ_MMR_REG` 0x263e (a refusal fails the probe: the guest's addrlib needs it), the
  pre-Vega registers only below family 141. `address32_hi` as libdrm computes it from the high VA range;
  `alignments` = pte_fragment_size, gart_page_size. The marketing name is not libdrm's table (the host has no
  `amdgpu.ids`): `AMD Radeon (DEVICE_ID)`, or `Samsung Xclipse` on an `sgpu` driver.

**The driver name is a guess in part.** The node is taken when DRM_IOCTL_VERSION says `amdgpu` ([F] the A56 fork's
`docs/a56-guide.md`: "amdgpu (v3.49.0) over /dev/dri/renderD128") or `sgpu` (Samsung's own name for its Xclipse driver:
a guess, not seen on an A56). `gpu-node=PATH` (xnuhost.conf or `--set`) names another node.

**Protocol coverage** (every ccmd Mesa cecc0e9's `amdgpu_virtio*.c` sends): QUERY_INFO (passthrough of any
`AMDGPU_INFO_*`, up to 1 MiB, the kernel's errno in `ret`), GEM_NEW, BO_VA_OP (MAP, UNMAP, REPLACE, CLEAR; sparse),
CS_SUBMIT (BO_HANDLES, FENCE, DEPENDENCIES, IB: the four Mesa forwards; its SYNCOBJ chunks become virtgpu syncobjs in
the guest and never reach the host; up to 128 chunks, the IB 8 words), SET_METADATA (every chunk, kept for
BO_QUERY_INFO), BO_QUERY_INFO, CREATE_CTX (and its DESTROY flag), RESERVE_VMID (and UNRESERVE), SET_PSTATE (GET and
SET), CS_QUERY_FENCE_STATUS (WAIT_CS, a relative timeout made absolute as libdrm does; fence 0 is done). **Not
served:** the in-fence (above); `resource_map` of a host BO (there are none: K2); `export_opaque_handle` (nothing on
the host imports a guest BO).

**The self-test** (`selftest.rs`; xnuhost runs it at the start before it picks this renderer, `gpuctl amdgpu` alone):
1. the fork's steps by raw ioctls: a udmabuf of a sealed memfd imported (PRIME), mapped (GEM_VA), an amdgpu context, a
   BO list, an IB of eight `0x80000000` (the fork's GFX PACKET2 NOP, PASS on the A56 2026-11-13/14 in its record)
   submitted, its fence waited for (WAIT_CS, 1 s);
2. the same through the renderer's own Gpu3d calls over a memfd of its own (8 MiB, pool in its last 4): the capset
   (592 bytes, context type 2), a context, its shmem from the pool, QUERY_INFO DEV_INFO answered there (the seqno),
   GEM_NEW and a HOST3D_GUEST blob of 3 udmabufs (one per page, 3 pages apart), CREATE_CTX, BO_VA_OP over the three,
   CS_SUBMIT of the NOP IB (in the BO's last chunk) with the BO in its list on the GFX ring, the virtio fence retired
   through the retire callback (at most 2 s).
Nothing outlives it (its memfd, renderer, fence thread and DRM files go when it returns); no realtime thread.

**The choice** (`session/board.rs`, `gpu3d_amdgpu::choose`). The `galaxy-a56` row's default is now `gpu=auto`
(`runtime/bridges/hw/host_device.rs`; gpu=2d before). With `gpu=auto` or `gpu=3d`:
- a host with `/dev/kgsl-3d0`, or one with no amdgpu render node: the KGSL path, its code and lines unchanged (the
  amdgpu checks are not even asked: test `the_choice_of_renderer`);
- an amdgpu node without KGSL: the renderer when **(a)** the kernel's gpuaddon sidecar carries the OneiOSRadeonMetal
  plug-in whose `oneios-target` names the image set's build, and **(b)** the self-test passes; then
  `oneios-gpu-amd=1` is added to the boot arguments (K5);
- otherwise 2D exactly as before, one line saying why; with `gpu=3d` the session refuses to start instead.
`gpu=2d` (the command line, the image's `xnuhost.conf`) gives the 2D path with no GPU pool, as before. When (a) or (b)
fails under `gpu=auto` the 8 MiB GPU pool stays reserved and unused (as a KGSL phone whose KGSL fails).

**Host tests** (`cargo test -p xnuhost --lib gpu3d_amdgpu`, in `runtime/host-tests.sh`): the capset's bytes;
`address32_hi` without a high range; only an amdgpu or sgpu node; a DRM file per context, EEXIST, a wrong capset;
the shmem in the pool past 2 MiB (its header, heaps, a second refused, a HOST3D blob with blob_id != 0 refused, the
range zeroed and reused after unref); QUERY_INFO in the shmem (seqno, a refused query's errno, two in one submit, the
guest's length bound, too large, bad lengths, unknown command, trailing bytes, a response outside the shmem); GEM_NEW
and a 3-chunk blob, BO_QUERY_INFO from GEM_NEW, SET_METADATA on each chunk; every refusal of blobs (no GEM_NEW, size
mismatch, blob_id 0 or over 32 bits, GEM_NEW's padding, blob_id in use, size 0, async error counts); a display
surface imported where attached; VA maps across chunks (whole, a part starting and ending inside chunks, REPLACE,
CLEAR, sparse, refusals, a failed piece unmapping the ones before it); a submission's BO list expansion, the user
fence's chunk, dependencies, the ring's syncobj made once, an IB outside the list's maps refused by the stand-in; every
CS refusal (ring 0, ring 8, unknown context or resource, two lists, an IB of 6 words, chunk 0x0a, fence offsets,
129 chunks, descriptors past the request, data past it); contexts, pstate, VMID, fence queries; fences retiring in
order when the later work finishes first, rings apart, a context's fences dropped with it; the self-test passing and
failing (a GPU that never finishes, a refused import, no node); the plug-in in the sidecar (found, other build, none,
NativeAdreno only, no oneios-target, not NGPUADD2, cut); the choice (S24 Ultra stays KGSL, A56 with/without the
plug-in, self-test pass/fail, gpu=3d); a blob of 1100 pages through the real `/dev/udmabuf` (two or more chunks
mapped end to end); the layout. The config's tests: the A56 row's `gpu=auto` and `gpu-node=`. `xh_core::inflate`'s
tests (stored, fixed and dynamic blocks; every refusal).

**The A56 log lines that prove it** (xnuhost.log; `oneios-tool log` / `phone log --grep gpu`):
```
host: galaxy-a56 (Galaxy A56 (SM-A566, Exynos 1580)), matched by ro.product.model=SM-A566E; read: ...; defaults: gpu=auto, panel-fallback=1080x2340@120, width=720, height=1560, scale=2, hz=60, return-graceful-ms=4000, hw-backend-haptics=treble, hw-backend-fallback=1
sidecar <kernel>.gpuaddon: N bytes
gpu: self-test: PASS raw: guest memory (a udmabuf of a sealed memfd) imported into /dev/dri/renderD128 and mapped at GPU address 0x...
gpu: self-test: PASS raw: an IB of 8 GFX NOPs submitted (context N, sequence N)
gpu: self-test: PASS raw: the GPU ran it (fence N done)
gpu: amdgpu on /dev/dri/renderD128: amdgpu 3.49.0, device 0x.... rev 0x.. family ..., GPU addresses 0x...-0x... (high 0x...-0x...), GB_ADDR_CONFIG 0x........, rings GFXx1 COMPUTEx. DMAx. ... (N virtio rings); pool 4 MiB; ...   (the self-test's renderer)
gpu: context 1 xnu:self-test: amdgpu, its own GPU VM on /dev/dri/renderD128, N rings
gpu: self-test: PASS renderer: a context, its shmem at pool offset 0x200000, QUERY_INFO answered there (device 0x....)
gpu: self-test: PASS renderer: GEM_NEW and a guest blob of 3 udmabufs, mapped as one BO at GPU address 0x...
gpu: self-test: PASS renderer: CS_SUBMIT of the NOP IB on ring 1 (GFX), its virtio fence retired through the retire callback in N us
gpu: amdgpu on /dev/dri/renderD128: ... pool 8 MiB; ...                                                (the session's renderer)
gpu: 3D on amdgpu (/dev/dri/renderD128): the gpuaddon's OneiOSRadeonMetal plug-in (iPhone10,3 20F66) and the self-test passed; oneios-gpu-amd=1
gpu: context N xnu:backboardd: amdgpu, its own GPU VM on /dev/dri/renderD128, N rings              (the first guest context)
gpu: 10 s host: amdgpu fences N, fence->retired mean N us max N us; submissions N, mean N us max N us; blobs made N, mean N us max N us (N udmabufs)
```
The `oneios-gpu-amd=1` at the end of the `gpu: 3D on amdgpu` line is read back from the boot arguments after the word was added (K5; "the command line's oneios-gpu-amd= is kept" when the command line named it already). Without the plug-in or with a failed
self-test, instead: `gpu: 2D only: the gpuaddon has no OneiOSRadeonMetal plug-in (the A56's Metal)` (or `the kernel
has no gpuaddon sidecar, so no ...`, `the OneiOSRadeonMetal plug-in is for "...", the image set is build ...`, `the
amdgpu self-test failed (gpu: self-test: FAIL at STEP: WHY)`). Errors are `gpu: amdgpu: ...` lines (the first 64,
then every 1000th). By hand: `adb shell su -c '/data/local/tmp/gpuctl amdgpu'` (last line `ok` or `FAILED at: ...`).

**What the other stages must match:** the kernel stage publishes the A56 accelerator only with `oneios-gpu-amd=1`
(K5) and passes the DROIDVM_VIRTGPU_CREATE_BLOB size unchanged (the host refuses a size other than GEM_NEW's); the
guest's GEM_NEW precedes its blob in the same order Mesa's `vdrm_bo_create` sends them (the host takes GEM_NEW's
record when the blob comes). The Metal stage's sidecar must hold `addons/OneiOSRadeonMetal.bundle/OneiOSRadeonMetal`
and `addons/OneiOSRadeonMetal.bundle/oneios-target` (`oneios-tool ios addon`'s `metal_files` names only
NativeAdrenoMetal today). **Not done here:** nothing has run on an A56; the IO coherence of imported guest pages on
the Xclipse (the guest maps them cacheable; whether the GPU snoops GTT there is [U]: a render test on the phone
shows it); a per-context GPU policy like KGSL's 2.2b (none).

## 3. Guest: Mesa radeonsi for Darwin over the virtio amdgpu context

(Filled by the guest Mesa stage.)

## 4. Guest: the AIR Metal layer and the A56's Metal plug-in

(Filled by the Metal stage.)

## 5. Guest kernel: the accelerator on an amdgpu host

(Filled by the kernel stage.)

## 6. Safety, switches, and what is left

- `gpu=2d` (the command line, the image's `overlay/userland/xnuhost.conf`, or the `galaxy-a56` row) always gives the
  2D path of today. The way back to One UI is not touched by any part of this port.
