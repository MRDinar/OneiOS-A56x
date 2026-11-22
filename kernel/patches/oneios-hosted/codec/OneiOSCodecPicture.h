// SPDX-License-Identifier: BSD-2-Clause-Patent
//
// OneiOS's two picture helpers of the staged DroidVMVTHost.cpp (tools/oneios-tool/crates/oneios-kernel/src/staged/
// codec.rs), in plain C so that the host test (tests/test_picture.c, run by kernel/tests/host.sh) compiles the same
// code the kernel does:
//   oneios_nv12_copy_area   a part of an NV12 surface (a tile's origin and size) into a tightly packed NV12 picture of
//                           the size the device codes; what lies past the part, or past the surface, repeats the last
//                           column and row copied
//   oneios_hevc_sps_crop    an HEVC SPS with the conformance window that crops its coded picture to a smaller size:
//                           the phone's c2.qti encoders code nothing below 96x96, and chronod's widget tiles are 84x84
// Both read only the bytes they are given and write only within the capacity they are given.
#pragma once
#include <stdint.h>
#include <string.h>

struct oneios_nv12_plane {
    uint64_t offset, width, height, bytes_per_row;   // plane 1's width and height count chroma pairs
};

// The part (origin_x, origin_y: even; area_width x area_height) of an NV12 surface into `to`, w x h luma then w x h/2
// interleaved chroma (w even). 0 when the part does not start inside the surface.
static inline int oneios_nv12_copy_area(uint8_t *to, uint32_t w, uint32_t h, const uint8_t *surface,
                                        const struct oneios_nv12_plane plane[2], uint32_t origin_x, uint32_t origin_y,
                                        uint32_t area_width, uint32_t area_height) {
    if (origin_x >= plane[0].width || origin_y >= plane[0].height || origin_y / 2 >= plane[1].height || w < 2 || h < 2) return 0;
    uint64_t luma_width = plane[0].width - origin_x, luma_height = plane[0].height - origin_y;
    if (luma_width > area_width) luma_width = area_width;
    if (luma_height > area_height) luma_height = area_height;
    if (luma_width > w) luma_width = w;
    const uint64_t chroma_row = plane[1].width * 2 < plane[1].bytes_per_row ? plane[1].width * 2 : plane[1].bytes_per_row;
    if (chroma_row <= origin_x) return 0;
    uint64_t chroma_width = (chroma_row - origin_x) & ~(uint64_t)1, chroma_height = plane[1].height - origin_y / 2;
    if (chroma_width > ((luma_width + 1) & ~(uint64_t)1)) chroma_width = (luma_width + 1) & ~(uint64_t)1;
    if (chroma_width > (w & ~1u)) chroma_width = w & ~1u;
    if (chroma_height > (luma_height + 1) / 2) chroma_height = (luma_height + 1) / 2;
    if (!luma_width || !luma_height || !chroma_width || !chroma_height) return 0;
    for (uint32_t y = 0; y < h; ++y) {
        const uint64_t from = origin_y + (y < luma_height ? y : luma_height - 1);
        const uint8_t *row = surface + plane[0].offset + from * plane[0].bytes_per_row + origin_x;
        uint8_t *line = to + (uint64_t)y * w;
        memcpy(line, row, (size_t)luma_width);
        if (luma_width < w) memset(line + luma_width, row[luma_width - 1], (size_t)(w - luma_width));
    }
    uint8_t *chroma = to + (uint64_t)w * h;
    for (uint32_t y = 0; y < h / 2; ++y) {
        const uint64_t from = origin_y / 2 + (y < chroma_height ? y : chroma_height - 1);
        const uint8_t *row = surface + plane[1].offset + from * plane[1].bytes_per_row + origin_x;
        uint8_t *line = chroma + (uint64_t)y * w;
        memcpy(line, row, (size_t)chroma_width);
        for (uint64_t x = chroma_width; x + 1 < w; x += 2) { line[x] = row[chroma_width - 2]; line[x + 1] = row[chroma_width - 1]; }
    }
    return 1;
}

// The bits of an RBSP, most significant first; ue(v) as H.265 7.2.
struct oneios_bits {
    uint8_t *bytes;
    uint32_t size, at;   // in bits for a reader, the capacity in bytes for a writer
    int bad;
};

static inline uint32_t oneios_bits_u(struct oneios_bits *r, unsigned n) {
    uint32_t v = 0;
    while (n--) {
        if (r->at >= r->size) { r->bad = 1; return 0; }
        v = v << 1 | ((r->bytes[r->at >> 3] >> (7 - (r->at & 7))) & 1);
        ++r->at;
    }
    return v;
}

static inline uint32_t oneios_bits_ue(struct oneios_bits *r) {
    unsigned zeros = 0;
    while (!r->bad && !oneios_bits_u(r, 1))
        if (++zeros > 31) { r->bad = 1; return 0; }
    return zeros ? (uint32_t)((1ull << zeros) - 1 + oneios_bits_u(r, zeros)) : 0;
}

static inline void oneios_bits_put(struct oneios_bits *w, uint64_t v, unsigned n) {
    while (n--) {
        if ((w->at >> 3) >= w->size) { w->bad = 1; return; }
        if (!(w->at & 7)) w->bytes[w->at >> 3] = 0;
        w->bytes[w->at >> 3] |= (uint8_t)(((v >> n) & 1) << (7 - (w->at & 7)));
        ++w->at;
    }
}

static inline void oneios_bits_put_ue(struct oneios_bits *w, uint32_t v) {
    const uint64_t x = (uint64_t)v + 1;
    unsigned length = 0;
    while ((x >> length) > 1) ++length;
    oneios_bits_put(w, 0, length);
    oneios_bits_put(w, x, length + 1);
}

static inline void oneios_bits_copy(struct oneios_bits *r, struct oneios_bits *w, unsigned n) {
    oneios_bits_put(w, oneios_bits_u(r, n), n);
}

// An HEVC SPS NAL unit (its two-byte header, the payload with emulation prevention bytes, as a codec writes it) of a
// picture coded larger than width x height, into `out` with the conformance window that crops it to width x height from
// the codec's own top-left offsets: its size, or 0 when the SPS cannot be read (not 4:2:0, a picture smaller than the
// crop, a malformed field) or `out` is too small. Read as H.265 7.3.2.2 up to conformance_window_flag (profile_tier_level
// 7.3.3 with its sub-layers); the bits after the window are copied as they are, then the stop bit and the alignment.
static inline uint32_t oneios_hevc_sps_crop(const uint8_t *in, uint32_t n, uint32_t width, uint32_t height, uint8_t *out,
                                            uint32_t capacity) {
    uint8_t rbsp[512], payload[600];
    if (n < 4 || n > sizeof rbsp || capacity < 3) return 0;
    uint32_t size = 0, zeros = 0;
    for (uint32_t i = 2; i < n; ++i) {
        const uint8_t byte = in[i];
        if (zeros >= 2 && byte == 3) { zeros = 0; continue; }
        zeros = byte ? 0 : zeros + 1;
        rbsp[size++] = byte;
    }
    while (size && !rbsp[size - 1]) --size;
    if (!size) return 0;
    unsigned low = 0;
    while (!((rbsp[size - 1] >> low) & 1)) ++low;
    struct oneios_bits r = {rbsp, (size - 1) * 8 + 7 - low, 0, 0};   // the bits before rbsp_stop_one_bit
    struct oneios_bits w = {payload, sizeof payload, 0, 0};
    oneios_bits_copy(&r, &w, 4);                                     // sps_video_parameter_set_id
    const uint32_t sub_layers = oneios_bits_u(&r, 3);                // sps_max_sub_layers_minus1
    oneios_bits_put(&w, sub_layers, 3);
    oneios_bits_copy(&r, &w, 1);                                     // sps_temporal_id_nesting_flag
    oneios_bits_copy(&r, &w, 32);                                    // the general profile (88 bits), general_level_idc
    oneios_bits_copy(&r, &w, 32);
    oneios_bits_copy(&r, &w, 32);
    uint32_t profile[8] = {0}, level[8] = {0};
    for (uint32_t i = 0; i < sub_layers; ++i) {
        profile[i] = oneios_bits_u(&r, 1); oneios_bits_put(&w, profile[i], 1);
        level[i] = oneios_bits_u(&r, 1); oneios_bits_put(&w, level[i], 1);
    }
    if (sub_layers) for (uint32_t i = sub_layers; i < 8; ++i) oneios_bits_copy(&r, &w, 2);
    for (uint32_t i = 0; i < sub_layers; ++i) {
        if (profile[i]) { oneios_bits_copy(&r, &w, 32); oneios_bits_copy(&r, &w, 32); oneios_bits_copy(&r, &w, 24); }
        if (level[i]) oneios_bits_copy(&r, &w, 8);
    }
    oneios_bits_put_ue(&w, oneios_bits_ue(&r));                     // sps_seq_parameter_set_id
    const uint32_t chroma = oneios_bits_ue(&r);
    oneios_bits_put_ue(&w, chroma);
    if (chroma == 3) oneios_bits_copy(&r, &w, 1);
    const uint32_t coded_width = oneios_bits_ue(&r), coded_height = oneios_bits_ue(&r);
    oneios_bits_put_ue(&w, coded_width);
    oneios_bits_put_ue(&w, coded_height);
    uint32_t left = 0, top = 0;
    if (oneios_bits_u(&r, 1)) {
        left = oneios_bits_ue(&r); oneios_bits_ue(&r);
        top = oneios_bits_ue(&r); oneios_bits_ue(&r);
    }
    // 4:2:0 only: the window counts chroma samples, two luma each.
    if (r.bad || chroma != 1 || (width & 1) || (height & 1) || (uint64_t)coded_width < 2ull * left + width ||
        (uint64_t)coded_height < 2ull * top + height) return 0;
    oneios_bits_put(&w, 1, 1);
    oneios_bits_put_ue(&w, left);
    oneios_bits_put_ue(&w, (coded_width - 2 * left - width) / 2);
    oneios_bits_put_ue(&w, top);
    oneios_bits_put_ue(&w, (coded_height - 2 * top - height) / 2);
    while (r.at < r.size) { const unsigned k = r.size - r.at < 32 ? r.size - r.at : 32; oneios_bits_copy(&r, &w, k); }
    oneios_bits_put(&w, 1, 1);
    while (w.at & 7) oneios_bits_put(&w, 0, 1);
    if (r.bad || w.bad) return 0;
    uint32_t at = 0;
    out[at++] = in[0];
    out[at++] = in[1];
    zeros = 0;
    for (uint32_t i = 0; i < w.at / 8; ++i) {
        if (at + 2 > capacity) return 0;
        const uint8_t byte = payload[i];
        if (zeros >= 2 && byte <= 3) { out[at++] = 3; zeros = 0; }
        out[at++] = byte;
        zeros = byte ? 0 : zeros + 1;
    }
    return at;
}
