// SPDX-License-Identifier: MIT
// Disks backed by the image set's files. The role of UniXNU's RamBlock and VirtioMmioBlock back ends
// (https://github.com/UniXNU/RamBlock 60a54ce, https://github.com/UniXNU/VirtioMmioBlock 4e1013e):
// there the guest keeps a seed in RAM and changed pages in a store window; here Linux's page cache
// and a sparse file do both, so the store is simply a raw image the size of the Data disk.
//   read-only: os.img, systemos.img, app.img (raw APFS containers), opened O_RDONLY.
//   writable:  <data>/data.img, created sparse on the first start. A seed that is itself a disk image
//              (not a tar) is copied in, holes kept; a tar seed is the userland's to unpack.
#define _GNU_SOURCE
#include "xnuhost.h"
#include "imageset.h"
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <time.h>
#include <unistd.h>

// The counters of a disk: what the guest asked of it, and its flushes with their time. A writable disk's flush
// (VIRTIO_BLK_T_FLUSH: APFS's barriers and every fsync, SCPreferences' plist commits among them) is an fdatasync of
// the whole image, which runs inside the guest's notify while the guest's disk driver holds its one gate
// (kernel/patches/oneios-hosted/storage): every other request of the guest to that disk waits behind it. On the
// phone (8644ae9, 2026-11-15) configd held NetworkInterfaces.plist's lock for 6.4 s around one commit; these say
// how much of that, and of the boot, is flush time.
struct xh_blk {
  int fd; uint64_t bytes; bool writable; xh_verify *verify;
  char label[24];
  uint64_t reads, writes, read_bytes, write_bytes, flushes, flush_ns, flush_max_ns, slow;   // atomics
  uint64_t shown[6];                  // the counters at the last report
};

#define XH_MAX_BLK 8
static xh_blk *all_disks[XH_MAX_BLK];
static unsigned n_disks;

static uint64_t now_ns(void)
{
  struct timespec t;
  clock_gettime(CLOCK_MONOTONIC, &t);
  return (uint64_t)t.tv_sec * 1000000000ull + (uint64_t)t.tv_nsec;
}

static void register_disk(xh_blk *b, const xh_disk_config *config)
{
  const char *base = strrchr(config->path, '/');
  const char *name = config->data_store ? "data" : base ? base + 1 : config->path;
  size_t length = strlen(name) < sizeof b->label - 1 ? strlen(name) : sizeof b->label - 1;
  memcpy(b->label, name, length);
  b->label[length] = 0;
  if (n_disks < XH_MAX_BLK) all_disks[n_disks++] = b;
}

#define DATA_DEFAULT_BYTES (32ull << 30)      // sparse: costs what the guest writes

static bool is_tar(int fd)
{
  char magic[6] = { 0 };
  return pread(fd, magic, 5, 257) == 5 && !memcmp(magic, "ustar", 5);
}

// Copies a seed image into a new store, skipping blocks of zeros so the store stays sparse.
static int copy_seed(int from, int to, uint64_t bytes)
{
  enum { CHUNK = 1 << 20 };
  static const char zeros[4096];
  char *buffer = malloc(CHUNK);
  if (!buffer) return -1;
  int rc = 0;
  for (uint64_t at = 0; at < bytes && !rc;) {
    ssize_t n = pread(from, buffer, CHUNK, (off_t)at);
    if (n <= 0) { rc = -1; break; }
    for (ssize_t b = 0; b < n; b += 4096) {
      size_t len = (size_t)(n - b < 4096 ? n - b : 4096);
      if (!memcmp(buffer + b, zeros, len)) continue;
      if (pwrite(to, buffer + b, len, (off_t)(at + (uint64_t)b)) != (ssize_t)len) { rc = -1; break; }
    }
    at += (uint64_t)n;
  }
  free(buffer);
  return rc;
}

xh_blk *xh_blk_open(const xh_disk_config *config, const char **error)
{
  struct stat st;
  xh_blk *b = calloc(1, sizeof *b);
  if (!b) { *error = "out of memory"; return NULL; }
  b->fd = -1;
  b->writable = config->writable;
  if (!config->writable) {
    b->fd = open(config->path, O_RDONLY | O_CLOEXEC);
    if (b->fd < 0 || fstat(b->fd, &st)) { *error = "cannot open the image"; goto bad; }
  } else {
    b->fd = open(config->path, O_RDWR | O_CLOEXEC);
    if (b->fd < 0 && errno == ENOENT) {
      // First start: a new store, written under a temporary name so a crash leaves no half store.
      char temporary[600];
      snprintf(temporary, sizeof temporary, "%s.new", config->path);
      int fd = open(temporary, O_RDWR | O_CREAT | O_TRUNC | O_CLOEXEC, 0600);
      if (fd < 0) { *error = "cannot create the Data store"; goto bad; }
      uint64_t bytes = DATA_DEFAULT_BYTES;
      int seed = *config->seed ? open(config->seed, O_RDONLY | O_CLOEXEC) : -1;
      bool image = seed >= 0 && !is_tar(seed) && fstat(seed, &st) == 0 && st.st_size >= 4096;
      if (image && (uint64_t)st.st_size > bytes) bytes = (uint64_t)st.st_size;
      int rc = ftruncate(fd, (off_t)bytes);
      if (!rc && image) rc = copy_seed(seed, fd, (uint64_t)st.st_size);
      if (!rc) rc = fsync(fd);
      if (seed >= 0) close(seed);
      if (rc || rename(temporary, config->path)) { close(fd); unlink(temporary); *error = "cannot fill the new Data store"; goto bad; }
      b->fd = fd;
    }
    if (b->fd < 0 || fstat(b->fd, &st)) { *error = "cannot open the Data store"; goto bad; }
  }
  b->bytes = (uint64_t)st.st_size & ~511ull;
  if (b->bytes == 0) { *error = "the image is empty"; goto bad; }
  register_disk(b, config);
  return b;
bad:
  if (b->fd >= 0) close(b->fd);
  free(b);
  return NULL;
}

uint64_t xh_blk_bytes(const xh_blk *b) { return b->bytes; }
bool xh_blk_writable(const xh_blk *b) { return b->writable; }

// A read-only image whose chunks are checked against the image set's hash list before the guest
// gets them (imageset.c). A chunk that differs is a read error.
void xh_blk_set_verify(xh_blk *b, xh_verify *verify) { if (!b->writable) b->verify = verify; }

int xh_blk_read(xh_blk *b, uint64_t offset, void *buffer, uint64_t length)
{
  if (offset > b->bytes || length > b->bytes - offset) return -1;
  if (b->verify && xh_verify_range(b->verify, b->fd, offset, length)) return -1;
  for (uint64_t done = 0; done < length;) {
    ssize_t n = pread(b->fd, (char *)buffer + done, length - done, (off_t)(offset + done));
    if (n < 0 && errno == EINTR) continue;
    if (n <= 0) return -1;
    done += (uint64_t)n;
  }
  __atomic_fetch_add(&b->reads, 1, __ATOMIC_RELAXED);
  __atomic_fetch_add(&b->read_bytes, length, __ATOMIC_RELAXED);
  return 0;
}

int xh_blk_write(xh_blk *b, uint64_t offset, const void *buffer, uint64_t length)
{
  if (!b->writable || offset > b->bytes || length > b->bytes - offset) return -1;
  for (uint64_t done = 0; done < length;) {
    ssize_t n = pwrite(b->fd, (const char *)buffer + done, length - done, (off_t)(offset + done));
    if (n < 0 && errno == EINTR) continue;
    if (n <= 0) return -1;
    done += (uint64_t)n;
  }
  __atomic_fetch_add(&b->writes, 1, __ATOMIC_RELAXED);
  __atomic_fetch_add(&b->write_bytes, length, __ATOMIC_RELAXED);
  return 0;
}

int xh_blk_flush(xh_blk *b)
{
  if (!b->writable) return 0;
  uint64_t t0 = now_ns();
  int rc = fdatasync(b->fd);
  uint64_t ns = now_ns() - t0;
  __atomic_fetch_add(&b->flushes, 1, __ATOMIC_RELAXED);
  __atomic_fetch_add(&b->flush_ns, ns, __ATOMIC_RELAXED);
  uint64_t max = __atomic_load_n(&b->flush_max_ns, __ATOMIC_RELAXED);
  while (ns > max && !__atomic_compare_exchange_n(&b->flush_max_ns, &max, ns, true, __ATOMIC_RELAXED, __ATOMIC_RELAXED)) {}
  // The first slow ones as they happen, with the time: every other request of the guest to this disk waited.
  if (ns >= 500000000ull && __atomic_fetch_add(&b->slow, 1, __ATOMIC_RELAXED) < 16)
    LOGI("disk %s: a flush took %llu ms (fdatasync; the guest's other requests to this disk waited)", b->label, (unsigned long long)(ns / 1000000));
  return rc;
}

// One line per disk that had requests since the last report (and at the stop, since the start):
//   disk data: 30 s: reads 1200 (45 MiB), writes 300 (20 MiB), flushes 40 in 1234 ms (max 456 ms)
static void report(bool since_start, double seconds)
{
  for (unsigned i = 0; i < n_disks; i++) {
    xh_blk *b = all_disks[i];
    uint64_t v[6] = { __atomic_load_n(&b->reads, __ATOMIC_RELAXED), __atomic_load_n(&b->read_bytes, __ATOMIC_RELAXED),
                      __atomic_load_n(&b->writes, __ATOMIC_RELAXED), __atomic_load_n(&b->write_bytes, __ATOMIC_RELAXED),
                      __atomic_load_n(&b->flushes, __ATOMIC_RELAXED), __atomic_load_n(&b->flush_ns, __ATOMIC_RELAXED) };
    uint64_t d[6];
    for (int k = 0; k < 6; k++) d[k] = since_start ? v[k] : v[k] - b->shown[k];
    if (!since_start) memcpy(b->shown, v, sizeof v);
    if (!d[0] && !d[2] && !d[4]) continue;
    uint64_t max = since_start ? 0 : __atomic_exchange_n(&b->flush_max_ns, 0, __ATOMIC_RELAXED);
    char when[24], longest[32] = "";
    if (since_start) snprintf(when, sizeof when, "since the start");
    else { snprintf(when, sizeof when, "%.0f s", seconds); snprintf(longest, sizeof longest, " (max %llu ms)", (unsigned long long)(max / 1000000)); }
    LOGI("disk %s: %s: reads %llu (%llu MiB), writes %llu (%llu MiB), flushes %llu in %llu ms%s", b->label, when,
         (unsigned long long)d[0], (unsigned long long)(d[1] >> 20), (unsigned long long)d[2], (unsigned long long)(d[3] >> 20),
         (unsigned long long)d[4], (unsigned long long)(d[5] / 1000000), longest);
  }
}

void xh_blk_tick(void)
{
  static uint64_t last;
  uint64_t now = now_ns();
  if (!last) { last = now; return; }
  if (now - last < 30000000000ull) return;
  report(false, (double)(now - last) / 1e9);
  last = now;
}

void xh_blk_report_all(void) { report(true, 0); }
uint64_t xh_blk_flushes(xh_blk *b) { return __atomic_load_n(&b->flushes, __ATOMIC_RELAXED); }

void xh_blk_close(xh_blk *b)
{
  if (!b) return;
  if (b->writable) fsync(b->fd);
  for (unsigned i = 0; i < n_disks; i++)
    if (all_disks[i] == b) { all_disks[i] = all_disks[--n_disks]; break; }
  close(b->fd);
  free(b);
}
