// SPDX-License-Identifier: MIT
// virtio-blk device (VirtIO 1.2, 5.2) over blk.c. A read-only disk offers VIRTIO_BLK_F_RO, which
// NativeVirtioBlock requires of the verified images.
//
// Requests are served inside the guest's notify, on the vCPU that wrote the register (OneiOSHostedDisk polls for the
// answer), except a flush of a writable disk when the driver accepted F_LATE_FLUSH (OneiOS's own bit, the top device
// bit, which virtio-blk does not use): then the disk's flush thread runs the fdatasync(2) and completes the request
// from there, and the driver waits for it asleep (fix-widget-kill-loop, 2026-11-15). Before, the vCPU stayed in the
// host until the phone's file system had written the Data store out: every APFS transaction, every F_FULLFSYNC and
// F_BARRIERFSYNC of iOS (SQLite, plists) held a vCPU, with the guest's timers and IPIs on it waiting.
// Config blk-late-flush=0 does not offer the bit (the flush on the vCPU, as before).
//
// A lasting line per disk, once a minute while it is used (docs/booter.md "Disks"):
//   disk N: 60 s: reads R (M MiB, T ms, slowest S ms), writes W (M MiB, T ms, slowest S ms), flushes F (on its thread:
//   T ms, mean A ms, slowest S ms)
// The read and write times are what the vCPUs spent in the host; the flush time is the fdatasync's.
#define _GNU_SOURCE
#include "xnuhost.h"
#include <errno.h>
#include <pthread.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

#define F_RO (1ull << 5)
#define F_BLK_SIZE (1ull << 6)
#define F_FLUSH (1ull << 9)
#define F_LATE_FLUSH (1ull << 23)
enum { T_IN = 0, T_OUT = 1, T_FLUSH = 4, T_GET_ID = 8 };
enum { S_OK = 0, S_IOERR = 1, S_UNSUPP = 2 };

bool xh_blk_late_flush = true;

typedef struct {
  uint64_t reads, writes, flushes, read_bytes, write_bytes;
  uint64_t read_ns, write_ns, flush_ns, read_max_ns, write_max_ns, flush_max_ns;
} blk_stats;

typedef struct {
  xh_blk *disk;
  xh_virtio *dev;
  unsigned index;
  xh_virtio_ops ops;
  // The flush thread (late flushes): one request at a time, as the driver sends them.
  pthread_mutex_t lock;
  pthread_cond_t cond;
  pthread_t thread;
  bool thread_up, pending;
  uint16_t head;
  uint8_t *status;
  uint32_t generation;                    // a reset drops a flush in progress (it is not completed into the new queue)
  // The minute's counts (under lock).
  blk_stats s;
  uint64_t window_start;
} blk_dev;

static uint64_t now_ns(void)
{
  struct timespec ts;
  clock_gettime(CLOCK_MONOTONIC, &ts);
  return (uint64_t)ts.tv_sec * 1000000000ull + (uint64_t)ts.tv_nsec;
}

static double ms(uint64_t ns) { return (double)ns / 1e6; }

// Adds one request's time to the minute and writes the line when the minute is over. Called with b->lock held.
static void account(blk_dev *b, unsigned type, uint64_t bytes, uint64_t ns, uint64_t at)
{
  blk_stats *s = &b->s;
  if (type == T_IN) { s->reads++; s->read_bytes += bytes; s->read_ns += ns; if (ns > s->read_max_ns) s->read_max_ns = ns; }
  else if (type == T_OUT) { s->writes++; s->write_bytes += bytes; s->write_ns += ns; if (ns > s->write_max_ns) s->write_max_ns = ns; }
  else if (type == T_FLUSH) { s->flushes++; s->flush_ns += ns; if (ns > s->flush_max_ns) s->flush_max_ns = ns; }
  if (!b->window_start) { b->window_start = at; return; }
  if (at - b->window_start < 60000000000ull) return;
  bool late = (xh_virtio_driver_features(b->dev) & F_LATE_FLUSH) != 0;
  LOGI("disk %u: %.0f s: reads %llu (%.1f MiB, %.0f ms, slowest %.1f ms), writes %llu (%.1f MiB, %.0f ms, slowest %.1f ms), "
       "flushes %llu (%s: %.0f ms, mean %.2f ms, slowest %.1f ms)", b->index, (double)(at - b->window_start) / 1e9,
       (unsigned long long)s->reads, (double)s->read_bytes / 1048576.0, ms(s->read_ns), ms(s->read_max_ns),
       (unsigned long long)s->writes, (double)s->write_bytes / 1048576.0, ms(s->write_ns), ms(s->write_max_ns),
       (unsigned long long)s->flushes, late ? "on its thread" : "on the vCPU", ms(s->flush_ns),
       s->flushes ? ms(s->flush_ns) / (double)s->flushes : 0.0, ms(s->flush_max_ns));
  memset(s, 0, sizeof *s);
  b->window_start = at;
}

static void *flush_thread(void *opaque)
{
  blk_dev *b = opaque;
  pthread_mutex_lock(&b->lock);
  for (;;) {
    while (!b->pending) pthread_cond_wait(&b->cond, &b->lock);
    uint16_t head = b->head;
    uint8_t *status = b->status;
    uint32_t generation = b->generation;
    pthread_mutex_unlock(&b->lock);
    uint64_t began = now_ns();
    int rc = xh_blk_flush(b->disk);
    uint64_t done = now_ns();
    // The completion goes under the device's lock, as a vCPU's would: a reset between the take and here dropped it.
    xh_virtio_lock(b->dev);
    pthread_mutex_lock(&b->lock);
    b->pending = false;
    account(b, T_FLUSH, 0, done - began, done);
    if (generation == b->generation) {
      status[0] = rc ? S_IOERR : S_OK;
      pthread_mutex_unlock(&b->lock);
      xh_virtio_push(b->dev, 0, head, 1);
      pthread_mutex_lock(&b->lock);
    }
    xh_virtio_unlock(b->dev);
  }
  return NULL;
}

// A flush for the flush thread. Returns false when there is no thread (the caller flushes itself).
static bool late_flush(blk_dev *b, uint16_t head, uint8_t *status)
{
  pthread_mutex_lock(&b->lock);
  if (!b->thread_up) {
    pthread_attr_t attr;
    pthread_attr_init(&attr);
    pthread_attr_setstacksize(&attr, 64 << 10);
    b->thread_up = pthread_create(&b->thread, &attr, flush_thread, b) == 0;
    pthread_attr_destroy(&attr);
    if (b->thread_up) pthread_setname_np(b->thread, "xh-blk-flush");
    else LOGE("disk %u: no flush thread (%s): flushes stay on the vCPU", b->index, strerror(errno));
  }
  bool queued = b->thread_up && !b->pending;
  if (queued) {
    b->pending = true;
    b->head = head;
    b->status = status;
    pthread_cond_signal(&b->cond);
  }
  pthread_mutex_unlock(&b->lock);
  return queued;
}

static int64_t blk_handle(xh_virtio *dev, unsigned queue, uint16_t head, xh_viov *iov, unsigned n)
{
  (void)queue;
  blk_dev *b = xh_virtio_opaque(dev);
  xh_blk *disk = b->disk;
  struct { uint32_t type, reserved; uint64_t sector; } req;
  // header (readable, 16 bytes), data parts, status (writable, 1 byte, last)
  if (n < 2 || iov[0].writable || iov[0].length != sizeof req || !iov[n - 1].writable || iov[n - 1].length < 1) return 0;
  memcpy(&req, iov[0].data, sizeof req);
  if (req.type == T_FLUSH && (xh_virtio_driver_features(dev) & F_LATE_FLUSH) && late_flush(b, head, iov[n - 1].data))
    return -1;                                         // completed by the flush thread
  uint64_t began = now_ns();
  uint8_t status = S_OK;
  uint64_t offset = req.sector * 512, written = 0;
  if (req.sector > UINT64_MAX / 512) status = S_IOERR;
  for (unsigned i = 1; i + 1 < n && status == S_OK; i++) {
    switch (req.type) {
    case T_IN:
      if (!iov[i].writable || xh_blk_read(disk, offset, iov[i].data, iov[i].length)) status = S_IOERR;
      else written += iov[i].length;
      break;
    case T_OUT:
      if (iov[i].writable || xh_blk_write(disk, offset, iov[i].data, iov[i].length)) status = S_IOERR;
      break;
    case T_GET_ID:
      if (iov[i].writable && iov[i].length >= 20) { memset(iov[i].data, 0, 20); memcpy(iov[i].data, "oneios-xnuhost", 14); written += 20; }
      break;
    default: status = S_UNSUPP;
    }
    offset += iov[i].length;
  }
  if (req.type == T_FLUSH) status = xh_blk_flush(disk) ? S_IOERR : S_OK;
  else if (req.type != T_IN && req.type != T_OUT && req.type != T_GET_ID) status = S_UNSUPP;
  uint64_t done = now_ns();
  pthread_mutex_lock(&b->lock);
  account(b, req.type, offset - req.sector * 512, done - began, done);
  pthread_mutex_unlock(&b->lock);
  // Debugging: XNUHOST_BLKLOG=1 logs every request (which disk, what, where, how it ended).
  static int trace = -1;
  if (trace < 0) trace = getenv("XNUHOST_BLKLOG") != NULL;
  if (trace) LOGI("blk %p: type %u sector %llu, %u parts, %llu bytes, status %u", (void *)disk, req.type, (unsigned long long)req.sector, n - 2, (unsigned long long)(offset - req.sector * 512), status);
  iov[n - 1].data[0] = status;
  return (int64_t)written + 1;
}

static uint32_t blk_config(xh_virtio *dev, uint32_t offset, unsigned size)
{
  struct { uint64_t capacity; uint32_t size_max, seg_max; uint8_t geometry[4]; uint32_t blk_size; uint8_t rest[40]; } c;
  memset(&c, 0, sizeof c);
  c.capacity = xh_blk_bytes(((blk_dev *)xh_virtio_opaque(dev))->disk) / 512;
  c.seg_max = 62;
  c.blk_size = 512;
  uint32_t v = 0;
  if (offset < sizeof c && size <= sizeof c - offset) memcpy(&v, (uint8_t *)&c + offset, size < 4 ? size : 4);
  return v;
}

// A reset (status 0, under the device's lock) drops a late flush in progress: its completion would land in the
// driver's new queue.
static void blk_reset(xh_virtio *dev)
{
  blk_dev *b = xh_virtio_opaque(dev);
  pthread_mutex_lock(&b->lock);
  b->generation++;
  pthread_mutex_unlock(&b->lock);
}

xh_virtio *xh_virtio_blk_create(uint64_t base, unsigned spi, xh_blk *disk)
{
  static unsigned count;
  blk_dev *b = calloc(1, sizeof *b);
  if (!b) return NULL;
  b->disk = disk;
  b->index = count++;
  pthread_mutex_init(&b->lock, NULL);
  pthread_cond_init(&b->cond, NULL);
  bool writable = xh_blk_writable(disk);
  b->ops = (xh_virtio_ops){ .device_id = 2, .features = (writable ? 0 : F_RO) | F_BLK_SIZE | F_FLUSH | (writable && xh_blk_late_flush ? F_LATE_FLUSH : 0),
                            .nqueues = 1, .handle = blk_handle, .config_read = blk_config, .reset = blk_reset };
  b->dev = xh_virtio_create(base, spi, &b->ops, b, "virtio-blk");
  if (!b->dev) { free(b); return NULL; }
  return b->dev;
}
