# userland/camera: the capture plug-in (iOS side of the camera)

Owner: camera (was fable-camera). The host side: `runtime/bridges/camera`. The kernel side: `drivers/camera`. The
whole path: `docs/hardware.md` section 7.

**State (2026-11-12): built for iOS 27 with Xcode (arm64e, 4 KiB pages, `macho-check.py` PASS against the guest's own
24A446 stubs) and run on the Mac against macOS 27's CMCapture (35 checks).** In iOS on the phone (2026-11-22, iOS's log
`out/session-logs/2026-11-22/s7-ioslog`): `Camera driver AppleH9CamIn is active`, `loaded`, `the device made: stream 0
Back Camera (17 formats)`, `stream 1 Front Camera (8 formats)`, `Created FigCaptureDeviceRef`, `user client opened by
cameracaptured`; then CMCapture made no source: `Fig assert: "modelSpecificAVCaptureSessionPlist"`, err -12780,
`Created 0 in-memory built-in backings`. Its per-model folder comes from MobileGestalt's `HWModelDescriptionForCamera`
(`/product unique-model`), which the guest's tree did not have; the loader writes it now with the camera nodes
(`docs/hardware.md` 7, "The camera nodes"), and hosted 0084 removes MobileGestalt's cache once when the tree changes. Not run yet.

## Where the camera broke (iOS's own log, sessions of 2026-11-12)

iOS's capture stack never touched our camera. Every start of `cameracaptured` (21 a session) logged, in order:

```
<<<< FigCaptureCommon >>>> fcc_probeCameraDriverService_block_invoke: Camera driver not started in time
<<<< FigCaptureCommon >>>> FigCaptureGetCaptureDeviceCreateFunction: Failed to load CaptureDeviceCreate function
<<<< BWFigCaptureDeviceVendor >>>> -[BWFigCaptureDeviceVendor _createDevice:reason:clientPID:figCaptureDevice:]: Cannot create device without create function!
<<<< FigCaptureSourceBackingsProvider >>>> csu_createSourceInfoDictionariesFromAVCaptureSessionPlistForCaptureDeviceIDs: Error -12786 while creating FigCaptureDevice.
<<<< FigCaptureSourceBackingsProvider >>>> cs_getBackingsForBuiltInCameras: 0 total in-memory backings
```

So every client saw no camera (`callservicesd`: "Supports Back Camera: NO"). The kernel side was up
(`ONEIOS CAMERA: on: 3 cameras from the host so far`), but nothing in iOS read it: the old plug-in was in no image,
and nothing would have loaded it.

## How iOS 27 finds a camera [F, read in the 24A446 shared cache]

- `fcc_probeCameraDriverService` (CMCapture) adds an `IOServiceMatching(name)` first-match notification for each row
  of its table `sCameraDriverInfo` and waits 5 s:

  | Service class | Plug-in that is `dlopen()`ed | Function looked up |
  |---|---|---|
  | `AppleCamera` | `/System/Library/MediaCapture/ISP.mediacapture` | `ISPCaptureDeviceCreate` |
  | `AppleH16CamIn` | `/System/Library/MediaCapture/H16ISP.mediacapture` (in the shared cache) | `H16ISPCaptureDeviceCreate` |
  | `AppleH13CamIn`, `AppleH10CamIn`, **`AppleH9CamIn`**, `AppleH6CamIn` | `.../H13ISP`, `H10ISP`, **`H9ISP`**, `H6ISP.mediacapture` | **`H9ISPCaptureDeviceCreate`** for H9 |

- `FigCaptureGetCaptureDeviceCreateFunction` loads the matched row's plug-in and symbol, and
  `BWFigCaptureDeviceVendor` makes every device with it: `create(allocator, options, &device)` (x0, x1, x2 at the
  call in `_createDevice:reason:clientPID:figCaptureDevice:`).
- So the camera enters **through Apple's own plug-in loader**: no method of CMCapture is replaced, no
  `DYLD_INSERT_LIBRARIES`. The kernel publishes `AppleH9CamIn` (no kext of the collection has that class; the name
  is only in the sandbox profiles), and this plug-in is `/System/Library/MediaCapture/H9ISP.mediacapture`. That
  folder is empty on the sealed OS volume; hosted patch 0056 shows the Data seed's folder over it. `AppleCamera` is
  left alone: the ANE driver and `mobilerepaird` look for it.
- The object layout, read in `H16ISP.mediacapture` (`kH16ISPCaptureDeviceBaseClass`, `kH16ISPCaptureDeviceClass`,
  `kH16ISPCaptureStreamBaseClass`, `kH16ISPCaptureStreamClass`), is HakiOS's iOS 16.5 layout: base class version 1
  (size, equal, invalidate, finalize, description, copyProperty, setProperty, ...), device class version 3
  (ProcessingSessionCreate, RequestStreams, RelinquishStreams, PearlCalibration), stream class version 2 (Start, Stop,
  AddAttachmentsToSampleBuffer).
- Frames: `cameracaptured` runs sandboxed. Its profile lists `AppleH9CamInUserClient` among the user clients it may
  open; `/dev/oneios-camera` is not known to be allowed. So the plug-in reads frames through that user client
  (`drivers/camera/OneiOSCameraService.cpp`), and uses the node only where the user client is not there.

## Files

| File | What |
|---|---|
| `OneiOSCaptureDevice.c` | `H9ISPCaptureDeviceCreate`: a FigCaptureDevice (CMCaptureDevice's class) with a stream for each of the back wide and the front camera that the kernel lists. Each stream's formats are the device's own `CMCapture.framework/V54/AVCaptureSession.plist` stream formats (`FigCaptureStreamFormats` of "Back Camera", read at the first create, every key kept, so that each AVCaptureDevice format of that file resolves). The HakiOS property set. Frames scaled into the pools the graph gives each output (420f, 420v, BGRA) and handed to its `PixelBufferHandler` |
| `OneiOSCamera.c`, `.h` | the log (`ONEIOS CAMERA: ios: <process>[pid]: ...` to the unified log, `/dev/console` and `/private/var/oneios/log/camera.log`, each where the sandbox lets it), the off switch, a line when it is loaded |
| `oneios_camera_reader.c`, `.h` | the kernel's frames: AppleH9CamIn's user client (`-DONEIOS_CAMERA_IOKIT`, the iOS build) or `/dev/oneios-camera`; checks each header; copies the NV12 planes |
| `build.sh` | `out/userland/camera/H9ISP.mediacapture` and `cdhashes.txt` |
| `tests/host.sh` | CI: the reader against stand-ins (26 checks), then `host-mac.sh` |
| `tests/host-mac.sh`, `test_device_mac.m` | on a Mac, our create function under macOS 27's own CMCapture: the device is of `FigCaptureDeviceGetClassID`'s class; the stream answers (port, formats from a stand-in plist, pixel size, clock, supported properties); **macOS's `BWFigCaptureDeviceVendor` makes a `BWFigCaptureDevice` over ours with our function**; a frame from a stand-in node reaches the PrimaryScaler's handler, scaled into the test's pool (35 checks) |

What the property names are worth: they are HakiOS's (iOS 16). Every property iOS 27 asks for that is not answered,
and every one it sets that is not used, is logged once by name. That list, from the first session with the
plug-in, is the next step.

## Getting it to the phone

```sh
userland/camera/build.sh          # -> out/userland/camera/H9ISP.mediacapture, cdhashes.txt
# then the image build (docs/building.md section 1): the image hook takes out/userland/camera (ONEIOS_CAMERA_PLUGIN=DIR for another)
```

- The image hook (the root tweaks' `camera-plugin` tweak, `oneios-tool image tweaks`) writes it to
  `/private/var/oneios/addons/MediaCapture/H9ISP.mediacapture` in the Data seed and its CDHash to
  `overlay/userland/owner-trust.json` (library validation in `cameracaptured`). **A new seed starts the phone's Data
  store over** (settings, joined Wi-Fi networks, everything iOS wrote).
- The kernel must have AppleH9CamIn and hosted 0056 (one kernel build has both).
- Off without a rebuild: the boot argument `oneios-camera-plugin=0` (no AppleH9CamIn, no mount), or the file
  `/private/var/oneios/camera/off` (the plug-in makes no device).

## What the next session's logs must show

Kernel log (`xnuhost.log`):

```
ONEIOS CAMERA: on: 3 cameras from the host so far, 3 frame buffers of 3110464 bytes, interrupt
ONEIOS CAMERA: AppleH9CamIn published: CMCapture's probe loads /System/Library/MediaCapture/H9ISP.mediacapture
oneios-camera-addon: /private/var/oneios/addons/MediaCapture over /System/Library/MediaCapture: stage 5, status 0 (...)
ONEIOS CAMERA: user client opened by cameracaptured
```

Since the camera nodes (2026-11-22), `xnuhost.log` also has `camera: iOS is told of a rear camera and a front camera
(3 cameras from the helper)`, `identity: <16 hex digits> (/chosen oneios-identity: ...)` and, from hosted 0084,
`oneios-mg-cache: identity ... (marker none): cache removed (0), marker written (0)` on the first boot of an existing
Data store (`(marker the same): cache kept` after it).

iOS's log (and `xnuhost.log` too if the sandbox lets `cameracaptured` write the console): `Camera driver AppleH9CamIn
is active` in place of "not started in time"; `ONEIOS CAMERA: ios: cameracaptured[..]: loaded`; `the device made:
stream 0 Back Camera (17 formats)`; no "Cannot create device without create function"; no `Fig assert:
"modelSpecificAVCaptureSessionPlist"` and a non-zero `cs_getBackingsForBuiltInCameras: Created N in-memory built-in
backings`. MobileGestalt logs no answers: its `still-camera` shows as the Camera icon on the home screen (and, with
`phone pull-data`, in the rebuilt cache). When the Camera app opens:
`device: streams asked for`, `the graph starts the stream`, `the first frame`, `PrimaryScaler: the first picture
given`. Lines that say where it stops: `error '...' trying to load dylib` (the mount or the trust list),
`stage 1 status 2` (no plug-in in this Data store), `the kernel's camera: Operation not permitted` (the sandbox
refused the user client), `no answer for X` (a property to add).

## Not done

1. Run in iOS. Whether the 27 capture graph takes this device as it is, what it asks that is missing (the log
   lists it), and whether it needs more of HakiOS's replaced methods (stabilisation, noise reduction, the portrait
   prewarm): not known.
2. The front camera's plist entry: the iPhone 17 Pro's front device is "Front Ultra Wide Camera" with 4896x4896
   formats; the front stream takes the first "Front..." entry that has formats, and the port `PortTypeFront`. Not checked.
3. The device tree nodes for MobileGestalt (`/arm-io/isp` `camera-rear`, `camera-front`, `/product/camera`,
   `/product unique-model`): added 2026-11-22 (the Camera icon was not there without them: Camera.app's `SBAppTags`
   is `still-camera`). iOSPlus10 saw the app hang with the nodes and no frames on iOS 16; on 27 the source list needs
   `unique-model` first. Not run on the phone yet.
4. Stills above 1920x1080; focus, exposure and zoom from the sensor; the torch; two cameras at once.
