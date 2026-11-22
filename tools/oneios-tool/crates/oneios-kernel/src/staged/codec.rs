// Written with `oneios-tool kernel gen codec` (2026-11-15): no Python original.
/// DroidVMVTHost.cpp's lines that change.
///
/// 1. The hvcC's profile, tier and level (2026-11-15): upstream copies the twelve bytes after the SPS's first payload
///    byte as they are in the stream. Every HEVC SPS has emulation prevention bytes in those twelve (Main's
///    compatibility flags are 60 00 00 00, which the stream carries as 60 00 00 03 00, and the constraint flags' zeros
///    get two more), so the plain copy wrote them into the hvcC as flags and never reached general_level_idc (hvcC
///    byte 12 was a flags byte, 0 for a stream of MediaCodec's or x265's). The twelve bytes are taken without them.
/// 2. iOS 27's tile session (2026-11-22). Upstream's exchange was recorded against macOS's host and has no tiles; iOS
///    27 (24A446) CMPhoto writes every HEIC through VideoToolbox's tile encoder (`ParavirtualizedVideoEncoder_
///    StartTileSession`), so every widget snapshot was `+enc ests einv -enc`: `ests` had the unknown type's -12900,
///    VideoToolbox gave the session up and chronod tried again, about 3 sessions a second, no frame encoded. The
///    messages, as 24A446's VideoToolbox writes and reads them (its own host, `vtParavirtualizationHostEncoderSession_
///    HandleEncoder*Tile*`, and its guest, `ParavirtualizedVideoEncoder_*Tile*` and `paravirtualizedVideoEncoder_
///    HandleMessageFromHost`):
///      ests  StartTileSession: ,twh the tile size (VTInt32Size, "=32s": width, height), ,esp the session's
///            properties. Answered ,err. The device is opened at the tile size, and the host sends TEsa (,twh and
///            ,pba, the tiles' pixel buffer attributes: 420v), as esta sends Epba.
///      epti  PrepareToEncodeTiles (,ept): ,err.
///      etil  EncodeTile: ,til the tile ID (s64), ,tio its origin in the surface ("=32p": x, y), ,tia its size (at
///            most the tile size), ,tip options, ,uui the pixel buffer's UUID and the surface. Answered ,err ,efl (1,
///            asynchronous); the tile is copied from its part of the surface and coded as a frame is (every frame of
///            the phone's encoder is a key frame), then handed over by TEcd (,cod ,wdt ,hgt ,ext: the guest makes the
///            format description, as Efds) and TEem (,til ,err ,efl ,sbf ,uui, as Eemt with ,til for ,frm).
///      ecti  CompleteTiles: ,err once every tile given is handed over (ecfr's drain and deadline).
///    TEsr (the host's tile encode requirements) is optional and not sent.
/// 3. A small HEVC session (2026-11-22): the phone's c2.qti encoders code nothing below 96x96 (chronod's widget tiles
///    are 84x84). An HEVC session smaller than MinCodedSize is coded at MinCodedSize, the part past its size the last
///    column and row repeated, and the codec's SPS gets the conformance window that crops it back to the session's
///    size (only the window's fields change; the bits before and after it are the codec's). The copy and the crop are
///    kernel/patches/oneios-hosted/codec/OneiOSCodecPicture.h (plain C, included with -I that folder), which
///    kernel/tests/host.sh tests on the host: tests/test_picture.c, and tests/crop.sh with x265's and VideoToolbox's
///    streams decoded by ffmpeg.
/// 4. `jcap` (2026-11-22): VTParavirtualizedJPEGSessionCreate's capability query (SpringBoard), answered
///    kVTCouldNotFindVideoEncoderErr (-12908): the host has no JPEG codec and the guest keeps its own; it was counted
///    as an unknown type.
pub const CODEC_HOST: &[(&str, &str)] = &[
    ("    memcpy(out + at, s->sps + 3, 12); at += 12;   // profile space/tier/idc, compatibility flags, constraint flags, level\n",
     concat!("    // OneiOS (kernel gen codec): the twelve bytes without the stream's emulation prevention bytes (00 00 03 -> 00 00),\n",
             "    // counted from the SPS's first payload byte; upstream copied them as they are in the stream.\n",
             "    {\n",
             "        uint32_t zeros = 0, payload = 0, taken = 0;\n",
             "        for (uint32_t i = 2; i < s->spsSize && taken < 12; ++i) {\n",
             "            const uint8_t byte = s->sps[i];\n",
             "            if (zeros >= 2 && byte == 3) { zeros = 0; continue; }\n",
             "            zeros = byte ? 0 : zeros + 1;\n",
             "            if (payload++ >= 1) out[at + taken++] = byte;   // profile space/tier/idc, compatibility flags, constraint flags, level\n",
             "        }\n",
             "        if (taken < 12) return 0;\n",
             "        at += 12;\n",
             "    }\n")),
    // The part of the surface a frame is.
    ("        SurfaceView view;\n        bool fed;\n    } frames[64];\n",
     r#"        SurfaceView view;
        bool fed;
        // OneiOS (kernel gen codec): the part of the surface the frame is (a tile's origin and size; a frame's is all of it).
        uint32_t originX, originY, areaWidth, areaHeight;
    } frames[64];
"#),
    // The tile session's state.
    ("    bool describedToGuest = false;\n",
     r#"    bool describedToGuest = false;
    // OneiOS (kernel gen codec): a tile session (ests: TEsa, TEcd and TEem in place of Epba, Efds and Eemt); the size the
    // device codes (larger than the session's for an HEVC session below MinCodedSize, whose SPS then crops it back: spsIn
    // is the SPS as the codec wrote it); the type the outstanding ecfr or ecti is answered with.
    bool tiles = false;
    uint32_t codedWidth = 0, codedHeight = 0;
    uint8_t spsIn[256] = {};
    uint32_t spsInSize = 0;
    uint32_t finishType = 0;
"#),
    // TEcd for a tile session.
    ("        w.header(cc(\"Efds\"), FlagHostRequest, s->uuid, request);\n",
     "        w.header(s->tiles ? cc(\"TEcd\") : cc(\"Efds\"), FlagHostRequest, s->uuid, request);   // OneiOS: TEcd, a tile session's\n"),
    // TEem for a tile session.
    ("        w.header(cc(\"Eemt\"), last ? FlagHostRequest : 0x10, s->uuid, last ? request : 0);\n        if (first) {\n            w.s64(cc(\",frm\"), f.frame);\n",
     r#"        // OneiOS (kernel gen codec): a tile session's is TEem, the tile's ID ,til.
        w.header(s->tiles ? cc("TEem") : cc("Eemt"), last ? FlagHostRequest : 0x10, s->uuid, last ? request : 0);
        if (first) {
            w.s64(s->tiles ? cc(",til") : cc(",frm"), f.frame);
"#),
    // The frame's part of the surface into the device's size.
    (r#"    const SurfaceView view = f->view;
    const int64_t frame = f->frame;
    IOLockUnlock(s->lock);
    Mapped &out = s->output[index];
    const uint32_t w = s->width, h = s->height, need = w * h * 3 / 2;
    IOMemoryDescriptor *memory = IOMemoryDescriptor::withAddressRange(view.base, view.size, kIODirectionInOut, s->client->task);
    IOMemoryMap *map = memory && memory->prepare() == kIOReturnSuccess ? memory->createMappingInTask(kernel_task, 0, kIOMapAnywhere) : nullptr;
    const uint8_t *surface = map ? reinterpret_cast<const uint8_t *>(map->getVirtualAddress()) : nullptr;
    if (surface && need <= out.length) {
        for (uint32_t y = 0; y < h && y < view.plane[0].height; ++y)
            memcpy(out.bytes + uint64_t(y) * w, surface + view.plane[0].offset + y * view.plane[0].bytesPerRow, w);
        for (uint32_t y = 0; y < h / 2 && y < view.plane[1].height; ++y)
            memcpy(out.bytes + uint64_t(w) * h + uint64_t(y) * w, surface + view.plane[1].offset + y * view.plane[1].bytesPerRow, w);
    } else {
"#,
     r#"    const SurfaceView view = f->view;
    const int64_t frame = f->frame;
    const uint32_t originX = f->originX & ~1u, originY = f->originY & ~1u, areaWidth = f->areaWidth, areaHeight = f->areaHeight;
    IOLockUnlock(s->lock);
    Mapped &out = s->output[index];
    // OneiOS (kernel gen codec): the device's size (codedWidth x codedHeight), from the frame's part of the surface.
    const uint32_t w = s->codedWidth, h = s->codedHeight, need = w * h * 3 / 2;
    IOMemoryDescriptor *memory = IOMemoryDescriptor::withAddressRange(view.base, view.size, kIODirectionInOut, s->client->task);
    IOMemoryMap *map = memory && memory->prepare() == kIOReturnSuccess ? memory->createMappingInTask(kernel_task, 0, kIOMapAnywhere) : nullptr;
    const uint8_t *surface = map ? reinterpret_cast<const uint8_t *>(map->getVirtualAddress()) : nullptr;
    if (surface && need <= out.length && copyArea(out.bytes, w, h, surface, view, originX, originY, areaWidth, areaHeight)) {
        // copied
    } else {
"#),
    // The picture helpers of OneiOSCodecPicture.h (kernel/patches/oneios-hosted/codec, -I), host-tested.
    ("#include \"DroidVMVTWire.h\"\n",
     "#include \"DroidVMVTWire.h\"\n#include \"OneiOSCodecPicture.h\"   // OneiOS (kernel gen codec): oneios_nv12_copy_area, oneios_hevc_sps_crop\n"),
    // The copy of a frame's part (before feedFrame, which calls it).
    ("// The oldest frame not yet fed, into a free OUTPUT buffer as tightly packed NV12 (the lock held, released for the copy).\n",
     r#"// OneiOS (kernel gen codec): the part (originX, originY: even; areaWidth x areaHeight) of an NV12 surface into a tightly
// packed NV12 picture of w x h; what lies past the part, or past the surface, repeats the last column and row copied (a
// tile at the image's edge; a session coded larger than its size, whose SPS crops it away). OneiOSCodecPicture.h.
bool copyArea(uint8_t *to, uint32_t w, uint32_t h, const uint8_t *surface, const SurfaceView &view, uint32_t originX, uint32_t originY,
              uint32_t areaWidth, uint32_t areaHeight) {
    const oneios_nv12_plane planes[2] = {{view.plane[0].offset, view.plane[0].width, view.plane[0].height, view.plane[0].bytesPerRow},
                                         {view.plane[1].offset, view.plane[1].width, view.plane[1].height, view.plane[1].bytesPerRow}};
    return oneios_nv12_copy_area(to, w, h, surface, planes, originX, originY, areaWidth, areaHeight) != 0;
}

// The oldest frame not yet fed, into a free OUTPUT buffer as tightly packed NV12 (the lock held, released for the copy).
"#),
    // The SPS's crop (before handleCoded, which calls it).
    ("// A coded CAPTURE buffer: its parameter sets kept, its picture handed over.\n",
     r#"// OneiOS (kernel gen codec): the SPS of an HEVC session coded larger than its size (spsIn, as the codec wrote it) with the
// conformance window that crops the coded picture to the session's width x height, into sps. OneiOSCodecPicture.h.
bool cropSps(Session *s) {
    s->spsSize = oneios_hevc_sps_crop(s->spsIn, s->spsInSize, s->width, s->height, s->sps, sizeof s->sps);
    return s->spsSize != 0;
}

// A coded CAPTURE buffer: its parameter sets kept, its picture handed over.
"#),
    // The SPS of a session coded larger, kept with its crop.
    (r#"        if (set) {
            if (n <= 256 && (n != *setSize || memcmp(set, nal, n))) {
                memcpy(set, nal, n);
                *setSize = n;
                s->describedToGuest = false;
            }
            continue;
        }
"#,
     r#"        if (set) {
            // OneiOS (kernel gen codec): the SPS of a session coded larger than its size is compared as the codec wrote it
            // and kept with the window that crops it (an SPS that cannot be read keeps none: no description, step 102).
            const bool crop = set == s->sps && (s->codedWidth != s->width || s->codedHeight != s->height);
            uint8_t *seen = crop ? s->spsIn : set;
            uint32_t *seenSize = crop ? &s->spsInSize : setSize;
            if (n <= 256 && (n != *seenSize || memcmp(seen, nal, n))) {
                memcpy(seen, nal, n);
                *seenSize = n;
                if (crop && !cropSps(s)) failed(102, int32_t(n));
                s->describedToGuest = false;
            }
            continue;
        }
"#),
    // ecti is answered as ecfr is.
    ("    h.type = cc(\"ecfr\");\n",
     "    h.type = s->finishType ? s->finishType : cc(\"ecfr\");   // OneiOS: ecfr or ecti, the one outstanding\n"),
    // The encoder's open at a size of its own, and the tile session's start.
    (r#"int startEncoder(Session *s, const uint8_t *m, uint32_t n) {
    Param size;
    if (!param(m, n, cc(",vdi"), size) || size.size < 8) return -12902;
    const uint32_t width = be32(size.value), height = be32(size.value + 4);
    if (!width || !height || (width & 1) || (height & 1)) return -12902;
    teardown(s);
    s->width = width;
    s->height = height;
"#,
     r#"// OneiOS (kernel gen codec): the smallest size the phone's encoders code (c2.qti's 96x96); an HEVC session smaller is coded
// at this size and cropped back by its SPS (cropSps).
constexpr uint32_t MinCodedSize = 96;

// OneiOS (kernel gen codec): TEsa, the pixel buffers a tile session takes (420v; the tile size ,twh, "=32s": width and
// height). Sent, not waited for, as Epba is.
void tileAttributes(Session *s) {
    Plist *p = new Plist();
    uint8_t buffer[512];
    if (!p) return;
    const int format = p->integer(875704438);   // '420v'
    const int keys[4] = {p->string("PixelFormatType"), p->string("BytesPerRowAlignment"), p->string("PlaneAlignment"),
                         p->string("IOSurfaceProperties")};
    const int values[4] = {p->array(&format, 1), p->integer(64), p->integer(64), p->dict(nullptr, nullptr, 0)};
    const int attributes = p->dict(keys, values, 4);
    Writer w(buffer, sizeof buffer - 24);
    w.header(cc("TEsa"), FlagHostRequest, s->uuid, s->nextHostRequest++);
    w.plist(cc(",pba"), *p, attributes);
    delete p;
    if (!w.ok()) { failed(20, int32_t(w.size())); return; }
    uint8_t *size = buffer + w.size();
    put32(size, 24);
    put32(size + 4, cc("para"));
    put32(size + 8, cc(",twh"));
    put32(size + 12, cc("=32s"));
    put32(size + 16, s->width);
    put32(size + 20, s->height);
    droidvm_vthost_replies = droidvm_vthost_replies + 1;
    s->client->send(s->client->transport, buffer, w.size() + 24);
}

int openEncoder(Session *s, uint32_t width, uint32_t height, bool tiles);

int startEncoder(Session *s, const uint8_t *m, uint32_t n) {
    Param size;
    if (!param(m, n, cc(",vdi"), size) || size.size < 8) return -12902;
    return openEncoder(s, be32(size.value), be32(size.value + 4), false);
}

// OneiOS (kernel gen codec): ests, a tile session (,twh the tile size): the encoder opened at that size.
int startTiles(Session *s, const uint8_t *m, uint32_t n) {
    Param size;
    if (!param(m, n, cc(",twh"), size) || size.size < 8) return -12902;
    return openEncoder(s, be32(size.value), be32(size.value + 4), true);
}

// esta's and ests's open: width x height, coded at least MinCodedSize in each direction for HEVC.
int openEncoder(Session *s, uint32_t width, uint32_t height, bool tiles) {
    if (!width || !height || (width & 1) || (height & 1) || width > 16384 || height > 16384) return -12902;
    teardown(s);
    s->width = width;
    s->height = height;
    s->tiles = tiles;
    s->codedWidth = hevcSession(s) && width < MinCodedSize ? MinCodedSize : width;
    s->codedHeight = hevcSession(s) && height < MinCodedSize ? MinCodedSize : height;
    s->spsInSize = 0;
    s->finishType = 0;
"#),
    (r#"    f.type = TypeCaptureMplane;
    f.width = width;
    f.height = height;
    f.pixelformat = hevcSession(s) ? cc("CVEH") : cc("462H");
    f.num_planes = 1;
    f.planes[0].sizeimage = width * height / 2 + 65536;
"#,
     r#"    f.type = TypeCaptureMplane;
    f.width = s->codedWidth;     // OneiOS: the device's size
    f.height = s->codedHeight;
    f.pixelformat = hevcSession(s) ? cc("CVEH") : cc("462H");
    f.num_planes = 1;
    f.planes[0].sizeimage = s->codedWidth * s->codedHeight / 2 + 65536;
"#),
    (r#"    f.type = TypeOutputMplane;
    f.width = width;
    f.height = height;
    f.pixelformat = cc("21VN");   // V4L2_PIX_FMT_NV12
    f.num_planes = 1;
    f.planes[0].bytesperline = width;
    f.planes[0].sizeimage = width * height * 3 / 2;
"#,
     r#"    f.type = TypeOutputMplane;
    f.width = s->codedWidth;     // OneiOS: the device's size
    f.height = s->codedHeight;
    f.pixelformat = cc("21VN");   // V4L2_PIX_FMT_NV12
    f.num_planes = 1;
    f.planes[0].bytesperline = s->codedWidth;
    f.planes[0].sizeimage = s->codedWidth * s->codedHeight * 3 / 2;
"#),
    ("    encoderPixelBufferAttributes(s);\n    trace(\"encoder session %u %ux%u %c%c%c%c\", s->v4l2, width, height, s->codec >> 24, s->codec >> 16, s->codec >> 8, s->codec);\n",
     r#"    if (tiles) tileAttributes(s);   // OneiOS: TEsa, a tile session's
    else encoderPixelBufferAttributes(s);
    trace("encoder session %u %ux%u%s coded %ux%u %c%c%c%c", s->v4l2, width, height, tiles ? " tiles" : "", s->codedWidth,
          s->codedHeight, s->codec >> 24, s->codec >> 16, s->codec >> 8, s->codec);
"#),
    // One surface lookup for frames and tiles; the tile's queue.
    ("// efrm: the frame's IOSurface looked up on the guest's thread, the copy left to the worker.\n",
     r#"// OneiOS (kernel gen codec): the IOSurface of a frame or tile to encode (the app's, given to the codec process), looked up
// once and kept by ID (upstream's lookup of efrm, shared with etil).
bool encodeSurface(Session *s, uint32_t id, SurfaceView &view) {
    bool surface = false;
    IOLockLock(s->surfaceLock);
    for (unsigned i = 0; i < s->knownCount && !surface; ++i)
        if (s->known[i].id == id) { view = s->known[i].view; surface = true; }
    if (!surface) {
        if (!s->surfaces) s->surfaces = openSurfaceClient(s->client->task);
        if (!s->holder) s->holder = processSurfaceClient(s->client->pid);
        surface = s->surfaces && s->holder && lookupSurface(s->surfaces, id, view, s->holder);
        if (surface && s->knownCount < 48) s->known[s->knownCount++] = {id, view};
    }
    IOLockUnlock(s->surfaceLock);
    return surface;
}

// OneiOS (kernel gen codec): etil, a tile (,til its ID, ,tio its origin in the surface, ,tia its size: at most the tile
// size, ,uui its pixel buffer's UUID, the surface with it), queued as a frame is, with no times (a tile has none).
int queueEncodeTile(Session *s, const uint8_t *m, uint32_t n, const uint32_t *surfaces, unsigned surfaceCount) {
    int64_t tile = 0;
    Param origin, area, uuid;
    if (!s->outputStreaming || !surfaceCount || !paramS64(m, n, cc(",til"), tile) || !param(m, n, cc(",tio"), origin) ||
        origin.size < 8 || !param(m, n, cc(",tia"), area) || area.size < 8 || !param(m, n, cc(",uui"), uuid) || uuid.size < 16) {
        failed(100, int32_t(n));
        return -12902;
    }
    const int32_t x = int32_t(be32(origin.value)), y = int32_t(be32(origin.value + 4));
    const int32_t width = int32_t(be32(area.value)), height = int32_t(be32(area.value + 4));
    if (x < 0 || y < 0 || width < 1 || height < 1 || uint32_t(width) > s->width || uint32_t(height) > s->height) {
        failed(100, width);
        return -12902;
    }
    SurfaceView view;
    if (!encodeSurface(s, surfaces[0], view)) { failed(97, int32_t(surfaces[0])); return -12902; }
    if (view.format != cc("420v") && view.format != cc("420f")) { failed(101, int32_t(view.format)); return -12902; }
    IOLockLock(s->lock);
    if (s->frameHead - s->frameTail >= 64) { IOLockUnlock(s->lock); failed(98, 0); return -12911; }
    Session::Frame &f = s->frames[s->frameHead++ % 64];
    f.frame = tile;
    memcpy(f.uuid, uuid.value, 16);
    memset(f.pts, 0, 24);
    memset(f.duration, 0, 24);
    f.view = view;
    f.fed = false;
    f.originX = uint32_t(x);
    f.originY = uint32_t(y);
    f.areaWidth = uint32_t(width);
    f.areaHeight = uint32_t(height);
    IOLockUnlock(s->lock);
    IOLockWakeup(s->lock, &s->events, false);
    droidvm_vthost_frames_in = droidvm_vthost_frames_in + 1;
    return 0;
}

// efrm: the frame's IOSurface looked up on the guest's thread, the copy left to the worker.
"#),
    (r#"    SurfaceView view;
    bool surface = false;
    IOLockLock(s->surfaceLock);
    for (unsigned i = 0; i < s->knownCount && !surface; ++i)
        if (s->known[i].id == surfaces[0]) { view = s->known[i].view; surface = true; }
    if (!surface) {
        if (!s->surfaces) s->surfaces = openSurfaceClient(s->client->task);
        if (!s->holder) s->holder = processSurfaceClient(s->client->pid);
        surface = s->surfaces && s->holder && lookupSurface(s->surfaces, surfaces[0], view, s->holder);
        if (surface && s->knownCount < 48) s->known[s->knownCount++] = {surfaces[0], view};
    }
    IOLockUnlock(s->surfaceLock);
    if (!surface) { failed(97, int32_t(surfaces[0])); return -12902; }
    IOLockLock(s->lock);
    if (s->frameHead - s->frameTail >= 64) { IOLockUnlock(s->lock); failed(98, 0); return -12911; }
    Session::Frame &f = s->frames[s->frameHead++ % 64];
    f.frame = frame;
"#,
     r#"    SurfaceView view;
    if (!encodeSurface(s, surfaces[0], view)) { failed(97, int32_t(surfaces[0])); return -12902; }   // OneiOS: the shared lookup
    IOLockLock(s->lock);
    if (s->frameHead - s->frameTail >= 64) { IOLockUnlock(s->lock); failed(98, 0); return -12911; }
    Session::Frame &f = s->frames[s->frameHead++ % 64];
    f.frame = frame;
    f.originX = f.originY = 0;   // OneiOS: a frame is all of its surface
    f.areaWidth = s->width;
    f.areaHeight = s->height;
"#),
    // The new types.
    ("    case cc(\"esta\"): replyStatus(c, h, s ? startEncoder(s, m, n) : -12903); break;\n",
     r#"    case cc("esta"): replyStatus(c, h, s ? startEncoder(s, m, n) : -12903); break;
    // OneiOS (kernel gen codec): iOS 27's tile session (CMPhoto's HEIC) and its JPEG sessions' capability query.
    case cc("ests"): replyStatus(c, h, s ? startTiles(s, m, n) : -12903); break;
    case cc("epti"): replyStatus(c, h, s && s->tiles && s->outputStreaming ? 0 : -12903); break;
    case cc("etil"): {
        const int error = s && s->tiles ? queueEncodeTile(s, m, n, surfaces, surfaceCount) : -12903;
        uint8_t buffer[128];
        Writer w(buffer, sizeof buffer);
        w.header(h.type, FlagReply, h.uuid, h.request);
        w.s32(cc(",err"), error);
        w.u32(cc(",efl"), error ? 0 : 1);   // kVTEncodeInfo_Asynchronous: the tile comes in a TEem
        sendToGuest(c, w, buffer);
        break;
    }
    case cc("jcap"): replyStatus(c, h, -12908); break;   // kVTCouldNotFindVideoEncoderErr: no JPEG codec here
"#),
    ("    case cc(\"ecfr\"):\n        if (s && s->workerRunning) {\n            IOLockLock(s->lock);\n            s->finishWanted = true;\n",
     "    case cc(\"ecfr\"): case cc(\"ecti\"):   // OneiOS: ecti, a tile session's, answered as ecfr\n        if (s && s->workerRunning) {\n            IOLockLock(s->lock);\n            s->finishType = h.type;\n            s->finishWanted = true;\n"),
];
