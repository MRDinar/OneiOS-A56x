// SPDX-License-Identifier: MIT
#define _GNU_SOURCE
#include "xhd.h"
#include <errno.h>
#include <poll.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/un.h>
#include <time.h>
#include <unistd.h>

static long long now_ms(void)
{
  struct timespec t;
  clock_gettime(CLOCK_MONOTONIC, &t);
  return (long long)t.tv_sec * 1000 + t.tv_nsec / 1000000;
}

int xhd_check_hello(const struct xhd_message *m, size_t length, off_t memfd_size)
{
  if (length != sizeof *m || memcmp(m->magic, "XHD1", 4)) return -EPROTO;
  uint32_t width = m->a, height = m->b, stride = m->c, format = m->d;
  if (!width || !height || width > XHD_MAX_SIDE || height > XHD_MAX_SIDE) return -EINVAL;
  unsigned bpp = xh_pixels_bpp(format);
  if (!bpp) return -EINVAL;
  if (stride < width * bpp || stride > XHD_MAX_SIDE * 8 || stride % bpp) return -EINVAL;
  if (memfd_size < 0 || (uint64_t)stride * height > (uint64_t)memfd_size) return -EINVAL;
  return 0;
}

int xhd_connect(const char *path, int wait_ms, const volatile int *stop)
{
  struct sockaddr_un addr = { .sun_family = AF_UNIX };
  if (strlen(path) >= sizeof addr.sun_path) return -ENAMETOOLONG;
  strcpy(addr.sun_path, path);
  long long end = now_ms() + (wait_ms > 0 ? wait_ms : 0);
  int error = ENOENT;
  for (;;) {
    struct stat st;
    if (lstat(path, &st)) error = errno;
    else if (!S_ISSOCK(st.st_mode)) return -ENOTSOCK;       // a link or a file put there: not ours to follow
    else {
      int fd = socket(AF_UNIX, SOCK_SEQPACKET | SOCK_CLOEXEC, 0);
      if (fd < 0) return -errno;
      if (!connect(fd, (struct sockaddr *)&addr, sizeof addr)) return fd;
      error = errno;
      close(fd);
      if (error != ECONNREFUSED && error != ENOENT && error != EAGAIN && error != EINTR) return -error;
    }
    if ((stop && *stop) || now_ms() >= end) return -error;
    struct timespec nap = { 0, 100 * 1000000 };
    nanosleep(&nap, NULL);
  }
}

int xhd_send_panel(int fd, uint32_t width, uint32_t height, uint32_t hz, uint32_t flags)
{
  struct xhd_message m = { { 'X', 'H', 'D', 'P' }, width, height, hz, flags };
  return send(fd, &m, sizeof m, MSG_NOSIGNAL) < 0 ? -errno : 0;
}

int xhd_recv_hello(int fd, struct xhd_frame *frame, int timeout_ms)
{
  memset(frame, 0, sizeof *frame);
  frame->memfd = -1;
  struct pollfd p = { fd, POLLIN, 0 };
  int ready;
  do ready = poll(&p, 1, timeout_ms); while (ready < 0 && errno == EINTR);
  if (ready < 0) return -errno;
  if (!ready) return -ETIMEDOUT;
  struct xhd_message hello;
  char control[CMSG_SPACE(sizeof(int) * 4)];
  struct iovec iov = { &hello, sizeof hello };
  struct msghdr msg = { .msg_iov = &iov, .msg_iovlen = 1, .msg_control = control, .msg_controllen = sizeof control };
  ssize_t n;
  do n = recvmsg(fd, &msg, MSG_CMSG_CLOEXEC); while (n < 0 && errno == EINTR);
  if (n < 0) return -errno;
  // Every descriptor that came is ours to close; the first is the frame.
  int memfd = -1;
  for (struct cmsghdr *cm = CMSG_FIRSTHDR(&msg); cm; cm = CMSG_NXTHDR(&msg, cm)) {
    if (cm->cmsg_level != SOL_SOCKET || cm->cmsg_type != SCM_RIGHTS) continue;
    size_t count = (cm->cmsg_len - CMSG_LEN(0)) / sizeof(int);
    for (size_t i = 0; i < count; i++) {
      int got;
      memcpy(&got, CMSG_DATA(cm) + i * sizeof(int), sizeof(int));
      if (memfd < 0) memfd = got; else close(got);
    }
  }
  if (n == 0) { if (memfd >= 0) close(memfd); return -ECONNRESET; }
  if (memfd < 0 || (msg.msg_flags & (MSG_TRUNC | MSG_CTRUNC))) { if (memfd >= 0) close(memfd); return -EPROTO; }
  struct stat st;
  int bad = fstat(memfd, &st) ? -errno : !S_ISREG(st.st_mode) ? -EINVAL : xhd_check_hello(&hello, (size_t)n, st.st_size);
  if (bad) { close(memfd); return bad; }
  size_t bytes = (size_t)hello.c * hello.b;
  void *map = mmap(NULL, bytes, PROT_READ, MAP_SHARED, memfd, 0);
  if (map == MAP_FAILED) { int e = errno; close(memfd); return -e; }
  frame->width = hello.a; frame->height = hello.b; frame->stride = hello.c; frame->format = hello.d;
  frame->memfd = memfd; frame->pixels = map; frame->bytes = bytes;
  return 0;
}

void xhd_frame_close(struct xhd_frame *frame)
{
  if (frame->pixels) munmap((void *)frame->pixels, frame->bytes);
  if (frame->memfd >= 0) close(frame->memfd);
  frame->pixels = NULL;
  frame->memfd = -1;
}

int xhd_rect_clip(struct xhd_rect *r, uint32_t width, uint32_t height)
{
  if (!r->w || !r->h || r->x >= width || r->y >= height) { r->x = r->y = r->w = r->h = 0; return 0; }
  if (r->w > width - r->x) r->w = width - r->x;
  if (r->h > height - r->y) r->h = height - r->y;
  return 1;
}

void xhd_rect_union(struct xhd_rect *into, const struct xhd_rect *r)
{
  if (!r->w || !r->h) return;
  if (!into->w || !into->h) { *into = *r; return; }
  uint32_t x0 = into->x < r->x ? into->x : r->x, y0 = into->y < r->y ? into->y : r->y;
  uint64_t ax = (uint64_t)into->x + into->w, bx = (uint64_t)r->x + r->w, ay = (uint64_t)into->y + into->h, by = (uint64_t)r->y + r->h;
  uint64_t x1 = ax > bx ? ax : bx, y1 = ay > by ? ay : by;
  into->x = x0; into->y = y0; into->w = (uint32_t)(x1 - x0); into->h = (uint32_t)(y1 - y0);
}

int xhd_drain(int fd, const struct xhd_frame *frame, struct xhd_rect *damage) { return xhd_drain_direct(fd, frame, damage, NULL); }

void xhd_direct_init(struct xhd_direct *d)
{
  memset(d, 0, sizeof *d);
  d->ram_fd = d->table_fd = -1;
}

void xhd_direct_close(struct xhd_direct *d)
{
  if (d->ram) munmap((void *)d->ram, d->ram_size);
  if (d->table) munmap((void *)d->table, d->table_bytes);
  if (d->ram_fd >= 0) close(d->ram_fd);
  if (d->table_fd >= 0) close(d->table_fd);
  xhd_direct_init(d);
}

// xnuhost's "memory": guest RAM and the table, both mapped read-only. Only the first is taken.
static void direct_memory(struct xhd_direct *d, const struct xhd_message *m, const int *fds, unsigned nfds)
{
  uint64_t ram = (uint64_t)m->a | (uint64_t)m->b << 32;
  struct stat a, b;
  if (!d || d->ram || nfds != 2 || !m->c || m->c > 64 || m->d < sizeof(struct xhd_slot) || m->d > (64u << 20) || m->d % 8 ||
      fstat(fds[0], &a) || fstat(fds[1], &b) || !ram || (uint64_t)a.st_size < ram || (uint64_t)b.st_size < (uint64_t)m->c * m->d) return;
  void *rm = mmap(NULL, ram, PROT_READ, MAP_SHARED, fds[0], 0);
  if (rm == MAP_FAILED) return;
  void *tm = mmap(NULL, (size_t)m->c * m->d, PROT_READ, MAP_SHARED, fds[1], 0);
  if (tm == MAP_FAILED) { munmap(rm, ram); return; }
  d->ram = rm; d->ram_size = ram; d->table = tm; d->table_bytes = (size_t)m->c * m->d; d->slots = m->c; d->slot_bytes = m->d;
  d->ram_fd = dup(fds[0]); d->table_fd = dup(fds[1]);
}

int xhd_hold_first(const char *ready)
{
  struct stat st;
  return ready && *ready && stat(ready, &st) && errno == ENOENT;
}

int xhd_drain_direct(int fd, const struct xhd_frame *frame, struct xhd_rect *damage, struct xhd_direct *direct)
{
  // A bound on one round: a guest that floods flushes must not keep the frame from being posted.
  for (int i = 0; i < 4096; i++) {
    struct xhd_message m;
    char control[CMSG_SPACE(sizeof(int) * 4)];
    struct iovec iov = { &m, sizeof m };
    struct msghdr msg = { .msg_iov = &iov, .msg_iovlen = 1, .msg_control = control, .msg_controllen = sizeof control };
    ssize_t n = recvmsg(fd, &msg, MSG_DONTWAIT | MSG_CMSG_CLOEXEC);
    if (n < 0) {
      if (errno == EINTR) continue;
      if (errno == EAGAIN || errno == EWOULDBLOCK) return 0;
      fprintf(stderr, "oneios-display: reading xnuhost's socket failed: %s\n", strerror(errno));
      return -1;
    }
    int fds[4];
    unsigned nfds = 0;
    for (struct cmsghdr *cm = CMSG_FIRSTHDR(&msg); cm; cm = CMSG_NXTHDR(&msg, cm)) {
      if (cm->cmsg_level != SOL_SOCKET || cm->cmsg_type != SCM_RIGHTS) continue;
      size_t count = (cm->cmsg_len - CMSG_LEN(0)) / sizeof(int);
      for (size_t k = 0; k < count; k++) {
        int got;
        memcpy(&got, CMSG_DATA(cm) + k * sizeof(int), sizeof(int));
        if (nfds < 4) fds[nfds++] = got; else close(got);
      }
    }
    if (n == sizeof m && !memcmp(m.magic, "XHDM", 4)) direct_memory(direct, &m, fds, nfds);
    for (unsigned k = 0; k < nfds; k++) close(fds[k]);                // kept ones were duplicated
    if (n == 0) { fprintf(stderr, "oneios-display: xnuhost closed the socket\n"); return -1; }
    if (n != sizeof m) continue;
    if (!memcmp(m.magic, "XHDS", 4)) {
      // The newest frame wins; the answer for it covers the ones before it.
      uint64_t seq = (uint64_t)m.b | (uint64_t)m.c << 32;
      if (direct && direct->ram && m.a < direct->slots && (!direct->pending || seq > direct->seq)) { direct->pending = 1; direct->slot = m.a; direct->seq = seq; }
      continue;
    }
    if (memcmp(m.magic, "XHDF", 4)) continue;                         // not a flush: ignored
    struct xhd_rect r = { m.a, m.b, m.c, m.d };
    if (xhd_rect_clip(&r, frame->width, frame->height)) xhd_rect_union(damage, &r);
  }
  return 0;
}

// The rows of the slot's frame: a cursor over its segments, for rows that only go forward. Calls `piece` for each run of
// guest bytes in [x, x + w) of a row, in order. -EINVAL when the rows reach past the segments or a segment past RAM.
struct walk { const struct xhd_direct *d; struct xhd_slot s; const struct xhd_seg *segs; unsigned at; uint64_t start; };

static int walk_open(struct walk *k, const struct xhd_direct *d)
{
  if (!d->ram || !d->pending || d->slot >= d->slots) return -EINVAL;
  const uint8_t *base = d->table + (size_t)d->slot * d->slot_bytes;
  memcpy(&k->s, base, sizeof k->s);
  if (__atomic_load_n((const uint64_t *)base, __ATOMIC_ACQUIRE) != d->seq || k->s.seq != d->seq) return -ESTALE;   // reused already
  if (k->s.format != 1 || !k->s.width || !k->s.height || k->s.stride < (uint64_t)k->s.width * 4 ||
      k->s.nsegs > (d->slot_bytes - sizeof k->s) / sizeof(struct xhd_seg)) return -EINVAL;
  k->d = d; k->segs = (const struct xhd_seg *)(base + sizeof k->s); k->at = 0; k->start = 0;
  return 0;
}

typedef void (*piece_fn)(void *ctx, const uint8_t *from, uint64_t bytes);
static int walk_row(struct walk *k, uint32_t row, uint32_t x, uint32_t w, piece_fn piece, void *ctx)
{
  uint64_t want = k->s.offset + (uint64_t)row * k->s.stride + (uint64_t)x * 4, left = (uint64_t)w * 4;
  while (k->at < k->s.nsegs && want >= k->start + k->segs[k->at].length) { k->start += k->segs[k->at].length; k->at++; }
  unsigned i = k->at;
  uint64_t i_start = k->start, pos = want;
  while (left) {
    if (i >= k->s.nsegs) return -EINVAL;
    const struct xhd_seg *g = &k->segs[i];
    if (g->offset > k->d->ram_size || g->length > k->d->ram_size - g->offset || pos < i_start) return -EINVAL;
    uint64_t in = pos - i_start, n = g->length - in < left ? g->length - in : left;
    piece(ctx, k->d->ram + g->offset + in, n);
    left -= n; pos += n;
    if (left) { i_start += g->length; i++; }
  }
  return 0;
}

struct copy_ctx { uint8_t *to; int swap; };
static void copy_piece(void *ctx, const uint8_t *from, uint64_t n)
{
  struct copy_ctx *c = ctx;
  if (!c->swap) memcpy(c->to, from, (size_t)n);
  else for (uint64_t i = 0; i + 4 <= n; i += 4) { c->to[i] = from[i + 2]; c->to[i + 1] = from[i + 1]; c->to[i + 2] = from[i]; c->to[i + 3] = 0xff; }
  c->to += n;
}

int xhd_copy_direct(const struct xhd_direct *d, uint8_t *dst, size_t dst_stride, uint32_t dst_width, uint32_t dst_height, int swap,
                    const struct xhd_rect *only, struct xhd_rect *done)
{
  memset(done, 0, sizeof *done);
  struct walk k;
  int e = walk_open(&k, d);
  if (e) return e;
  struct xhd_rect r = only ? *only : (struct xhd_rect){ k.s.x, k.s.y, k.s.w, k.s.h };
  if (!xhd_rect_clip(&r, k.s.width, k.s.height) || !xhd_rect_clip(&r, dst_width, dst_height)) return 0;
  for (uint32_t row = r.y; row < r.y + r.h; row++) {
    struct copy_ctx c = { dst + (size_t)row * dst_stride + (size_t)r.x * 4, swap };
    if ((e = walk_row(&k, row, r.x, r.w, copy_piece, &c))) return e;
  }
  *done = r;
  return 0;
}

// ---- damage -----------------------------------------------------------------------------------
// Four lanes by pixel index (`at`, the pixel's place in the stream), each a multiply-add over the pixel's B,G,R: a
// function of the bytes and their order alone, however the stream is cut into pieces.
uint64_t xhd_hash_pixels(uint64_t state[4], const uint8_t *p, size_t bytes, size_t at)
{
  const uint64_t K = 0x9e3779b97f4a7c15ull;
  for (size_t i = 0; i + 4 <= bytes; i += 4, at++) {
    uint32_t v;
    memcpy(&v, p + i, 4);
    state[at & 3] = (state[at & 3] + (v & 0x00ffffffu) + 1) * K;
  }
  return at;
}

int xhd_damage_init(struct xhd_damage *d, uint32_t width, uint32_t height)
{
  memset(d, 0, sizeof *d);
  size_t n = (size_t)height * XHD_TILES;
  d->shown = calloc(n, sizeof *d->shown);
  d->next = calloc(n, sizeof *d->next);
  if (!d->shown || !d->next) { xhd_damage_free(d); return -ENOMEM; }
  d->width = width; d->height = height; d->tile = (width + XHD_TILES - 1) / XHD_TILES;
  return 0;
}

void xhd_damage_free(struct xhd_damage *d)
{
  free(d->shown); free(d->next);
  memset(d, 0, sizeof *d);
}

struct hash_ctx { uint64_t lane[4]; size_t at; };
static void hash_piece(void *ctx, const uint8_t *from, uint64_t n)
{
  struct hash_ctx *h = ctx;
  h->at = xhd_hash_pixels(h->lane, from, (size_t)n, h->at);
}

int xhd_direct_damage(const struct xhd_direct *dir, struct xhd_damage *d, struct xhd_rect *dirty)
{
  memset(dirty, 0, sizeof *dirty);
  struct walk k;
  int e = walk_open(&k, dir);
  if (e) return e;
  uint32_t width = k.s.width < d->width ? k.s.width : d->width, height = k.s.height < d->height ? k.s.height : d->height;
  uint32_t x0 = UINT32_MAX, x1 = 0, y0 = UINT32_MAX, y1 = 0;
  for (uint32_t row = 0; row < height; row++)
    for (uint32_t t = 0; t < XHD_TILES; t++) {
      uint32_t x = t * d->tile, w = x >= width ? 0 : (width - x < d->tile ? width - x : d->tile);
      if (!w) continue;
      struct hash_ctx h = { { 1, 2, 3, 4 }, 0 };
      if ((e = walk_row(&k, row, x, w, hash_piece, &h))) return e;
      uint64_t v = h.lane[0] ^ (h.lane[1] * 3) ^ (h.lane[2] * 5) ^ (h.lane[3] * 7) ^ ((uint64_t)w << 48), *slot = &d->next[(size_t)row * XHD_TILES + t];
      *slot = v;
      if (d->valid && d->shown[(size_t)row * XHD_TILES + t] == v) continue;
      if (x < x0) x0 = x;
      if (x + w > x1) x1 = x + w;
      if (row < y0) y0 = row;
      y1 = row + 1;
    }
  if (y0 != UINT32_MAX) *dirty = (struct xhd_rect){ x0, y0, x1 - x0, y1 - y0 };
  return 0;
}

void xhd_damage_commit(struct xhd_damage *d)
{
  uint64_t *t = d->shown;
  d->shown = d->next; d->next = t;
  d->valid = 1;
}

int xhd_send_done(int fd, uint64_t seq, uint32_t flags)
{
  struct xhd_message m = { { 'X', 'H', 'D', 'A' }, (uint32_t)seq, (uint32_t)(seq >> 32), flags, 0 };
  return send(fd, &m, sizeof m, MSG_NOSIGNAL | MSG_DONTWAIT) < 0 ? -errno : 0;
}

void xhd_fit(uint32_t sw, uint32_t sh, uint32_t dw, uint32_t dh, struct xhd_fit *fit)
{
  memset(fit, 0, sizeof *fit);
  if (!sw || !sh || !dw || !dh) return;
  uint64_t w, h;
  if ((uint64_t)sw * dh <= (uint64_t)sh * dw) {        // the source is the narrower: full height, bars left and right
    h = dh;
    w = ((uint64_t)sw * dh + sh / 2) / sh;
  } else {                                             // full width, bars above and below
    w = dw;
    h = ((uint64_t)sh * dw + sw / 2) / sw;
  }
  if (w < 1) w = 1;
  if (h < 1) h = 1;
  if (w > dw) w = dw;
  if (h > dh) h = dh;
  fit->w = (uint32_t)w; fit->h = (uint32_t)h;
  fit->x = (dw - fit->w) / 2; fit->y = (dh - fit->h) / 2;
}

int xhd_copy(uint8_t *dst, size_t dst_stride, int buffer, const uint8_t *src, size_t src_stride, uint32_t format, struct xhd_rect r)
{
  unsigned in = xh_pixels_bpp(format), out = buffer == XHD_BUFFER_RGBA_FP16 ? 8 : buffer == XHD_BUFFER_BGRA_8888 || buffer == XHD_BUFFER_RGBX_8888 ? 4 : 0;
  if (!in || !out) return -EINVAL;
  for (uint32_t row = r.y; row < r.y + r.h; row++) {
    const uint8_t *s = src + (size_t)row * src_stride + (size_t)r.x * in;
    uint8_t *d = dst + (size_t)row * dst_stride + (size_t)r.x * out;
    if (format == XHD_FORMAT_RGBA16F) {
      if (buffer == XHD_BUFFER_RGBA_FP16) memcpy(d, s, (size_t)r.w * 8);
      else xh_pixels_rgba16f_to_8(d, s, r.w, buffer == XHD_BUFFER_RGBX_8888);
    } else if (buffer == XHD_BUFFER_BGRA_8888) memcpy(d, s, (size_t)r.w * 4);
    else if (buffer == XHD_BUFFER_RGBA_FP16) xh_pixels_bgrx8_to_rgba16f(d, s, r.w);
    else for (uint32_t i = 0; i < r.w; i++, s += 4, d += 4) { d[0] = s[2]; d[1] = s[1]; d[2] = s[0]; d[3] = 0xff; }
  }
  return 0;
}

int xhd_buffer_for(uint32_t format, int *dataspace)
{
  if (format == XHD_FORMAT_BGRX) { *dataspace = XHD_DATASPACE_SRGB; return XHD_BUFFER_BGRA_8888; }
  if (format == XHD_FORMAT_RGBA16F) { *dataspace = XHD_DATASPACE_SCRGB_LINEAR; return XHD_BUFFER_RGBA_FP16; }
  *dataspace = XHD_DATASPACE_UNKNOWN;
  return 0;
}

static const struct { const char *name; int value; } dataspaces[] = {
  { "unknown", XHD_DATASPACE_UNKNOWN }, { "srgb", XHD_DATASPACE_SRGB }, { "p3", XHD_DATASPACE_DISPLAY_P3 },
  { "scrgb", XHD_DATASPACE_SCRGB }, { "scrgb-linear", XHD_DATASPACE_SCRGB_LINEAR },
};

const char *xhd_dataspace_name(int dataspace)
{
  for (size_t i = 0; i < sizeof dataspaces / sizeof *dataspaces; i++) if (dataspaces[i].value == dataspace) return dataspaces[i].name;
  return "other";
}

int xhd_dataspace_parse(const char *name)
{
  if (!strcmp(name, "auto")) return -1;
  for (size_t i = 0; i < sizeof dataspaces / sizeof *dataspaces; i++) if (!strcmp(dataspaces[i].name, name)) return dataspaces[i].value;
  return -2;
}

int xhd_listen(const char *path)
{
  struct sockaddr_un addr = { .sun_family = AF_UNIX };
  if (strlen(path) >= sizeof addr.sun_path) return -ENAMETOOLONG;
  strcpy(addr.sun_path, path);
  unlink(path);
  int fd = socket(AF_UNIX, SOCK_SEQPACKET | SOCK_CLOEXEC, 0);
  if (fd < 0) return -errno;
  if (bind(fd, (struct sockaddr *)&addr, sizeof addr) || listen(fd, 2)) { int e = errno; close(fd); return -e; }
  return fd;
}

int xhd_send_hello(int fd, uint32_t width, uint32_t height, uint32_t stride, uint32_t format, int memfd)
{
  struct xhd_message hello = { { 'X', 'H', 'D', '1' }, width, height, stride, format };
  char control[CMSG_SPACE(sizeof(int))];
  struct iovec iov = { &hello, sizeof hello };
  struct msghdr msg = { .msg_iov = &iov, .msg_iovlen = 1, .msg_control = control, .msg_controllen = sizeof control };
  memset(control, 0, sizeof control);
  struct cmsghdr *cm = CMSG_FIRSTHDR(&msg);
  cm->cmsg_level = SOL_SOCKET;
  cm->cmsg_type = SCM_RIGHTS;
  cm->cmsg_len = CMSG_LEN(sizeof(int));
  memcpy(CMSG_DATA(cm), &memfd, sizeof(int));
  return sendmsg(fd, &msg, MSG_NOSIGNAL) < 0 ? -errno : 0;
}

int xhd_send_flush(int fd, uint32_t x, uint32_t y, uint32_t w, uint32_t h)
{
  struct xhd_message m = { { 'X', 'H', 'D', 'F' }, x, y, w, h };
  return send(fd, &m, sizeof m, MSG_NOSIGNAL) < 0 ? -errno : 0;
}

int xhd_memfd(const char *name, size_t bytes)
{
  int fd = (int)syscall(SYS_memfd_create, name, 1u /* MFD_CLOEXEC */);
  if (fd < 0) return -errno;
  if (ftruncate(fd, (off_t)bytes)) { int e = errno; close(fd); return -e; }
  return fd;
}
