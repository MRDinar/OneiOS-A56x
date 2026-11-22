// SPDX-License-Identifier: MIT
// virtio-mmio transport, version 2 (VirtIO 1.2, section 4.2), split virtqueues: the device side of
// what UniXNU's NativeVirtioBlock (https://github.com/UniXNU/VirtioMmioBlock, 4e1013e) and
// UniXNU-droidvm's virtio drivers (package/DroidVMVirtio*.c) program. crosvm is that device side in
// a DroidVM; here it is this file, in the same process as the kernel.
//
// Health (lasting lines, docs/booter.md "Health lines"): each queue counts the chains taken from the guest and returned
// to it; xh_virtio_health prints them every 60 s when they moved, and once per episode a queue that holds chains and
// returned none for a whole span, or (a queue the transport pops itself) whose posted chains nobody took. The driver's
// state changes are one line each: up (DRIVER_OK), FAILED, a reset after it was up.
#include "xnuhost.h"
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

#define MAGIC 0x74726976u
#define F_VERSION_1 (1ull << 32)
#define DESC_NEXT 1u
#define DESC_WRITE 2u
#define DESC_INDIRECT 4u

typedef struct {
  uint32_t num, ready;
  uint64_t desc, avail, used;
  uint16_t last_avail;
} vqueue;

// A queue's counters: kept over a reset (reset() clears vqueue, not these).
typedef struct {
  uint64_t taken, returned, bad;
  uint64_t shown_taken, shown_returned;      // at the last report
  uint64_t quiet_since_ms;                   // when the held (or unread) episode began, 0: none
  bool held_told, unread_told;
} vstats;

struct xh_virtio {
  pthread_mutex_t lock;
  const xh_virtio_ops *ops;
  void *opaque;
  const char *name;
  char label[24];                            // "gpu#0": the name without "virtio-", the index among its kind
  unsigned spi;
  uint32_t status, isr, features_sel, driver_sel, queue_sel, generation;
  uint64_t driver_features;
  vqueue q[XH_VIRTIO_QUEUES];
  vstats st[XH_VIRTIO_QUEUES];
};

// Every device, for the health report. Made at the boot, from one thread; never freed.
#define MAX_DEVICES 32
static xh_virtio *all_devices[MAX_DEVICES];
static unsigned n_devices;

// The lines go to xh_virtio_log (xnuhost sets it to its log; NULL: none). virtio.c is also linked without xnuhost's log
// (the bridges' tests, encctl), so it never calls xh_log itself.
xh_line_fn xh_virtio_log;
__attribute__((format(printf, 1, 2))) static void say(const char *fmt, ...)
{
  if (!xh_virtio_log) return;
  char line[256];
  va_list ap;
  va_start(ap, fmt);
  vsnprintf(line, sizeof line, fmt, ap);
  va_end(ap);
  xh_virtio_log(NULL, line);
}

void *(*xh_virtio_translate)(uint64_t pa, uint64_t len) = xh_ram;

void *xh_virtio_opaque(xh_virtio *d) { return d->opaque; }
void xh_virtio_lock(xh_virtio *d) { pthread_mutex_lock(&d->lock); }
void xh_virtio_unlock(xh_virtio *d) { pthread_mutex_unlock(&d->lock); }

static void raise_irq(xh_virtio *d, uint32_t bits)
{
  d->isr |= bits;
  xh_gic_set_spi(d->spi, true);
}

static void reset(xh_virtio *d)
{
  if (d->status & 4) say("virtio %s: reset by the guest's driver (it was up)", d->label);
  for (unsigned i = 0; i < XH_VIRTIO_QUEUES; i++) {      // what was in flight is the reset's: not held any more
    vstats *s = &d->st[i];
    s->taken = s->shown_taken = s->returned;
    s->shown_returned = s->returned;
    s->quiet_since_ms = 0;
    s->held_told = s->unread_told = false;
  }
  d->status = d->isr = d->features_sel = d->driver_sel = d->queue_sel = 0;
  d->driver_features = 0;
  memset(d->q, 0, sizeof d->q);
  xh_gic_set_spi(d->spi, false);
  if (d->ops->reset) d->ops->reset(d);
}

// Reads the chain starting at `head` into iov. Returns the count of parts, 0 for a bad chain.
static unsigned chain(xh_virtio *d, vqueue *q, uint16_t head, xh_viov *iov, unsigned max)
{
  struct desc { uint64_t addr; uint32_t len; uint16_t flags, next; } *table, e;
  unsigned n = 0, seen = 0, count = q->num;
  bool indirect = false;
  (void)d;
  table = xh_virtio_translate(q->desc, 16ull * q->num);
  if (!table) return 0;
  uint16_t i = head;
  for (;;) {
    if (i >= count || ++seen > count) return 0;
    memcpy(&e, &table[i], sizeof e);
    if (e.flags & DESC_INDIRECT) {
      if (indirect || (e.len % 16) || e.len == 0) return 0;      // one level only
      indirect = true;
      count = e.len / 16;
      table = xh_virtio_translate(e.addr, e.len);
      if (!table) return 0;
      i = 0; seen = 0;
      continue;
    }
    if (n == max) return 0;
    iov[n].data = e.len ? xh_virtio_translate(e.addr, e.len) : NULL;
    if (e.len && !iov[n].data) return 0;
    iov[n].length = e.len;
    iov[n].writable = e.flags & DESC_WRITE;
    n++;
    if (!(e.flags & DESC_NEXT)) break;
    i = e.next;
  }
  return n;
}

static bool pop(xh_virtio *d, unsigned qi, uint16_t *head, xh_viov *iov, unsigned max, unsigned *n)
{
  vqueue *q = &d->q[qi];
  if (!q->ready || !q->num) return false;
  uint16_t *avail = xh_virtio_translate(q->avail, 4 + 2ull * q->num);
  if (!avail) return false;
  uint16_t idx = __atomic_load_n(&avail[1], __ATOMIC_ACQUIRE);
  if (idx == q->last_avail) return false;
  *head = avail[2 + q->last_avail % q->num];
  q->last_avail++;
  *n = chain(d, q, *head, iov, max);
  d->st[qi].taken++;
  if (!*n) d->st[qi].bad++;
  return true;
}

static void push(xh_virtio *d, unsigned qi, uint16_t head, uint32_t written)
{
  vqueue *q = &d->q[qi];
  uint16_t *used = xh_virtio_translate(q->used, 4 + 8ull * q->num);
  if (!used) return;
  uint32_t *ring = (uint32_t *)(used + 2);
  uint16_t idx = used[1];
  ring[2 * (idx % q->num)] = head;
  ring[2 * (idx % q->num) + 1] = written;
  __atomic_store_n(&used[1], (uint16_t)(idx + 1), __ATOMIC_RELEASE);
  d->st[qi].returned++;
  raise_irq(d, 1);
}

unsigned xh_virtio_pop(xh_virtio *d, unsigned qi, uint16_t *head, xh_viov *iov, unsigned max)
{
  unsigned n = 0;
  pthread_mutex_lock(&d->lock);
  bool got = qi < XH_VIRTIO_QUEUES && (d->status & 4) && pop(d, qi, head, iov, max, &n);
  if (got && n == 0) push(d, qi, *head, 0);          // a bad chain is returned unused
  pthread_mutex_unlock(&d->lock);
  return got ? n : 0;
}

void xh_virtio_push(xh_virtio *d, unsigned qi, uint16_t head, uint32_t written)
{
  pthread_mutex_lock(&d->lock);
  push(d, qi, head, written);
  pthread_mutex_unlock(&d->lock);
}

uint64_t xh_virtio_driver_features(xh_virtio *d)
{
  pthread_mutex_lock(&d->lock);
  uint64_t features = d->driver_features;
  pthread_mutex_unlock(&d->lock);
  return features;
}

bool xh_virtio_ready(xh_virtio *d)
{
  pthread_mutex_lock(&d->lock);
  bool ready = d->status & 4;
  pthread_mutex_unlock(&d->lock);
  return ready;
}

void xh_virtio_config_changed(xh_virtio *d)
{
  pthread_mutex_lock(&d->lock);
  d->generation++;
  raise_irq(d, 2);
  pthread_mutex_unlock(&d->lock);
}

static void notify(xh_virtio *d, unsigned qi)
{
  xh_viov iov[64];
  uint16_t head;
  unsigned n;
  if (qi >= d->ops->nqueues || !(d->status & 4)) return;
  if (d->ops->manual & (1u << qi)) { if (d->ops->kick) d->ops->kick(d, qi); return; }
  while (pop(d, qi, &head, iov, 64, &n)) {
    int64_t written = n ? d->ops->handle(d, qi, head, iov, n) : 0;
    if (written >= 0) push(d, qi, head, (uint32_t)written);
  }
}

uint64_t xh_virtio_mmio_read(xh_virtio *d, uint64_t off, unsigned size)
{
  uint64_t v = 0;
  pthread_mutex_lock(&d->lock);
  vqueue *q = &d->q[d->queue_sel < XH_VIRTIO_QUEUES ? d->queue_sel : 0];
  if (off >= 0x100) {
    v = d->ops->config_read ? d->ops->config_read(d, (uint32_t)(off - 0x100), size) : 0;
    pthread_mutex_unlock(&d->lock);
    return v;
  }
  switch (off) {
  case 0x000: v = MAGIC; break;
  case 0x004: v = 2; break;
  case 0x008: v = d->ops->device_id; break;
  case 0x00c: v = 0x4f694f53; break;                 // vendor: "SOiO"
  case 0x010: v = d->features_sel == 0 ? (uint32_t)d->ops->features :
                  d->features_sel == 1 ? (uint32_t)((d->ops->features | F_VERSION_1) >> 32) : 0; break;
  case 0x034: v = d->queue_sel < d->ops->nqueues ? XH_VIRTIO_QSIZE : 0; break;
  case 0x044: v = q->ready; break;
  case 0x060: v = d->isr; break;
  case 0x070: v = d->status; break;
  case 0x0fc: v = d->generation; break;
  }
  pthread_mutex_unlock(&d->lock);
  return v;
}

static void set_half(uint64_t *value, bool high, uint32_t half)
{
  *value = high ? (*value & 0xffffffffull) | (uint64_t)half << 32 : (*value & ~0xffffffffull) | half;
}

void xh_virtio_mmio_write(xh_virtio *d, uint64_t off, unsigned size, uint64_t value)
{
  uint32_t v = (uint32_t)value;
  pthread_mutex_lock(&d->lock);
  vqueue *q = &d->q[d->queue_sel < XH_VIRTIO_QUEUES ? d->queue_sel : 0];
  if (off >= 0x100) {
    if (d->ops->config_write) d->ops->config_write(d, (uint32_t)(off - 0x100), size, v);
    pthread_mutex_unlock(&d->lock);
    return;
  }
  switch (off) {
  case 0x014: d->features_sel = v; break;
  case 0x020: if (d->driver_sel < 2) set_half(&d->driver_features, d->driver_sel, v); break;
  case 0x024: d->driver_sel = v; break;
  case 0x030: d->queue_sel = v; break;
  case 0x038: if (v && v <= XH_VIRTIO_QSIZE && !(v & (v - 1))) q->num = v; break;
  case 0x044: q->ready = v & 1; if (!q->ready) q->last_avail = 0; break;
  case 0x050: notify(d, v); break;
  case 0x064: d->isr &= ~v; if (!d->isr) xh_gic_set_spi(d->spi, false); break;
  case 0x070:
    if (v == 0) reset(d);
    else {
      // FEATURES_OK is refused unless the driver accepted VERSION_1 and nothing the device lacks.
      if ((v & 8) && (!(d->driver_features & F_VERSION_1) || (d->driver_features & ~(d->ops->features | F_VERSION_1)))) v &= ~8u;
      if ((v & 4) && !(d->status & 4))
        say("virtio %s: the guest's driver is up (features 0x%llx, %u queue(s) ready)", d->label, (unsigned long long)d->driver_features,
             (d->q[0].ready ? 1u : 0u) + (d->q[1].ready ? 1u : 0u) + (d->q[2].ready ? 1u : 0u) + (d->q[3].ready ? 1u : 0u));
      if ((v & 128) && !(d->status & 128)) say("virtio %s: the guest's driver set FAILED (status 0x%x)", d->label, v);
      d->status = v;
    }
    break;
  case 0x080: set_half(&q->desc, false, v); break;
  case 0x084: set_half(&q->desc, true, v); break;
  case 0x090: set_half(&q->avail, false, v); break;
  case 0x094: set_half(&q->avail, true, v); break;
  case 0x0a0: set_half(&q->used, false, v); break;
  case 0x0a4: set_half(&q->used, true, v); break;
  }
  pthread_mutex_unlock(&d->lock);
}

static uint64_t region_read(void *opaque, uint64_t off, unsigned size) { return xh_virtio_mmio_read(opaque, off, size); }
static void region_write(void *opaque, uint64_t off, unsigned size, uint64_t value) { xh_virtio_mmio_write(opaque, off, size, value); }

xh_virtio *xh_virtio_create(uint64_t base, unsigned spi, const xh_virtio_ops *ops, void *opaque, const char *name)
{
  xh_virtio *d = calloc(1, sizeof *d);
  if (!d) return NULL;
  pthread_mutexattr_t attr;
  pthread_mutexattr_init(&attr);
  pthread_mutexattr_settype(&attr, PTHREAD_MUTEX_RECURSIVE);     // a handler may complete chains itself
  pthread_mutex_init(&d->lock, &attr);
  d->ops = ops;
  d->opaque = opaque;
  d->name = name;
  d->spi = spi;
  if (base && xh_mmio_add(&(xh_mmio_region){ .base = base, .size = 0x1000, .name = name, .opaque = d, .read = region_read, .write = region_write })) {
    free(d);
    return NULL;
  }
  const char *kind = name && !strncmp(name, "virtio-", 7) ? name + 7 : name ? name : "?";
  unsigned index = 0;
  for (unsigned i = 0; i < n_devices; i++) if (!strcmp(all_devices[i]->name ? all_devices[i]->name : "?", name ? name : "?")) index++;
  snprintf(d->label, sizeof d->label, "%s#%u", kind, index);
  if (n_devices < MAX_DEVICES) all_devices[n_devices++] = d;
  return d;
}

// ---- health -------------------------------------------------------------------------------------------------------
// One device's part of the summary line, and its episode lines. d->lock held.
static void device_health(xh_virtio *d, uint64_t now_ms, char *sum, size_t max, size_t *used, xh_line_fn out, void *ctx)
{
  char line[192];
  bool named = false;
  for (unsigned i = 0; i < d->ops->nqueues && i < XH_VIRTIO_QUEUES; i++) {
    vstats *s = &d->st[i];
    vqueue *q = &d->q[i];
    uint64_t returned = s->returned - s->shown_returned, taken = s->taken - s->shown_taken;
    uint64_t in_flight = s->taken > s->returned ? s->taken - s->returned : 0;
    // Chains the guest posted that nobody took: only a queue the transport pops on the guest's kick can have them
    // waiting for no reason (a device's own queue keeps the guest's buffers until it has data).
    unsigned unread = 0;
    if (q->ready && q->num && !(d->ops->manual & (1u << i))) {
      uint16_t *avail = xh_virtio_translate(q->avail, 4 + 2ull * q->num);
      if (avail) unread = (uint16_t)(__atomic_load_n(&avail[1], __ATOMIC_ACQUIRE) - q->last_avail);
    }
    if (returned || taken) {
      int n = snprintf(sum + *used, max - *used, "%s%s q%u +%llu", *used ? (named ? "" : ", ") : "", named ? "" : d->label, i,
                       (unsigned long long)returned);
      if (n > 0 && (size_t)n < max - *used) *used += (size_t)n;
      if (in_flight) {
        n = snprintf(sum + *used, max - *used, " (in flight %llu)", (unsigned long long)in_flight);
        if (n > 0 && (size_t)n < max - *used) *used += (size_t)n;
      }
      named = true;
    }
    // An episode: chains held (or left unread) and nothing returned for a whole span. Told once; and when it moves again.
    bool quiet = !returned && (in_flight || unread);
    if (quiet) {
      if (!s->quiet_since_ms) s->quiet_since_ms = now_ms;
      else if (!s->held_told && in_flight) {
        snprintf(line, sizeof line, "virtio %s: queue %u: %llu chain(s) in flight and none returned in %llu s (taken %llu, returned %llu since the boot)",
                 d->label, i, (unsigned long long)in_flight, (unsigned long long)((now_ms - s->quiet_since_ms) / 1000),
                 (unsigned long long)s->taken, (unsigned long long)s->returned);
        out(ctx, line);
        s->held_told = true;
      } else if (!s->unread_told && unread && !in_flight) {
        snprintf(line, sizeof line, "virtio %s: queue %u: %u chain(s) posted by the guest and not taken in %llu s", d->label, i, unread,
                 (unsigned long long)((now_ms - s->quiet_since_ms) / 1000));
        out(ctx, line);
        s->unread_told = true;
      }
    } else {
      if (s->held_told || s->unread_told) {
        snprintf(line, sizeof line, "virtio %s: queue %u returns chains again (after %llu s)", d->label, i,
                 (unsigned long long)((now_ms - s->quiet_since_ms) / 1000));
        out(ctx, line);
      }
      s->quiet_since_ms = 0;
      s->held_told = s->unread_told = false;
    }
    s->shown_returned = s->returned;
    s->shown_taken = s->taken;
  }
}

void xh_virtio_health(uint64_t now_ms, xh_line_fn out, void *ctx)
{
  static uint64_t last_ms, bad_shown;
  char sum[900];
  size_t used = 0;
  uint64_t bad = 0;
  for (unsigned k = 0; k < n_devices; k++) {
    xh_virtio *d = all_devices[k];
    pthread_mutex_lock(&d->lock);
    device_health(d, now_ms, sum, sizeof sum, &used, out, ctx);
    for (unsigned i = 0; i < XH_VIRTIO_QUEUES; i++) bad += d->st[i].bad;
    pthread_mutex_unlock(&d->lock);
  }
  if (used) {
    char line[1024];
    char bads[48] = "";
    if (bad != bad_shown) snprintf(bads, sizeof bads, "; bad chains +%llu", (unsigned long long)(bad - bad_shown));
    snprintf(line, sizeof line, "virtio: %llu s: %s%s", (unsigned long long)(last_ms && now_ms > last_ms ? (now_ms - last_ms + 500) / 1000 : XH_VIRTIO_REPORT_MS / 1000),
             sum, bads);
    out(ctx, line);
  }
  bad_shown = bad;
  last_ms = now_ms;
}

void xh_virtio_tick(void)
{
  static uint64_t next_ms;
  struct timespec t;
  clock_gettime(CLOCK_MONOTONIC, &t);
  uint64_t now = (uint64_t)t.tv_sec * 1000u + (uint64_t)t.tv_nsec / 1000000u;
  if (!xh_virtio_log) return;
  if (!next_ms) { next_ms = now + XH_VIRTIO_REPORT_MS; xh_virtio_health(now, xh_virtio_log, NULL); return; }   // the baseline
  if (now < next_ms) return;
  next_ms = now + XH_VIRTIO_REPORT_MS;
  xh_virtio_health(now, xh_virtio_log, NULL);
}
