// SPDX-License-Identifier: MIT
// The display helper's portable core: xnuhost's display.sock protocol (runtime/xnuhost/src/display.c,
// docs/booter.md 2.8), the letterbox fit, and the row copy. No Android here, so the host tests run it.
#ifndef ONEIOS_XHD_H
#define ONEIOS_XHD_H
#include <stddef.h>
#include <stdint.h>
#include <sys/types.h>

#define XHD_FORMAT_BGRX 1u            // bytes B,G,R,X
#define XHD_MAX_SIDE 16384u

// One SOCK_SEQPACKET message, little-endian:
//   hello  { "XHD1", width, height, stride, format } + the frame memfd (SCM_RIGHTS)
//   flush  { "XHDF", x, y, w, h }
// and from the helper, straight after connecting (xnuhost waits for it before it sizes the frame in a session):
//   panel  { "XHDP", width, height, hz, flags }      flags bit 0 (XHD_PANEL_DIRECT): frames may come as "frame" below
// Direct frames (runtime/xnuhost/src/display.c): the helper copies the guest's scanout from guest RAM itself.
//   memory { "XHDM", RAM bytes low, high, slots, slot bytes } + the RAM memfd and the table memfd (SCM_RIGHTS)
//   frame  { "XHDS", slot, seq low, seq high, 0 }    described by that slot of the table (struct xhd_slot, then segments)
//   done   { "XHDA", seq low, seq high, flags, 0 }   (helper -> xnuhost) the frames up to seq are in the layer; flags
//          bit 0 (XHD_DONE_FAILED): the last could not be copied, and xnuhost goes back to copying frames itself
struct xhd_message { char magic[4]; uint32_t a, b, c, d; };

struct xhd_rect { uint32_t x, y, w, h; };            // w == 0: empty

struct xhd_frame {
  uint32_t width, height, stride, format;
  int memfd;
  const uint8_t *pixels;                              // read-only map of the memfd
  size_t bytes;
};

// The session's folder is writable by the guest, so nothing from the socket is trusted:
// 0 when the hello describes a frame that fits in memfd_size bytes, else -EPROTO/-EINVAL.
int xhd_check_hello(const struct xhd_message *m, size_t length, off_t memfd_size);
// Connect to the socket at path, waiting up to wait_ms for it to appear; *stop != 0 ends the wait.
// Refuses anything that is not a socket (a link included). Returns the fd or -errno.
int xhd_connect(const char *path, int wait_ms, const volatile int *stop);
// Tell xnuhost the panel's mode (and XHD_PANEL_DIRECT in flags: direct frames are welcome). 0 or -errno.
#define XHD_PANEL_DIRECT 1u
int xhd_send_panel(int fd, uint32_t width, uint32_t height, uint32_t hz, uint32_t flags);

// A slot of the direct table, as xnuhost writes it: the frame `seq`, rows of `stride` bytes from byte `offset` of the
// segments (the guest's pages as offsets into the RAM memfd, in the scanout's order), `width` x `height`, the
// rectangle x, y, w, h changed, format 1 (B,G,R,X/A). seq is written last.
struct xhd_slot { uint64_t seq, offset; uint32_t stride, width, height, nsegs, x, y, w, h, format, pad[3]; };
struct xhd_seg { uint64_t offset, length; };
struct xhd_direct {
  int ram_fd, table_fd;
  const uint8_t *ram, *table;                         // read-only maps; NULL until xnuhost's "memory"
  uint64_t ram_size;
  size_t table_bytes;
  uint32_t slots, slot_bytes;
  int pending;                                        // a frame came: the newest is slot / seq
  uint32_t slot;
  uint64_t seq;
};
void xhd_direct_init(struct xhd_direct *d);
void xhd_direct_close(struct xhd_direct *d);
// Copy a rectangle of the pending frame (`only`, or NULL: the slot's changed rectangle; clipped to the frame and to
// dst_width x dst_height) from guest RAM into a B,G,R,X/A buffer (swap: R,G,B,X, alpha 0xff). 0, with *done the
// rectangle, or -errno: a slot that changed or does not fit RAM copies nothing.
int xhd_copy_direct(const struct xhd_direct *d, uint8_t *dst, size_t dst_stride, uint32_t dst_width, uint32_t dst_height, int swap,
                    const struct xhd_rect *only, struct xhd_rect *done);

// What changed: the guest flushes its whole surface every frame (its swap carries no dirty region), so the helper finds
// the damage itself. Each row is cut into XHD_TILES tiles; a tile's hash is of its pixels' B,G,R bytes (not X) as a
// stream, so the same picture in another surface (the compositor rotates three, scattered differently over RAM) hashes
// the same. `shown` holds the hashes of what the layer shows, valid once a whole frame was posted.
#define XHD_TILES 4
struct xhd_damage { uint32_t width, height, tile; uint64_t *shown, *next; int valid; };
int  xhd_damage_init(struct xhd_damage *d, uint32_t width, uint32_t height);   // 0 or -ENOMEM
void xhd_damage_free(struct xhd_damage *d);
// Hashes the pending frame (all of it) and compares: 0 with *dirty the bounding rectangle of the tiles that differ from
// what is shown (w == 0: the frame is what the layer already shows; nothing to post), or -errno (as xhd_copy_direct).
int  xhd_direct_damage(const struct xhd_direct *dir, struct xhd_damage *d, struct xhd_rect *dirty);
// The frame just hashed is in the layer now (posted whole or its damage over the rest).
void xhd_damage_commit(struct xhd_damage *d);
uint64_t xhd_hash_pixels(uint64_t state[4], const uint8_t *p, size_t bytes, size_t at);   // (for the tests)
#define XHD_DONE_FAILED 1u
#define XHD_DONE_UNCHANGED 2u                         // the frame was what the layer showed: nothing was posted
int xhd_send_done(int fd, uint64_t seq, uint32_t flags);
// Receive the hello and map the frame. 0 or -errno.
int xhd_recv_hello(int fd, struct xhd_frame *frame, int timeout_ms);
void xhd_frame_close(struct xhd_frame *frame);
// Read every pending flush without blocking, adding each (clipped to the frame) to *damage.
// 0 the socket is open, -1 it closed.
int xhd_drain(int fd, const struct xhd_frame *frame, struct xhd_rect *damage);
// The same, and with `direct` (may be NULL: those messages are then ignored) the "memory" and "frame" messages too.
int xhd_drain_direct(int fd, const struct xhd_frame *frame, struct xhd_rect *damage, struct xhd_direct *direct);

int xhd_rect_clip(struct xhd_rect *r, uint32_t width, uint32_t height);   // 0 when nothing is left
void xhd_rect_union(struct xhd_rect *into, const struct xhd_rect *r);

// The largest rectangle with the source's aspect inside dw x dh, centred.
struct xhd_fit { uint32_t x, y, w, h; };
void xhd_fit(uint32_t sw, uint32_t sh, uint32_t dw, uint32_t dh, struct xhd_fit *fit);

// Copy rectangle r of a B,G,R,X frame into a buffer of the same size. swap: the buffer is R,G,B,X
// (alpha written as 0xff); else the bytes are copied as they are.
void xhd_copy(uint8_t *dst, size_t dst_stride, const uint8_t *src, size_t src_stride, struct xhd_rect r, int swap);

// The serving side (xnuhost's), for displayctl and the tests.
int xhd_listen(const char *path);                                        // fd or -errno
int xhd_send_hello(int fd, uint32_t width, uint32_t height, uint32_t stride, int memfd);
int xhd_send_flush(int fd, uint32_t x, uint32_t y, uint32_t w, uint32_t h);
int xhd_memfd(const char *name, size_t bytes);                           // fd or -errno
#endif
