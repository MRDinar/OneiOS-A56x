// SPDX-License-Identifier: BSD-2-Clause-Patent
//
// The host test of OneiOSCodecPicture.h, the code the staged DroidVMVTHost.cpp runs in the kernel (kernel/tests/host.sh
// builds and runs it; tests/crop.sh then decodes a cropped stream with ffmpeg):
//   copy      oneios_nv12_copy_area on made-up NV12 surfaces: a tile inside, a tile at the edges (cut, then repeated),
//             a surface of a padded row, a picture coded larger than the tile, parts that do not start inside
//   copy16    oneios_yuv_copy_area on made-up 10-bit surfaces (x420, xf20: 16-bit samples): into P010 as they are,
//             into NV12 as their high bytes, the same cases; an 8-bit surface into P010; rows shorter than the plane
//   sps       oneios_hevc_sps_crop on HEVC SPSs: a made-up one built field by field here (no window; a window of its
//             own; sub-layers; emulation prevention bytes in and out), read back field by field
//   stream    with a FILE: the HEVC stream FILE (Annex B, as an encoder wrote it at WxH) with every SPS cropped to
//             CWxCH, written to OUT (`test_picture stream FILE CW CH OUT`)
// No Apple file, no file of another project.
#include "../OneiOSCodecPicture.h"
#include <stdio.h>
#include <stdlib.h>

static int checks, failures;
#define CHECK(cond, ...) do { ++checks; if (!(cond)) { ++failures; fprintf(stderr, "FAIL %s:%d: ", __FILE__, __LINE__); \
                              fprintf(stderr, __VA_ARGS__); fputc('\n', stderr); } } while (0)

// A surface of W x H with rows of `stride` bytes: luma(x, y) = x + 3y, chroma pair (x, y) = (100 + x + y, 200 + x - y).
static uint8_t *surface(uint32_t w, uint32_t h, uint32_t stride, struct oneios_nv12_plane plane[2]) {
    uint8_t *s = calloc(1, (size_t)stride * h * 3 / 2 + 64);
    plane[0] = (struct oneios_nv12_plane){0, w, h, stride};
    plane[1] = (struct oneios_nv12_plane){(uint64_t)stride * h, w / 2, h / 2, stride};
    for (uint32_t y = 0; y < h; ++y) for (uint32_t x = 0; x < w; ++x) s[y * stride + x] = (uint8_t)(x + 3 * y);
    for (uint32_t y = 0; y < h / 2; ++y) for (uint32_t x = 0; x < w / 2; ++x) {
        s[stride * h + y * stride + 2 * x] = (uint8_t)(100 + x + y);
        s[stride * h + y * stride + 2 * x + 1] = (uint8_t)(200 + x - y);
    }
    return s;
}

// What `to` must hold: the part at (ox, oy) of aw x ah, cut by the surface, then the last column and row repeated.
static int same_as_expected(const uint8_t *to, uint32_t w, uint32_t h, uint32_t sw, uint32_t sh, uint32_t ox, uint32_t oy,
                            uint32_t aw, uint32_t ah) {
    uint32_t lw = sw - ox < aw ? sw - ox : aw, lh = sh - oy < ah ? sh - oy : ah;
    if (lw > w) lw = w;
    for (uint32_t y = 0; y < h; ++y) for (uint32_t x = 0; x < w; ++x) {
        const uint32_t sx = ox + (x < lw ? x : lw - 1), sy = oy + (y < lh ? y : lh - 1);
        if (to[y * w + x] != (uint8_t)(sx + 3 * sy)) return 0;
    }
    const uint32_t cw = (lw + 1) / 2, ch = (lh + 1) / 2;
    for (uint32_t y = 0; y < h / 2; ++y) for (uint32_t x = 0; x < w / 2; ++x) {
        const uint32_t sx = ox / 2 + (x < cw ? x : cw - 1), sy = oy / 2 + (y < ch ? y : ch - 1);
        if (to[w * h + y * w + 2 * x] != (uint8_t)(100 + sx + sy) || to[w * h + y * w + 2 * x + 1] != (uint8_t)(200 + sx - sy)) return 0;
    }
    return 1;
}

static void test_copy(void) {
    struct oneios_nv12_plane plane[2];
    uint8_t *s = surface(200, 120, 256, plane);
    uint8_t *to = malloc(96 * 96 * 3 / 2);
    // a tile inside the surface, coded at its own size
    CHECK(oneios_nv12_copy_area(to, 84, 84, s, plane, 84, 0, 84, 84) && same_as_expected(to, 84, 84, 200, 120, 84, 0, 84, 84), "a tile inside");
    // the same tile coded at 96x96 (a picture below the encoders' smallest): the last column and row repeated
    CHECK(oneios_nv12_copy_area(to, 96, 96, s, plane, 84, 0, 84, 84) && same_as_expected(to, 96, 96, 200, 120, 84, 0, 84, 84), "a tile coded larger");
    // a tile at the right and bottom edges: the surface ends first (200 - 168 = 32 columns, 120 - 84 = 36 rows)
    CHECK(oneios_nv12_copy_area(to, 84, 84, s, plane, 168, 84, 84, 84) && same_as_expected(to, 84, 84, 200, 120, 168, 84, 84, 84), "a tile at the edge");
    // a smaller part than the picture (a tile cut by the image), and a whole frame (origin 0, the surface's size)
    CHECK(oneios_nv12_copy_area(to, 84, 84, s, plane, 0, 84, 40, 20) && same_as_expected(to, 84, 84, 200, 120, 0, 84, 40, 20), "a cut tile");
    CHECK(oneios_nv12_copy_area(to, 96, 96, s, plane, 0, 0, 200, 120) && same_as_expected(to, 96, 96, 200, 120, 0, 0, 200, 120), "a frame");
    // parts that do not start inside the surface: nothing is copied
    CHECK(!oneios_nv12_copy_area(to, 84, 84, s, plane, 200, 0, 84, 84), "origin x past the surface");
    CHECK(!oneios_nv12_copy_area(to, 84, 84, s, plane, 0, 120, 84, 84), "origin y past the surface");
    CHECK(!oneios_nv12_copy_area(to, 84, 84, s, plane, 0, 0, 0, 84), "an empty part");
    free(s);
    free(to);
}


// --- 10-bit surfaces (x420, xf20: 16-bit samples, the value in the high 10 bits) and P010 pictures (2026-11-23) -------
// A 10-bit value at (x, y) of plane p (0 luma; 1 U, 2 V of chroma pair x), in 0..1023 and distinct in its low bits.
static uint32_t ten(uint32_t p, uint32_t x, uint32_t y) { return (p * 300 + x * 5 + y * 7) % 1024; }

// A surface of W x H, 16-bit samples, rows of `stride` bytes (at least 2 W).
static uint8_t *surface16(uint32_t w, uint32_t h, uint32_t stride, struct oneios_nv12_plane plane[2]) {
    uint8_t *s = calloc(1, (size_t)stride * h * 3 / 2 + 64);
    plane[0] = (struct oneios_nv12_plane){0, w, h, stride};
    plane[1] = (struct oneios_nv12_plane){(uint64_t)stride * h, w / 2, h / 2, stride};
    for (uint32_t y = 0; y < h; ++y) for (uint32_t x = 0; x < w; ++x) {
        const uint32_t v = ten(0, x, y) << 6;
        s[y * stride + 2 * x] = (uint8_t)v; s[y * stride + 2 * x + 1] = (uint8_t)(v >> 8);
    }
    for (uint32_t y = 0; y < h / 2; ++y) for (uint32_t x = 0; x < w / 2; ++x) for (uint32_t c = 0; c < 2; ++c) {
        const uint32_t v = ten(1 + c, x, y) << 6;
        uint8_t *at = s + (size_t)stride * h + (size_t)y * stride + 4 * x + 2 * c;
        at[0] = (uint8_t)v; at[1] = (uint8_t)(v >> 8);
    }
    return s;
}

// A sample of the picture `to` (`bytes` a sample, w x h): plane 0 luma at (x, y), 1 and 2 chroma pair x of row y.
static uint32_t sample(const uint8_t *to, uint32_t bytes, uint32_t w, uint32_t h, uint32_t p, uint32_t x, uint32_t y) {
    const uint64_t i = p == 0 ? (uint64_t)y * w + x : (uint64_t)w * h + (uint64_t)y * w + 2 * x + (p - 1);
    return bytes == 2 ? (uint32_t)to[2 * i] | (uint32_t)to[2 * i + 1] << 8 : to[i];
}

// What `to` must hold from surface16 (as same_as_expected): each sample the 10-bit value in the high bits of 16 (P010),
// or its high byte (NV12).
static int same_as_expected16(const uint8_t *to, uint32_t bytes, uint32_t w, uint32_t h, uint32_t sw, uint32_t sh, uint32_t ox,
                              uint32_t oy, uint32_t aw, uint32_t ah) {
    uint32_t lw = sw - ox < aw ? sw - ox : aw, lh = sh - oy < ah ? sh - oy : ah;
    if (lw > w) lw = w;
    const uint32_t cw = (lw + 1) / 2, ch = (lh + 1) / 2;
    for (uint32_t p = 0; p < 3; ++p)
        for (uint32_t y = 0; y < (p ? h / 2 : h); ++y) for (uint32_t x = 0; x < (p ? w / 2 : w); ++x) {
            const uint32_t sx = p ? ox / 2 + (x < cw ? x : cw - 1) : ox + (x < lw ? x : lw - 1);
            const uint32_t sy = p ? oy / 2 + (y < ch ? y : ch - 1) : oy + (y < lh ? y : lh - 1);
            const uint32_t want = bytes == 2 ? ten(p, sx, sy) << 6 : ten(p, sx, sy) >> 2;
            if (sample(to, bytes, w, h, p, x, y) != want) return 0;
        }
    return 1;
}

static void test_copy16(void) {
    struct oneios_nv12_plane plane[2];
    uint8_t *s = surface16(200, 120, 448, plane);
    uint8_t *to = malloc(96 * 96 * 3);
    // 10-bit into P010: the samples as they are; inside, coded larger (96x96), at the edges, cut, a whole frame
    CHECK(oneios_yuv_copy_area(to, 2, 84, 84, s, 2, plane, 84, 0, 84, 84) && same_as_expected16(to, 2, 84, 84, 200, 120, 84, 0, 84, 84), "P010: a tile inside");
    CHECK(oneios_yuv_copy_area(to, 2, 96, 96, s, 2, plane, 84, 0, 84, 84) && same_as_expected16(to, 2, 96, 96, 200, 120, 84, 0, 84, 84), "P010: coded larger");
    CHECK(oneios_yuv_copy_area(to, 2, 84, 84, s, 2, plane, 168, 84, 84, 84) && same_as_expected16(to, 2, 84, 84, 200, 120, 168, 84, 84, 84), "P010: at the edge");
    CHECK(oneios_yuv_copy_area(to, 2, 84, 84, s, 2, plane, 0, 84, 40, 20) && same_as_expected16(to, 2, 84, 84, 200, 120, 0, 84, 40, 20), "P010: a cut tile");
    CHECK(oneios_yuv_copy_area(to, 2, 96, 96, s, 2, plane, 0, 0, 200, 120) && same_as_expected16(to, 2, 96, 96, 200, 120, 0, 0, 200, 120), "P010: a frame");
    // 10-bit into NV12 (the encoder has no Main10): the high byte of each sample
    CHECK(oneios_yuv_copy_area(to, 1, 96, 96, s, 2, plane, 84, 0, 84, 84) && same_as_expected16(to, 1, 96, 96, 200, 120, 84, 0, 84, 84), "10-bit to NV12: coded larger");
    CHECK(oneios_yuv_copy_area(to, 1, 84, 84, s, 2, plane, 168, 84, 84, 84) && same_as_expected16(to, 1, 84, 84, 200, 120, 168, 84, 84, 84), "10-bit to NV12: at the edge");
    // parts that do not start inside, a sample size that is neither, a row shorter than the plane says: cut at the row
    CHECK(!oneios_yuv_copy_area(to, 2, 84, 84, s, 2, plane, 200, 0, 84, 84), "P010: origin x past the surface");
    CHECK(!oneios_yuv_copy_area(to, 3, 84, 84, s, 2, plane, 0, 0, 84, 84) && !oneios_yuv_copy_area(to, 2, 84, 84, s, 4, plane, 0, 0, 84, 84), "sample sizes");
    struct oneios_nv12_plane short_rows[2] = {plane[0], plane[1]};
    short_rows[0].bytes_per_row = short_rows[1].bytes_per_row = 448;   // 224 samples: the 200 fit, so the same
    short_rows[0].width = 240; short_rows[1].width = 120;               // a plane that claims more than its rows hold
    CHECK(oneios_yuv_copy_area(to, 2, 84, 84, s, 2, short_rows, 168, 0, 84, 84), "P010: rows shorter than the width");
    CHECK(sample(to, 2, 84, 84, 0, 83, 0) == sample(to, 2, 84, 84, 0, 224 - 168 - 1, 0), "P010: cut at the row's end, then repeated");
    free(s);
    // 8-bit into P010 (a 420v tile of a session that codes P010): v * 257 in the high bits
    s = surface(200, 120, 256, plane);
    CHECK(oneios_yuv_copy_area(to, 2, 96, 96, s, 1, plane, 84, 0, 84, 84), "8-bit to P010");
    int good = 1;
    for (uint32_t y = 0; y < 96; ++y) for (uint32_t x = 0; x < 96; ++x) {
        const uint32_t sx = 84 + (x < 84 ? x : 83), sy = y < 84 ? y : 83, v = (uint8_t)(sx + 3 * sy);
        good &= sample(to, 2, 96, 96, 0, x, y) == ((v * 257) & 0xffc0);
    }
    CHECK(good, "8-bit to P010: v * 257, the low 6 bits clear");
    free(s);
    free(to);
}

// --- An SPS built here (H.265 7.3.2.2), with its own fields after the window, and read back ------------------------
struct fields {
    uint32_t sub_layers, width, height, window, left, right, top, bottom, tail;
};

static uint32_t emulate(const uint8_t *payload, uint32_t bits, uint8_t *nal) {
    uint32_t at = 0, zeros = 0;
    nal[at++] = 0x42; nal[at++] = 0x01;   // nal_unit_type 33
    for (uint32_t i = 0; i < (bits + 7) / 8; ++i) {
        if (zeros >= 2 && payload[i] <= 3) { nal[at++] = 3; zeros = 0; }
        nal[at++] = payload[i];
        zeros = payload[i] ? 0 : zeros + 1;
    }
    return at;
}

static uint32_t build_sps(const struct fields *f, uint8_t *nal) {
    uint8_t payload[256];
    struct oneios_bits w = {payload, sizeof payload, 0, 0};
    oneios_bits_put(&w, 0, 4);                     // sps_video_parameter_set_id
    oneios_bits_put(&w, f->sub_layers, 3);
    oneios_bits_put(&w, 1, 1);
    // the general profile: Main (space 0, tier 0, idc 1), compatibility 0x60000000, the four flags 1 0 0 1, 43 zero
    // bits and a zero: zeros that the stream must carry with emulation prevention bytes; level 90 (3.0)
    oneios_bits_put(&w, 1, 8); oneios_bits_put(&w, 0x60000000u, 32); oneios_bits_put(&w, 9, 4);
    oneios_bits_put(&w, 0, 32); oneios_bits_put(&w, 0, 12); oneios_bits_put(&w, 90, 8);
    for (uint32_t i = 0; i < f->sub_layers; ++i) { oneios_bits_put(&w, 1, 1); oneios_bits_put(&w, i & 1, 1); }
    if (f->sub_layers) for (uint32_t i = f->sub_layers; i < 8; ++i) oneios_bits_put(&w, 0, 2);
    for (uint32_t i = 0; i < f->sub_layers; ++i) {
        oneios_bits_put(&w, 0x01600000u, 32); oneios_bits_put(&w, 0, 32); oneios_bits_put(&w, 0x5a, 24);
        if (i & 1) oneios_bits_put(&w, 60, 8);
    }
    oneios_bits_put_ue(&w, 0);                     // sps_seq_parameter_set_id
    oneios_bits_put_ue(&w, 1);                     // chroma_format_idc 4:2:0
    oneios_bits_put_ue(&w, f->width);
    oneios_bits_put_ue(&w, f->height);
    oneios_bits_put(&w, f->window, 1);
    if (f->window) { oneios_bits_put_ue(&w, f->left); oneios_bits_put_ue(&w, f->right); oneios_bits_put_ue(&w, f->top); oneios_bits_put_ue(&w, f->bottom); }
    oneios_bits_put(&w, f->tail, 32);              // what follows the window, as it is (bit_depth and the rest, made up)
    oneios_bits_put(&w, 0, 17);
    oneios_bits_put(&w, 1, 1);                     // rbsp_stop_one_bit
    while (w.at & 7) oneios_bits_put(&w, 0, 1);
    return emulate(payload, w.at, nal);
}

static int read_sps(const uint8_t *nal, uint32_t n, struct fields *f) {
    uint8_t rbsp[512];
    uint32_t size = 0, zeros = 0;
    for (uint32_t i = 2; i < n; ++i) {
        if (zeros >= 2 && nal[i] == 3) { zeros = 0; continue; }
        if (zeros >= 2 && nal[i] < 3) return 0;     // a start code or a zero run the stream may not carry
        zeros = nal[i] ? 0 : zeros + 1;
        rbsp[size++] = nal[i];
    }
    struct oneios_bits r = {rbsp, size * 8, 0, 0};
    oneios_bits_u(&r, 4);
    f->sub_layers = oneios_bits_u(&r, 3);
    oneios_bits_u(&r, 1);
    oneios_bits_u(&r, 32); oneios_bits_u(&r, 32); oneios_bits_u(&r, 32);
    uint32_t profile[8] = {0}, level[8] = {0};
    for (uint32_t i = 0; i < f->sub_layers; ++i) { profile[i] = oneios_bits_u(&r, 1); level[i] = oneios_bits_u(&r, 1); }
    if (f->sub_layers) for (uint32_t i = f->sub_layers; i < 8; ++i) oneios_bits_u(&r, 2);
    for (uint32_t i = 0; i < f->sub_layers; ++i) {
        if (profile[i]) { oneios_bits_u(&r, 32); oneios_bits_u(&r, 32); oneios_bits_u(&r, 24); }
        if (level[i]) oneios_bits_u(&r, 8);
    }
    oneios_bits_ue(&r);
    if (oneios_bits_ue(&r) != 1) return 0;
    f->width = oneios_bits_ue(&r);
    f->height = oneios_bits_ue(&r);
    f->window = oneios_bits_u(&r, 1);
    f->left = f->right = f->top = f->bottom = 0;
    if (f->window) { f->left = oneios_bits_ue(&r); f->right = oneios_bits_ue(&r); f->top = oneios_bits_ue(&r); f->bottom = oneios_bits_ue(&r); }
    f->tail = oneios_bits_u(&r, 32);
    if (oneios_bits_u(&r, 17) != 0 || oneios_bits_u(&r, 1) != 1) return 0;   // the rest, then the stop bit
    while (r.at & 7) if (oneios_bits_u(&r, 1)) return 0;
    return !r.bad && r.at == r.size;
}

static void test_sps(void) {
    uint8_t in[256], out[256];
    struct fields f;
    // a codec's 96x96 with no window, cropped to 84x84: the window 0 6 0 6, the rest as it was
    const struct fields plain = {0, 96, 96, 0, 0, 0, 0, 0, 0xa5000001u};
    uint32_t n = build_sps(&plain, in), m = oneios_hevc_sps_crop(in, n, 84, 84, out, sizeof out);
    CHECK(m && read_sps(out, m, &f) && f.width == 96 && f.height == 96 && f.window && f.left == 0 && f.right == 6 && f.top == 0 &&
          f.bottom == 6 && f.tail == plain.tail, "96x96 to 84x84 (%u bytes)", m);
    CHECK(n > 8 && in[2] == 0x01 && in[6] == 0x00 && in[7] == 0x03, "the made-up SPS carries emulation prevention bytes");
    // a codec's own window (its picture padded to 112x96 and cropped to 100x90 with left 2, top 1) cropped to 84x84:
    // the codec's top-left offsets stay, right and bottom take the rest
    const struct fields own = {0, 112, 96, 1, 2, 6, 1, 2, 0x0000ffffu};
    n = build_sps(&own, in);
    m = oneios_hevc_sps_crop(in, n, 84, 84, out, sizeof out);
    CHECK(m && read_sps(out, m, &f) && f.window && f.left == 2 && f.right == (112 - 4 - 84) / 2 && f.top == 1 &&
          f.bottom == (96 - 2 - 84) / 2 && f.tail == own.tail, "a window of the codec's own");
    // sub-layers (their profile and level fields are skipped as 7.3.3 lays them out)
    const struct fields layers = {3, 128, 96, 0, 0, 0, 0, 0, 0x12345678u};
    n = build_sps(&layers, in);
    m = oneios_hevc_sps_crop(in, n, 120, 90, out, sizeof out);
    CHECK(m && read_sps(out, m, &f) && f.sub_layers == 3 && f.right == 4 && f.bottom == 3 && f.tail == layers.tail, "sub-layers");
    // no crop at all (the session's size): the window is written with zeros, the picture the same
    n = build_sps(&plain, in);
    m = oneios_hevc_sps_crop(in, n, 96, 96, out, sizeof out);
    CHECK(m && read_sps(out, m, &f) && f.window && !f.right && !f.bottom && f.tail == plain.tail, "the same size");
    // refused: a crop larger than the picture, an odd size, a cut SPS, no room for the answer
    CHECK(!oneios_hevc_sps_crop(in, n, 98, 84, out, sizeof out), "a crop larger than the picture");
    CHECK(!oneios_hevc_sps_crop(in, n, 83, 84, out, sizeof out), "an odd width");
    CHECK(!oneios_hevc_sps_crop(in, 12, 84, 84, out, sizeof out), "a cut SPS");
    CHECK(!oneios_hevc_sps_crop(in, n, 84, 84, out, 8), "no room");
}

// `stream FILE CW CH OUT`: FILE's SPSs cropped, every other NAL unit as it is.
static int crop_stream(const char *path, uint32_t width, uint32_t height, const char *to) {
    FILE *in = fopen(path, "rb"), *out = fopen(to, "wb");
    if (!in || !out) { perror(path); return 2; }
    static uint8_t data[1 << 22];
    const size_t n = fread(data, 1, sizeof data, in);
    fclose(in);
    size_t at = 0;
    uint32_t cropped = 0;
    while (at + 3 < n) {
        size_t start = at;
        while (start + 3 <= n && !(data[start] == 0 && data[start + 1] == 0 && data[start + 2] == 1)) ++start;
        if (start + 3 > n) break;
        const size_t nal = start + 3;
        size_t next = nal;
        while (next + 3 <= n && !(data[next] == 0 && data[next + 1] == 0 && (data[next + 2] == 1 || (data[next + 2] == 0 && next + 3 < n && data[next + 3] == 1)))) ++next;
        if (next + 3 > n) next = n;
        static const uint8_t code[4] = {0, 0, 0, 1};
        fwrite(code, 1, 4, out);
        if (((data[nal] >> 1) & 0x3f) == 33) {
            uint8_t sps[256];
            const uint32_t m = oneios_hevc_sps_crop(data + nal, (uint32_t)(next - nal), width, height, sps, sizeof sps);
            if (!m) { fprintf(stderr, "stream: an SPS of %zu bytes could not be cropped\n", next - nal); return 1; }
            fwrite(sps, 1, m, out);
            ++cropped;
        } else {
            fwrite(data + nal, 1, next - nal, out);
        }
        at = next;
    }
    fclose(out);
    printf("stream: %u SPS(s) cropped to %ux%u\n", cropped, width, height);
    return cropped ? 0 : 1;
}

int main(int argc, char **argv) {
    if (argc == 6 && !strcmp(argv[1], "stream")) return crop_stream(argv[2], (uint32_t)atoi(argv[3]), (uint32_t)atoi(argv[4]), argv[5]);
    test_copy();
    test_copy16();
    test_sps();
    printf("test_picture: %d checks, %d failed\n", checks, failures);
    return failures ? 1 : 0;
}
