// SPDX-License-Identifier: MIT
// Host tests of the display helper's core: the display.sock protocol against a serving thread
// (what xnuhost does), the letterbox fit, the damage rectangles, the row copy; and displayctl
// against a stand-in helper. No phone, no Apple files.
//   test_display [path to a host build of displayctl]
#define _GNU_SOURCE
#include "xhd.h"
#include <errno.h>
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/wait.h>
#include <unistd.h>

static int checks, failed;
static int as_helper(const char *socket_path);
#define CHECK(c) do { checks++; if (!(c)) { failed++; printf("FAIL %s:%d: %s\n", __FILE__, __LINE__, #c); } } while (0)

static void test_fit(void)
{
  struct xhd_fit f;
  xhd_fit(1440, 3120, 1440, 3120, &f);                 // the guest at the panel's native mode: 1:1, no bars
  CHECK(f.w == 1440 && f.h == 3120 && f.x == 0 && f.y == 0);
  xhd_fit(1440, 3120, 1080, 2340, &f);                 // the panel left at FHD+ (the native mode was refused): scaled, same shape
  CHECK(f.w == 1080 && f.h == 2340 && f.x == 0 && f.y == 0);
  xhd_fit(1320, 2868, 1440, 3120, &f);                 // a mismatch: the iPhone's own frame on the S24 Ultra's panel
  CHECK(f.w == 1436 && f.h == 3120 && f.x == 2 && f.y == 0);
  xhd_fit(1320, 2868, 1080, 2340, &f);                 // One UI at FHD+
  CHECK(f.w == 1077 && f.h == 2340 && f.x == 1 && f.y == 0);
  xhd_fit(1440, 3120, 1440, 3120, &f);
  CHECK(f.w == 1440 && f.h == 3120 && f.x == 0 && f.y == 0);
  xhd_fit(720, 1560, 1440, 3120, &f);                  // same aspect, scaled up
  CHECK(f.w == 1440 && f.h == 3120 && f.x == 0 && f.y == 0);
  xhd_fit(1920, 1080, 1440, 3120, &f);                 // wide in tall: bars above and below
  CHECK(f.w == 1440 && f.h == 810 && f.x == 0 && f.y == 1155);
  xhd_fit(1320, 2868, 3120, 1440, &f);                 // tall in wide: bars left and right
  CHECK(f.h == 1440 && f.w == 663 && f.x == 1228 && f.y == 0);
  xhd_fit(1, 10000, 100, 100, &f);                     // never thinner than a pixel
  CHECK(f.w == 1 && f.h == 100);
  xhd_fit(16384, 16384, 16384, 16384, &f);             // no overflow at the largest sizes
  CHECK(f.w == 16384 && f.h == 16384);
  xhd_fit(0, 10, 100, 100, &f);
  CHECK(f.w == 0 && f.h == 0);
  for (uint32_t sw = 1; sw < 400; sw += 7) for (uint32_t sh = 1; sh < 400; sh += 11) {
    xhd_fit(sw, sh, 144, 312, &f);
    if (!(f.w >= 1 && f.h >= 1 && f.x + f.w <= 144 && f.y + f.h <= 312 && (f.w == 144 || f.h == 312))) { CHECK(!"fit inside the panel, touching two edges"); return; }
  }
  CHECK(1);
}

static void test_rects(void)
{
  struct xhd_rect r = { 10, 20, 30, 40 };
  CHECK(xhd_rect_clip(&r, 100, 100) && r.x == 10 && r.y == 20 && r.w == 30 && r.h == 40);
  r = (struct xhd_rect){ 90, 95, 30, 40 };
  CHECK(xhd_rect_clip(&r, 100, 100) && r.w == 10 && r.h == 5);
  r = (struct xhd_rect){ 100, 0, 1, 1 };
  CHECK(!xhd_rect_clip(&r, 100, 100) && r.w == 0);
  r = (struct xhd_rect){ 5, 5, 0xffffffffu, 0xffffffffu };              // a width that would wrap
  CHECK(xhd_rect_clip(&r, 100, 100) && r.w == 95 && r.h == 95);
  r = (struct xhd_rect){ 0xfffffff0u, 0, 0x20, 1 };
  CHECK(!xhd_rect_clip(&r, 100, 100));
  r = (struct xhd_rect){ 0, 0, 0, 5 };
  CHECK(!xhd_rect_clip(&r, 100, 100));
  struct xhd_rect u = { 0, 0, 0, 0 }, a = { 10, 10, 5, 5 }, b = { 50, 2, 10, 10 }, none = { 7, 7, 0, 0 };
  xhd_rect_union(&u, &a);
  CHECK(u.x == 10 && u.y == 10 && u.w == 5 && u.h == 5);
  xhd_rect_union(&u, &b);
  CHECK(u.x == 10 && u.y == 2 && u.w == 50 && u.h == 13);
  xhd_rect_union(&u, &none);
  CHECK(u.x == 10 && u.y == 2 && u.w == 50 && u.h == 13);
}

static void test_hello_check(void)
{
  struct xhd_message m = { { 'X', 'H', 'D', '1' }, 1320, 2868, 5280, 1 };
  off_t size = (off_t)5280 * 2868;
  CHECK(xhd_check_hello(&m, sizeof m, size) == 0);
  CHECK(xhd_check_hello(&m, sizeof m, size + 4096) == 0);
  CHECK(xhd_check_hello(&m, sizeof m, size - 1) == -EINVAL);            // a frame larger than its memfd
  CHECK(xhd_check_hello(&m, sizeof m - 4, size) == -EPROTO);
  struct xhd_message bad = m;
  bad.magic[3] = 'F';
  CHECK(xhd_check_hello(&bad, sizeof bad, size) == -EPROTO);
  bad = m; bad.a = 0;
  CHECK(xhd_check_hello(&bad, sizeof bad, size) == -EINVAL);
  bad = m; bad.b = 20000;
  CHECK(xhd_check_hello(&bad, sizeof bad, (off_t)1 << 40) == -EINVAL);
  bad = m; bad.c = 5276;                                                // a stride shorter than a row
  CHECK(xhd_check_hello(&bad, sizeof bad, size) == -EINVAL);
  bad = m; bad.c = 5281;
  CHECK(xhd_check_hello(&bad, sizeof bad, size * 2) == -EINVAL);
  bad = m; bad.d = 3;                                                   // a format this helper does not know
  CHECK(xhd_check_hello(&bad, sizeof bad, size) == -EINVAL);
  bad = m; bad.d = 0;
  CHECK(xhd_check_hello(&bad, sizeof bad, size) == -EINVAL);
  bad = m; bad.a = 0x40000001u;                                         // width * 4 wraps to 4
  CHECK(xhd_check_hello(&bad, sizeof bad, size) == -EINVAL);
  // Half floats: 8 bytes a pixel, the stride a whole number of them.
  struct xhd_message half = { { 'X', 'H', 'D', '1' }, 1440, 3120, 11520, XHD_FORMAT_RGBA16F };
  CHECK(xhd_check_hello(&half, sizeof half, (off_t)11520 * 3120) == 0);
  CHECK(xhd_check_hello(&half, sizeof half, (off_t)11520 * 3120 - 1) == -EINVAL);
  bad = half; bad.c = 5760;                                             // a B,G,R,X stride: a half-float row does not fit
  CHECK(xhd_check_hello(&bad, sizeof bad, (off_t)11520 * 3120) == -EINVAL);
  bad = half; bad.c = 11524;                                            // not a whole number of pixels
  CHECK(xhd_check_hello(&bad, sizeof bad, (off_t)11524 * 3120) == -EINVAL);
}

// The frame formats' buffers and dataspaces, the names of --dataspace, and the half-float conversions the copy uses.
static void test_formats(void)
{
  int ds = -5;
  CHECK(xhd_buffer_for(XHD_FORMAT_BGRX, &ds) == XHD_BUFFER_BGRA_8888 && ds == XHD_DATASPACE_SRGB);
  CHECK(xhd_buffer_for(XHD_FORMAT_RGBA16F, &ds) == XHD_BUFFER_RGBA_FP16 && ds == XHD_DATASPACE_SCRGB_LINEAR);
  CHECK(xhd_buffer_for(7, &ds) == 0 && ds == XHD_DATASPACE_UNKNOWN);
  CHECK(xhd_dataspace_parse("auto") == -1 && xhd_dataspace_parse("p3") == XHD_DATASPACE_DISPLAY_P3 &&
        xhd_dataspace_parse("scrgb-linear") == XHD_DATASPACE_SCRGB_LINEAR && xhd_dataspace_parse("bt709") == -2);
  CHECK(!strcmp(xhd_dataspace_name(XHD_DATASPACE_SRGB), "srgb") && !strcmp(xhd_dataspace_name(1234), "other"));
  // Android's values, from its headers (data_space.h): standard << 16 | transfer << 22 | range << 27.
  CHECK(XHD_DATASPACE_SRGB == (1 << 16 | 2 << 22 | 1 << 27) && XHD_DATASPACE_DISPLAY_P3 == (10 << 16 | 2 << 22 | 1 << 27) &&
        XHD_DATASPACE_SCRGB == (1 << 16 | 2 << 22 | 3 << 27) && XHD_DATASPACE_SCRGB_LINEAR == (1 << 16 | 1 << 22 | 3 << 27));
  // Halves: exact values, rounding, the ends.
  CHECK(xh_half_from_float(1.0f) == 0x3c00 && xh_half_from_float(-2.0f) == 0xc000 && xh_half_from_float(0.5f) == 0x3800);
  CHECK(xh_half_from_float(65504.0f) == 0x7bff && xh_half_from_float(1e6f) == 0x7c00 && xh_half_from_float(0.0f) == 0);
  CHECK(xh_half_from_float(5.9604645e-8f) == 0x0001);                  // the smallest subnormal
  CHECK((xh_half_from_float(__builtin_nanf("")) & 0x7fff) > 0x7c00);   // a NaN stays one
  int round_trip = 1;
  for (unsigned h = 0; h < 0x7c00; h++) if (xh_half_from_float(xh_half_to_float((uint16_t)h)) != h) round_trip = 0;
  CHECK(round_trip);                                                    // every finite positive half
  CHECK(xh_half_to_float(0x3c00) == 1.0f && xh_half_to_float(0xbc00) == -1.0f && xh_half_to_float(0x0400) == 6.1035156e-5f);
  // sRGB: 8-bit -> linear -> 8-bit gives the byte back; out of range is clamped.
  const uint16_t *lin = xh_srgb8_to_half();
  const uint8_t *enc = xh_half_to_srgb8();
  int back = 1;
  for (unsigned i = 0; i < 256; i++) if (enc[lin[i]] != i) back = 0;
  CHECK(back);
  CHECK(lin[0] == 0 && lin[255] == 0x3c00 && enc[0xbc00] == 0 && enc[0x4000] == 255 && enc[0x7c00] == 255 && enc[0x7e00] == 0);
  CHECK(lin[128] == xh_half_from_float(0.21586050f));                  // sRGB's 128 is 21.6% of the light
}

// The copy between frame formats and buffer layouts.
static void test_copy_formats(void)
{
  enum { W = 3, H = 2 };
  uint16_t half[H][W][4], out16[H][W][4];
  const float values[W][3] = { { 1.0f, 0.0f, 0.0f }, { 1.2249f, -0.042f, -0.0197f }, { 2.0f, 0.5f, 0.2158605f } };
  for (int y = 0; y < H; y++) for (int x = 0; x < W; x++) {
    for (int c = 0; c < 3; c++) half[y][x][c] = xh_half_from_float(values[x][c]);
    half[y][x][3] = 0x3c00;
  }
  struct xhd_rect all = { 0, 0, W, H };
  memset(out16, 0, sizeof out16);
  CHECK(xhd_copy((uint8_t *)out16, W * 8, XHD_BUFFER_RGBA_FP16, (const uint8_t *)half, W * 8, XHD_FORMAT_RGBA16F, all) == 0);
  CHECK(!memcmp(out16, half, sizeof half));                             // half floats into RGBA_FP16: as they are
  uint8_t out8[H][W][4];
  CHECK(xhd_copy((uint8_t *)out8, W * 4, XHD_BUFFER_BGRA_8888, (const uint8_t *)half, W * 8, XHD_FORMAT_RGBA16F, all) == 0);
  // into 8-bit: encoded and clamped, so sRGB's red and P3's red (outside sRGB) become the same 255,0,0
  CHECK(out8[0][0][0] == 0 && out8[0][0][1] == 0 && out8[0][0][2] == 255 && out8[0][0][3] == 255);
  CHECK(out8[0][1][0] == 0 && out8[0][1][1] == 0 && out8[0][1][2] == 255);
  CHECK(out8[1][2][2] == 255 && out8[1][2][1] == 188 && out8[1][2][0] == 128);   // 2.0 clamps; 0.5 is sRGB 188; 0.2159 is 128
  CHECK(xhd_copy((uint8_t *)out8, W * 4, XHD_BUFFER_RGBX_8888, (const uint8_t *)half, W * 8, XHD_FORMAT_RGBA16F, all) == 0);
  CHECK(out8[1][2][0] == 255 && out8[1][2][1] == 188 && out8[1][2][2] == 128 && out8[1][2][3] == 255);
  // 8-bit into RGBA_FP16: decoded to linear
  uint8_t bgrx[1][2][4] = { { { 0, 128, 255, 7 }, { 255, 255, 255, 0 } } };
  uint16_t got[1][2][4];
  struct xhd_rect two = { 0, 0, 2, 1 };
  CHECK(xhd_copy((uint8_t *)got, 16, XHD_BUFFER_RGBA_FP16, (const uint8_t *)bgrx, 8, XHD_FORMAT_BGRX, two) == 0);
  CHECK(got[0][0][0] == 0x3c00 && got[0][0][1] == xh_half_from_float(0.21586050f) && got[0][0][2] == 0 && got[0][0][3] == 0x3c00);
  CHECK(got[0][1][0] == 0x3c00 && got[0][1][1] == 0x3c00 && got[0][1][2] == 0x3c00);
  CHECK(xhd_copy((uint8_t *)got, 16, 99, (const uint8_t *)bgrx, 8, XHD_FORMAT_BGRX, two) == -EINVAL);
  CHECK(xhd_copy((uint8_t *)got, 16, XHD_BUFFER_RGBA_FP16, (const uint8_t *)bgrx, 8, 9, two) == -EINVAL);
}

static void test_copy(void)
{
  enum { W = 7, H = 5, SS = 40, DS = 32 };                              // strides wider than the rows
  uint8_t src[SS * H], dst[DS * H], want[DS * H];
  for (size_t i = 0; i < sizeof src; i++) src[i] = (uint8_t)(i * 7 + 1);
  memset(dst, 0xee, sizeof dst);
  memset(want, 0xee, sizeof want);
  struct xhd_rect r = { 2, 1, 4, 3 };
  CHECK(xhd_copy(dst, DS, XHD_BUFFER_BGRA_8888, src, SS, XHD_FORMAT_BGRX, r) == 0);
  for (uint32_t y = 1; y < 4; y++) memcpy(want + y * DS + 8, src + y * SS + 8, 16);
  CHECK(!memcmp(dst, want, sizeof dst));                                // only the rectangle, bytes as they are
  CHECK(xhd_copy(dst, DS, XHD_BUFFER_RGBX_8888, src, SS, XHD_FORMAT_BGRX, r) == 0);
  int ok = 1;
  for (uint32_t y = 1; y < 4; y++) for (uint32_t x = 2; x < 6; x++) {
    const uint8_t *s = src + y * SS + x * 4, *d = dst + y * DS + x * 4;
    if (d[0] != s[2] || d[1] != s[1] || d[2] != s[0] || d[3] != 0xff) ok = 0;
  }
  CHECK(ok);                                                            // B and R swapped, alpha opaque
  CHECK(dst[0] == 0xee && dst[DS * 4] == 0xee && dst[DS + 7] == 0xee && dst[DS + 24] == 0xee);
}

// ---- direct frames: guest RAM and the table from xnuhost, the rows copied from scattered segments --------
static void send_fds(int fd, const struct xhd_message *m, const int *fds, int n)
{
  char control[CMSG_SPACE(sizeof(int) * 2)];
  struct iovec iov = { (void *)m, sizeof *m };
  struct msghdr msg = { .msg_iov = &iov, .msg_iovlen = 1, .msg_control = control, .msg_controllen = CMSG_SPACE(sizeof(int) * (size_t)n) };
  memset(control, 0, sizeof control);
  struct cmsghdr *cm = CMSG_FIRSTHDR(&msg);
  cm->cmsg_level = SOL_SOCKET; cm->cmsg_type = SCM_RIGHTS; cm->cmsg_len = CMSG_LEN(sizeof(int) * (size_t)n);
  memcpy(CMSG_DATA(cm), fds, sizeof(int) * (size_t)n);
  sendmsg(fd, &msg, 0);
}

static void test_direct(void)
{
  enum { RAM = 0x10000, SLOTS = 4, SLOT = 64 + 8 * 16, W = 10, H = 100, STRIDE = 48, DS = 44 };
  int ram = xhd_memfd("test-ram", RAM), tab = xhd_memfd("test-table", SLOTS * SLOT), pair[2];
  CHECK(ram >= 0 && tab >= 0 && socketpair(AF_UNIX, SOCK_SEQPACKET, 0, pair) == 0);
  uint8_t *rp = mmap(NULL, RAM, PROT_READ | PROT_WRITE, MAP_SHARED, ram, 0), *tp = mmap(NULL, SLOTS * SLOT, PROT_READ | PROT_WRITE, MAP_SHARED, tab, 0);
  for (size_t i = 0; i < RAM; i++) rp[i] = (uint8_t)(i * 13 + (i >> 8));
  // The scanout: 4800 bytes in two pages out of order, so row 85 runs from one into the other.
  struct xhd_slot *slot = (struct xhd_slot *)(tp + 2 * SLOT);
  struct xhd_seg *segs = (struct xhd_seg *)(slot + 1);
  segs[0] = (struct xhd_seg){ 0x8000, 0x1000 }; segs[1] = (struct xhd_seg){ 0x2000, 0x1000 };
  *slot = (struct xhd_slot){ 7, 0, STRIDE, W, H, 2, 1, 0, 8, H, 1, { 0 } };
  struct xhd_message mem = { { 'X', 'H', 'D', 'M' }, RAM, 0, SLOTS, SLOT }, frame = { { 'X', 'H', 'D', 'S' }, 2, 7, 0, 0 };
  int fds[2] = { ram, tab };
  send_fds(pair[0], &mem, fds, 2);
  send(pair[0], &frame, sizeof frame, 0);
  struct xhd_frame f = { .width = W, .height = H };
  struct xhd_direct d;
  xhd_direct_init(&d);
  struct xhd_rect damage = { 0, 0, 0, 0 };
  CHECK(xhd_drain_direct(pair[1], &f, &damage, &d) == 0 && d.ram && d.ram_size == RAM && d.slots == SLOTS && d.pending && d.slot == 2 && d.seq == 7);
  static uint8_t dst[DS * H], want[DS * H];
  memset(dst, 0xee, sizeof dst); memset(want, 0xee, sizeof want);
  for (uint32_t y = 0; y < H; y++) for (uint32_t x = 1; x < 9; x++) for (int b = 0; b < 4; b++) {
    uint64_t at = (uint64_t)y * STRIDE + x * 4 + (uint64_t)b;
    want[y * DS + x * 4 + (uint32_t)b] = rp[(at < 0x1000 ? 0x8000 : 0x2000 - 0x1000) + at];
  }
  struct xhd_rect done;
  CHECK(xhd_copy_direct(&d, dst, DS, W, H, 0, NULL, &done) == 0 && done.x == 1 && done.w == 8 && done.h == H);
  CHECK(!memcmp(dst, want, sizeof dst));                                 // every row, across the two pages, nothing else
  CHECK(xhd_copy_direct(&d, dst, DS, W, H, 1, NULL, &done) == 0);
  CHECK(dst[85 * DS + 4] == want[85 * DS + 6] && dst[85 * DS + 6] == want[85 * DS + 4] && dst[85 * DS + 7] == 0xff);   // swapped
  CHECK(xhd_copy_direct(&d, dst, DS, 4, 3, 0, NULL, &done) == 0 && done.w == 3 && done.h == 3);                // clipped to the buffer
  // The snapshot: one read of the guest's pages a frame, each tile hashed as read and copied on only when it changed;
  // after the read the guest's pages may change and the snapshot keeps the frame as it was read.
  {
#define GUEST(y, b) rp[((uint64_t)(y) * STRIDE + (b) < 0x1000 ? 0x8000 : 0x2000 - 0x1000) + (uint64_t)(y) * STRIDE + (b)]
    struct xhd_snapshot s;
    struct xhd_rect ch;
    CHECK(xhd_snapshot_init(&s, W, H) == 0 && !s.full && s.stride == W * 4 && s.tile == 3);
    CHECK(xhd_snapshot_update(&s, &d, &ch) == 0 && s.full && ch.x == 0 && ch.y == 0 && ch.w == W && ch.h == H);   // the first: all
    int same = 1;
    for (uint32_t y = 0; y < H; y++) for (uint32_t x = 0; x < W * 4; x++) if (s.pixels[y * s.stride + x] != GUEST(y, x)) same = 0;
    CHECK(same);                                                               // every row, across the two pages
    CHECK(xhd_snapshot_update(&s, &d, &ch) == 0 && ch.w == 0);                 // nothing changed: nothing copied
    GUEST(11, 4 * 4) ^= 0xff;                                                  // row 11, pixel 4: tile 1 (pixels 3-5)
    CHECK(xhd_snapshot_update(&s, &d, &ch) == 0 && ch.x == 3 && ch.w == 3 && ch.y == 11 && ch.h == 1);
    CHECK(s.pixels[11 * s.stride + 16] == GUEST(11, 16));
    GUEST(12, 3) ^= 0xff;                                                      // the X byte alone is no change
    CHECK(xhd_snapshot_update(&s, &d, &ch) == 0 && ch.w == 0);
    GUEST(12, 3) ^= 0xff;
    uint8_t kept = s.pixels[90 * s.stride];
    GUEST(90, 0) ^= 0xff;                                                      // row 90 (in the second page), pixel 0
    CHECK(s.pixels[90 * s.stride] == kept);                                    // the guest's write after the read: not in it
    CHECK(xhd_snapshot_update(&s, &d, &ch) == 0 && ch.x == 0 && ch.w == 3 && ch.y == 90 && ch.h == 1 && s.pixels[90 * s.stride] == (uint8_t)(kept ^ 0xff));
    // xnuhost's own pixels over a rectangle: its tiles lose their hashes, so the next frame takes them from the guest again.
    static uint8_t mem[W * 4 * H];
    memset(mem, 0x5a, sizeof mem);
    struct xhd_frame own = { .width = W, .height = H, .stride = W * 4, .format = XHD_FORMAT_BGRX, .pixels = mem };
    CHECK(xhd_snapshot_put(&s, &own, (struct xhd_rect){ 4, 20, 2, 3 }) == 0 && s.pixels[21 * s.stride + 16] == 0x5a);
    CHECK(xhd_snapshot_update(&s, &d, &ch) == 0 && ch.x == 3 && ch.w == 3 && ch.y == 20 && ch.h == 3 && s.pixels[21 * s.stride + 16] == GUEST(21, 16));
    own.format = XHD_FORMAT_RGBA16F;
    CHECK(xhd_snapshot_put(&s, &own, (struct xhd_rect){ 0, 0, W, H }) == -EINVAL);
    // A fault halfway (rows past the segments from row 85): the tiles read before it are in, the snapshot stays whole.
    GUEST(5, 0) ^= 0xff;
    slot->nsegs = 1;
    CHECK(xhd_snapshot_update(&s, &d, &ch) == -EINVAL && s.full && ch.y == 5 && ch.h == 1 && s.pixels[5 * s.stride] == GUEST(5, 0));
    slot->nsegs = 2;
    slot->seq = 11;
    CHECK(xhd_snapshot_update(&s, &d, &ch) == -ESTALE && ch.w == 0);           // the slot was reused: nothing read
    slot->seq = 7;
    GUEST(5, 0) ^= 0xff; GUEST(90, 0) ^= 0xff; GUEST(11, 16) ^= 0xff;
    xhd_snapshot_free(&s);
    CHECK(!s.pixels);
#undef GUEST
  }
  slot->nsegs = 1;
  CHECK(xhd_copy_direct(&d, dst, DS, W, H, 0, NULL, &done) == -EINVAL);                                         // rows past the segments
  slot->nsegs = 2; segs[1].offset = RAM - 0x800;
  CHECK(xhd_copy_direct(&d, dst, DS, W, H, 0, NULL, &done) == -EINVAL);                                         // a segment past the RAM
  segs[1].offset = 0x2000; slot->seq = 11;
  CHECK(xhd_copy_direct(&d, dst, DS, W, H, 0, NULL, &done) == -ESTALE);                                         // the slot was reused
  frame.b = 5;                                                                                              // an older frame: the newer stays
  send(pair[0], &frame, sizeof frame, 0);
  CHECK(xhd_drain_direct(pair[1], &f, &damage, &d) == 0 && d.seq == 7);
  CHECK(xhd_send_done(pair[1], 7, 0) == 0);
  struct xhd_message a;
  CHECK(recv(pair[0], &a, sizeof a, 0) == (ssize_t)sizeof a && !memcmp(a.magic, "XHDA", 4) && a.a == 7 && a.b == 0);
  xhd_direct_close(&d);
  munmap(rp, RAM); munmap(tp, SLOTS * SLOT);
  close(ram); close(tab); close(pair[0]); close(pair[1]);
}

// ---- the layer's buffers: Surface::lock's copy-back, and what the helper fills -----------------
// A model of AOSP's Surface::lock (libs/gui/Surface.cpp) on an 8x8 picture, a region as a 64-bit mask of its pixels:
// copy back `mDirtyRegion - dirty` from the last posted buffer, then mDirtyRegion -= the handed-out buffer's last dirty
// region, that region = dirty, mDirtyRegion |= dirty. No buffer posted yet: nothing copied back, the dirty region whole.
enum { CB = 8, CB_MAX = 5 };
struct surface_model { unsigned n; uint8_t buf[CB_MAX][CB * CB]; uint64_t slot_dirty[CB_MAX], dirty; int front; };

static uint64_t rect_mask(struct xhd_rect r)
{
  uint64_t m = 0;
  for (uint32_t y = r.y; y < r.y + r.h && y < CB; y++) for (uint32_t x = r.x; x < r.x + r.w && x < CB; x++) m |= 1ull << (y * CB + x);
  return m;
}

static struct xhd_rect mask_bounds(uint64_t m)
{
  struct xhd_rect r = { 0, 0, 0, 0 };
  for (unsigned i = 0; i < 64; i++) if (m >> i & 1) { struct xhd_rect p = { i % CB, i / CB, 1, 1 }; xhd_rect_union(&r, &p); }
  return r;
}

static struct xhd_rect model_lock(struct surface_model *s, unsigned b, struct xhd_rect want)
{
  uint64_t dirty = rect_mask(want);
  if (s->front >= 0) {
    uint64_t back = s->dirty & ~dirty;
    if ((int)b != s->front) for (unsigned i = 0; i < 64; i++) if (back >> i & 1) s->buf[b][i] = s->buf[s->front][i];
  } else {
    dirty = ~0ull;
    s->dirty = 0;
    memset(s->slot_dirty, 0, sizeof s->slot_dirty);
  }
  s->dirty &= ~s->slot_dirty[b];
  s->slot_dirty[b] = dirty;
  s->dirty |= dirty;
  return mask_bounds(dirty);
}

// 300 frames of a UI in which rectangles change here and there (a widget, a page, a clock). The helper before
// 2026-11-22 locked only the changed rectangle and filled it (`old`); now it locks the buffer whole and fills what
// xhd_buffers says the buffer lacks. order 0: the buffers in turn; 1: any buffer but the one shown, at random (a queue
// that does not hand them out in turn). Returns how many posts did not show the picture.
static int run_layer(unsigned nbuf, int old, int order, unsigned long *filled)
{
  struct surface_model s;
  memset(&s, 0, sizeof s);
  s.n = nbuf; s.front = -1;
  static uint8_t picture[CB * CB];
  memset(picture, 0, sizeof picture);
  struct xhd_buffers q;
  xhd_buffers_reset(&q, CB, CB);
  unsigned seed = 12345, b = 0;
  int wrong = 0;
  *filled = 0;
  for (unsigned k = 0; k < 300; k++) {
    // This frame's change: one rectangle (sometimes two), the frame's number written into it.
    struct xhd_rect changed = { 0, 0, 0, 0 };
    for (int n = 0; n < 1 + (k % 5 == 0); n++) {
      seed = seed * 1103515245 + 12345; uint32_t x = (seed >> 8) % CB, y = (seed >> 12) % CB;
      seed = seed * 1103515245 + 12345; uint32_t w = 1 + (seed >> 8) % 3, h = 1 + (seed >> 12) % 3;
      struct xhd_rect r = { x, y, w, h };
      xhd_rect_clip(&r, CB, CB);
      for (uint32_t yy = r.y; yy < r.y + r.h; yy++) for (uint32_t xx = r.x; xx < r.x + r.w; xx++) picture[yy * CB + xx] = (uint8_t)(k + 1);
      xhd_rect_union(&changed, &r);
    }
    if (k == 0) changed = (struct xhd_rect){ 0, 0, CB, CB };
    if (order == 0) b = k % nbuf;
    else {
      seed = seed * 1103515245 + 12345;
      if (nbuf == 1 || s.front < 0) b = (seed >> 8) % nbuf;
      else { b = (seed >> 8) % (nbuf - 1); if ((int)b >= s.front) b++; }   // any buffer but the one shown
    }
    struct xhd_rect fill;
    if (old) fill = model_lock(&s, b, changed);
    else {
      model_lock(&s, b, (struct xhd_rect){ 0, 0, CB, CB });
      fill = xhd_buffers_fill(&q, s.buf[b], &changed);
    }
    for (uint32_t yy = fill.y; yy < fill.y + fill.h; yy++) for (uint32_t xx = fill.x; xx < fill.x + fill.w; xx++) s.buf[b][yy * CB + xx] = picture[yy * CB + xx];
    *filled += (unsigned long)fill.w * fill.h;
    s.front = (int)b;                                                    // posted
    if (!old) xhd_buffers_posted(&q, s.buf[b], &changed);
    if (memcmp(s.buf[b], picture, sizeof picture)) wrong++;
  }
  return wrong;
}

static void test_buffers(void)
{
  unsigned long filled, whole = 300ul * CB * CB;
  // The fault as the phone had it: with two buffers Surface's copy-back is right, with three or more a post shows an
  // area as it was frames ago.
  CHECK(run_layer(2, 1, 0, &filled) == 0);
  CHECK(run_layer(3, 1, 0, &filled) > 0);
  CHECK(run_layer(4, 1, 0, &filled) > 0);
  // Now: right with any number of buffers in any order, and still far from filling every buffer whole.
  for (unsigned n = 1; n <= CB_MAX; n++) {
    CHECK(run_layer(n, 0, 0, &filled) == 0);
    CHECK(n > 3 || filled < whole * 3 / 4);                             // (on 8x8 the union of five changes is most of it)
    CHECK(run_layer(n, 0, 1, &filled) == 0);
  }
  // The bookkeeping itself.
  struct xhd_buffers q;
  xhd_buffers_reset(&q, 100, 50);
  static char bits[XHD_BUFFERS + 1];
  struct xhd_rect a = { 1, 2, 3, 4 }, b = { 50, 40, 10, 10 }, r;
  r = xhd_buffers_fill(&q, &bits[0], &a);
  CHECK(r.x == 0 && r.y == 0 && r.w == 100 && r.h == 50);                // not seen yet: whole
  xhd_buffers_posted(&q, &bits[0], &a);
  r = xhd_buffers_fill(&q, &bits[0], &b);
  CHECK(r.x == 50 && r.y == 40 && r.w == 10 && r.h == 10);               // seen, nothing posted since: only this change
  xhd_buffers_posted(&q, &bits[1], &b);                                  // another buffer posts b
  r = xhd_buffers_fill(&q, &bits[0], &a);
  CHECK(r.x == 1 && r.y == 2 && r.w == 59 && r.h == 48);                 // it lacks b, and gets a
  xhd_buffers_posted(&q, &bits[0], &a);
  struct xhd_rect none = { 0, 0, 0, 0 };
  r = xhd_buffers_fill(&q, &bits[1], &none);
  CHECK(r.x == 1 && r.y == 2 && r.w == 3 && r.h == 4);                   // the other one lacks a only
  for (unsigned i = 2; i <= XHD_BUFFERS; i++) xhd_buffers_posted(&q, &bits[i], &a);
  CHECK(q.n == XHD_BUFFERS && q.forgotten == 1);                         // nine buffers: the one posted longest ago forgotten
  r = xhd_buffers_fill(&q, &bits[1], &a);
  CHECK(r.w == 100 && r.h == 50);                                        // and so filled whole
  r = xhd_buffers_fill(&q, NULL, &a);
  CHECK(r.w == 100 && r.h == 50);
  xhd_buffers_reset(&q, 100, 50);
  r = xhd_buffers_fill(&q, &bits[0], &a);
  CHECK(r.w == 100 && q.n == 0);                                         // new buffers (a format fallback): none known
}

static void test_tile_hash(void)
{
  enum { N = 360 };
  static uint8_t p[N * 4];
  for (unsigned i = 0; i < sizeof p; i++) p[i] = (uint8_t)(i * 7 + 3);
  uint64_t h = xhd_hash_tile(p, N);
  CHECK(h && h == xhd_hash_tile(p, N) && h != xhd_hash_tile(p, N - 1));
  for (unsigned i = 3; i < sizeof p; i += 4) p[i] ^= 0x5a;               // the X bytes do not count
  CHECK(xhd_hash_tile(p, N) == h);
  // Every pair of changes to the top bit of the R byte of two pixels (what a multiply alone would let cancel in 16 bits),
  // every single bit of the first pixels, and a tile of all zeros against one of all X.
  int distinct = 1;
  for (unsigned i = 0; i < 64 && distinct; i++)
    for (unsigned j = i + 1; j < 64; j++) {
      p[i * 4 + 2] ^= 0x80; p[j * 4 + 2] ^= 0x80;
      if (xhd_hash_tile(p, N) == h) distinct = 0;
      p[i * 4 + 2] ^= 0x80; p[j * 4 + 2] ^= 0x80;
    }
  for (unsigned bit = 0; bit < 24 * 4 && distinct; bit++) {
    unsigned at = (bit / 24) * 4 + (bit % 24) / 8;
    p[at] ^= (uint8_t)(1u << (bit % 8));
    if (xhd_hash_tile(p, N) == h) distinct = 0;
    p[at] ^= (uint8_t)(1u << (bit % 8));
  }
  CHECK(distinct);
  memset(p, 0, sizeof p);
  uint64_t zero = xhd_hash_tile(p, N), odd = xhd_hash_tile(p, 7);
  for (unsigned i = 0; i < sizeof p; i++) p[i] = (i % 4 == 3) ? 0xff : 0;
  CHECK(zero == xhd_hash_tile(p, N) && zero != odd && xhd_hash_tile(p, 7) == odd);   // 7: the odd pixel at the end
}

// ---- the protocol against a serving thread --------------------------------------------------
struct server { char path[200]; int delay_ms; uint32_t width, height; int mode; int done; };

static void *serve(void *arg)
{
  struct server *s = arg;
  usleep((useconds_t)s->delay_ms * 1000);                               // the helper is already waiting for the socket
  int listener = xhd_listen(s->path);
  int fd = accept(listener, NULL, NULL);
  size_t bytes = (size_t)s->width * 4 * s->height;
  int memfd = xhd_memfd("test-frame", s->mode == 1 ? bytes / 2 : bytes);
  if (s->mode != 1) {
    uint8_t *p = mmap(NULL, bytes, PROT_READ | PROT_WRITE, MAP_SHARED, memfd, 0);
    for (size_t i = 0; i < bytes; i++) p[i] = (uint8_t)(i % 251);
    munmap(p, bytes);
  }
  if (s->mode == 2) {                                                   // a hello with no descriptor
    struct xhd_message m = { { 'X', 'H', 'D', '1' }, s->width, s->height, s->width * 4, 1 };
    send(fd, &m, sizeof m, 0);
  } else xhd_send_hello(fd, s->width, s->height, s->width * 4, XHD_FORMAT_BGRX, memfd);
  if (s->mode == 0) {
    xhd_send_flush(fd, 0, 0, s->width, s->height);                      // what xnuhost sends at a connection
    char ack;
    recv(fd, &ack, 1, 0);                                               // the test read the full flush
    xhd_send_flush(fd, 10, 10, 20, 20);
    xhd_send_flush(fd, 100, 5, 20, 20);
    xhd_send_flush(fd, s->width - 4, s->height - 4, 50, 50);            // reaches past the frame: clipped
    xhd_send_flush(fd, s->width + 5, 0, 5, 5);                          // outside: dropped
    struct xhd_message junk = { { 'N', 'O', 'P', 'E' }, 0, 0, 9999, 9999 };
    send(fd, &junk, sizeof junk, 0);                                    // unknown: ignored
    send(fd, "short", 5, 0);
    recv(fd, &ack, 1, 0);
  }
  close(memfd);
  close(fd);
  close(listener);
  s->done = 1;
  return NULL;
}

static void test_panel_report(void)
{
  int pair[2];
  CHECK(socketpair(AF_UNIX, SOCK_SEQPACKET, 0, pair) == 0);
  CHECK(xhd_send_panel(pair[0], 1440, 3120, 120, XHD_PANEL_DIRECT) == 0);
  struct xhd_message m;
  CHECK(recv(pair[1], &m, sizeof m, 0) == (ssize_t)sizeof m);
  CHECK(!memcmp(m.magic, "XHDP", 4) && m.a == 1440 && m.b == 3120 && m.c == 120 && m.d == 1);
  close(pair[0]);
  close(pair[1]);
}

// The first post waits for SESSION/ready (2026-11-15: the layer covers the OneOS boot screen, and the frames before
// ready are black or the console): held while it is missing, not once it is there, never without a session.
static void test_hold_first(const char *dir)
{
  char ready[300];
  snprintf(ready, sizeof ready, "%s/ready", dir);
  unlink(ready);
  CHECK(xhd_hold_first(ready) == 1);
  CHECK(xhd_hold_first(NULL) == 0 && xhd_hold_first("") == 0);                 // --socket: no hold
  FILE *f = fopen(ready, "w");
  CHECK(f && fputs("ready\n", f) >= 0 && fclose(f) == 0);
  CHECK(xhd_hold_first(ready) == 0);                                           // xnuhost said ready
  unlink(ready);
  CHECK(mkdir(ready, 0700) == 0 && xhd_hold_first(ready) == 0 && rmdir(ready) == 0);   // whatever it is: there
}

static void test_protocol(const char *dir)
{
  struct server s = { .delay_ms = 300, .width = 64, .height = 48 };
  snprintf(s.path, sizeof s.path, "%s/display.sock", dir);
  pthread_t thread;
  pthread_create(&thread, NULL, serve, &s);
  CHECK(xhd_connect(s.path, 0, NULL) == -ENOENT);                       // not there yet, and no wait asked
  int fd = xhd_connect(s.path, 5000, NULL);                             // waits for xnuhost to make it
  CHECK(fd >= 0);
  struct xhd_frame frame;
  CHECK(xhd_recv_hello(fd, &frame, 5000) == 0);
  CHECK(frame.width == 64 && frame.height == 48 && frame.stride == 256 && frame.format == XHD_FORMAT_BGRX && frame.bytes == 256 * 48);
  int same = 1;
  for (size_t i = 0; i < frame.bytes; i++) if (frame.pixels[i] != (uint8_t)(i % 251)) same = 0;
  CHECK(same);                                                          // the shared frame, mapped
  struct xhd_rect damage = { 0, 0, 0, 0 };
  for (int i = 0; i < 200 && !damage.w; i++) { CHECK(xhd_drain(fd, &frame, &damage) == 0); usleep(10000); }
  CHECK(damage.x == 0 && damage.y == 0 && damage.w == 64 && damage.h == 48);
  damage = (struct xhd_rect){ 0, 0, 0, 0 };
  CHECK(xhd_drain(fd, &frame, &damage) == 0 && damage.w == 0);          // nothing pending, and it did not block
  send(fd, "a", 1, 0);
  usleep(200000);
  CHECK(xhd_drain(fd, &frame, &damage) == 0);
  // 10,10 20x20 + 100,5 20x20 (clipped to 64 wide: dropped, x >= width) + the corner 60,44 4x4
  CHECK(damage.x == 10 && damage.y == 10 && damage.w == 54 && damage.h == 38);
  send(fd, "b", 1, 0);
  pthread_join(thread, NULL);
  damage = (struct xhd_rect){ 0, 0, 0, 0 };
  CHECK(xhd_drain(fd, &frame, &damage) == -1);                          // xnuhost closed: the session is over
  xhd_frame_close(&frame);
  CHECK(frame.pixels == NULL && frame.memfd == -1);
  close(fd);

  // Refused hellos: a frame larger than its memfd, and no memfd at all.
  for (int mode = 1; mode <= 2; mode++) {
    s = (struct server){ .delay_ms = 0, .width = 64, .height = 48, .mode = mode };
    snprintf(s.path, sizeof s.path, "%s/display.sock", dir);
    pthread_create(&thread, NULL, serve, &s);
    fd = xhd_connect(s.path, 5000, NULL);
    CHECK(fd >= 0);
    int e = xhd_recv_hello(fd, &frame, 5000);
    CHECK(e == (mode == 1 ? -EINVAL : -EPROTO));
    CHECK(frame.pixels == NULL);
    close(fd);
    pthread_join(thread, NULL);
  }

  // Not a socket: a link (even to a socket) and a plain file are refused; a stop ends the wait.
  char other[256], link_path[256];
  snprintf(other, sizeof other, "%s/real.sock", dir);
  snprintf(link_path, sizeof link_path, "%s/link.sock", dir);
  int listener = xhd_listen(other);
  CHECK(listener >= 0);
  CHECK(symlink(other, link_path) == 0);
  CHECK(xhd_connect(link_path, 0, NULL) == -ENOTSOCK);
  close(listener);
  unlink(link_path);
  unlink(other);
  FILE *plain = fopen(link_path, "w");
  fclose(plain);
  CHECK(xhd_connect(link_path, 1000, NULL) == -ENOTSOCK);
  unlink(link_path);
  volatile int stop = 1;
  CHECK(xhd_connect(link_path, 60000, &stop) == -ENOENT);               // returns at once
  // A socket nobody listens on any more: refused, after the wait.
  listener = xhd_listen(other);
  close(listener);
  CHECK(xhd_connect(other, 200, NULL) == -ECONNREFUSED);
  unlink(other);
  // A hello that never comes.
  int pair[2];
  CHECK(socketpair(AF_UNIX, SOCK_SEQPACKET, 0, pair) == 0);
  CHECK(xhd_recv_hello(pair[0], &frame, 100) == -ETIMEDOUT);
  close(pair[1]);
  CHECK(xhd_recv_hello(pair[0], &frame, 100) == -ECONNRESET);
  close(pair[0]);
  unlink(s.path);
}

// displayctl with a stand-in for the helper: a shell script that takes the socket path, and leaves.
static const char *self_path;
static void test_displayctl(const char *displayctl, const char *dir)
{
  char helper[256], command[1024];
  snprintf(helper, sizeof helper, "%s/helper.sh", dir);
  FILE *f = fopen(helper, "w");
  fprintf(f, "#!/bin/sh\necho \"$@\" > %s/helper.args\nexit 7\n", dir);
  fclose(f);
  chmod(helper, 0755);
  snprintf(command, sizeof command, "TMPDIR=%s %s test --seconds 1 --size 64x48 --helper %s -- --hz 60 > %s/out 2>&1", dir, displayctl, helper, dir);
  int status = system(command);
  CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 1);                  // a helper that never connects is a failure
  snprintf(command, sizeof command, "grep -q 'socket .*displayctl\\..*/display.sock --wait 5 --hz 60' %s/helper.args", dir);
  CHECK(system(command) == 0);                                          // direct diagnostic socket; never an XNU session
  snprintf(command, sizeof command, "grep -q 'ended before it connected' %s/out && ! ls -d %s/displayctl.* 2>/dev/null", dir, dir);
  CHECK(system(command) == 0);                                          // and its folder is gone
  snprintf(command, sizeof command, "rm -f %s/helper.sh %s/helper.args %s/out", dir, dir, dir);
  CHECK(system(command) == 0);
  // With a helper that speaks the protocol (this program, see main): the whole exchange passes.
  snprintf(command, sizeof command, "TMPDIR=%s %s test --seconds 1 --size 64x48 --helper %s > %s/out 2>&1", dir, displayctl, self_path, dir);
  status = system(command);
  CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0);
  snprintf(command, sizeof command, "grep -q 'removed its layer and exited 0' %s/out && grep -q '^displayctl: ok' %s/out", dir, dir);
  CHECK(system(command) == 0);
  // One initial full-frame hello flush + exactly one union rectangle per
  // animation update. This catches the S26 flicker-causing double flush.
  snprintf(command, sizeof command,
           "awk '/^displayctl: [0-9]+ frames in / {steps=$2+0} /^test-helper: [0-9]+ flush messages$/ {packets=$2+0} END {exit !(steps>0 && packets==steps+1)}' %s/out",
           dir);
  CHECK(system(command) == 0);
  // The half-float pattern: the stand-in checks the stride, the white edge and the wide band's P3 red.
  snprintf(command, sizeof command, "TMPDIR=%s %s test --seconds 1 --size 64x48 --format rgba16f --helper %s > %s/out 2>&1", dir, displayctl, self_path, dir);
  status = system(command);
  CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 0);
  snprintf(command, sizeof command, "grep -q 'pattern in R,G,B,A half float' %s/out && grep -q '^displayctl: ok' %s/out", dir, dir);
  CHECK(system(command) == 0);
  snprintf(command, sizeof command,
           "awk '/^displayctl: [0-9]+ frames in / {steps=$2+0} /^test-helper: [0-9]+ flush messages$/ {packets=$2+0} END {exit !(steps>0 && packets==steps+1)}' %s/out",
           dir);
  CHECK(system(command) == 0);
  snprintf(command, sizeof command, "rm -f %s/out", dir);
  CHECK(system(command) == 0);
  // A live One UI diagnostic must reject destructive or endpoint-overriding
  // helper flags before spawning a helper or creating a test session.
  static const char * const denied[] = { "--takeover", "--session", "--socket", "--lib", "--wait", "--logical" };
  for (unsigned n = 0; n < sizeof denied / sizeof *denied; n++) {
    snprintf(command, sizeof command, "TMPDIR=%s %s test --seconds 1 --size 64x48 --helper %s -- %s ignored > %s/out 2>&1",
             dir, displayctl, self_path, denied[n], dir);
    status = system(command);
    CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 2);
    snprintf(command, sizeof command, "grep -q 'refusing %s' %s/out && ! ls -d %s/displayctl.* 2>/dev/null && rm -f %s/out",
             denied[n], dir, dir, dir);
    CHECK(system(command) == 0);
  }
  snprintf(command, sizeof command, "%s test --format rgb565 > /dev/null 2>&1", displayctl);
  status = system(command);
  CHECK(WIFEXITED(status) && WEXITSTATUS(status) == 2);                  // a format it does not make: usage
}

// This program as the helper displayctl starts (--socket PATH --wait N): connects, takes the hello,
// reads flushes until the socket closes, and checks it saw the moving square's damage.
static int as_helper(const char *socket_path)
{
  // Diagnostics must be free of the real guest's session/ready requirement.
  if (xhd_hold_first(NULL)) return 9;
  int fd = xhd_connect(socket_path, 5000, NULL);
  struct xhd_frame frame;
  if (fd < 0 || xhd_recv_hello(fd, &frame, 5000)) return 9;
  int full = 0, partial = 0, flushes = 0;
  for (int i = 0; i < 2000; i++) {
    // Count wire packets, not helper presentations: a double-flush per
    // animation step would pass the image tests yet flicker on the S26.
    struct xhd_message m;
    ssize_t n = recv(fd, &m, sizeof m, MSG_DONTWAIT);
    if (n < 0 && errno == EINTR) continue;
    if (n < 0 && (errno == EAGAIN || errno == EWOULDBLOCK)) { usleep(5000); continue; }
    if (n < 0) return 10;
    if (n > 0) {
      if (n != sizeof m || memcmp(m.magic, "XHDF", 4)) return 11;
      struct xhd_rect r = { m.a, m.b, m.c, m.d };
      if (!xhd_rect_clip(&r, frame.width, frame.height)) return 12;
      flushes++;
      if (r.w == frame.width && r.h == frame.height) full++;
      else partial++;
      continue;
    }
    // Peer closed: one initial full flush plus ONE union flush per
    // completed animation step. These counts must agree with displayctl.
    if (n == 0) {
      printf("test-helper: %d flush messages\n", flushes);
    fflush(stdout);
    if (frame.format == XHD_FORMAT_BGRX) return full && partial && frame.pixels[3] == 255 ? 0 : 8;
      // half floats: the white edge at 1.0, and the wide band's P3 red (second eighth, lowest quarter) above sRGB's 1.0
      // (the bouncing square may cover part of the band: any pixel of the row will do)
      uint16_t edge[4], p3[4];
      memcpy(edge, frame.pixels, 8);
      int wide = 0;
      for (uint32_t x = frame.width / 8; x < frame.width / 4; x++) {
        memcpy(p3, frame.pixels + (size_t)(frame.height - 4) * frame.stride + (size_t)x * 8, 8);
        if (xh_half_to_float(p3[0]) > 1.2f && xh_half_to_float(p3[1]) < 0.0f) wide = 1;
      }
      int ok = frame.format == XHD_FORMAT_RGBA16F && frame.stride == frame.width * 8 && edge[0] == 0x3c00 && edge[3] == 0x3c00 && wide;
      return full && partial && ok ? 0 : 6;
    }
  }
  return 7;
}

int main(int argc, char **argv)
{
  if (argc > 2 && !strcmp(argv[1], "--socket")) return as_helper(argv[2]);
  self_path = argv[0];
  char dir[] = "/tmp/oneios-display-test.XXXXXX";
  if (!mkdtemp(dir)) { perror("mkdtemp"); return 1; }
  test_fit();
  test_rects();
  test_hello_check();
  test_copy();
  test_formats();
  test_copy_formats();
  test_panel_report();
  test_direct();
  test_buffers();
  test_tile_hash();
  test_hold_first(dir);
  test_protocol(dir);
  if (argc > 1) test_displayctl(argv[1], dir);
  rmdir(dir);
  printf("%s: %d checks, %d failed\n", failed ? "FAILED" : "ok", checks, failed);
  return failed ? 1 : 0;
}
