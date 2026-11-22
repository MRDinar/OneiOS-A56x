// SPDX-License-Identifier: MIT
// The renderer's C side for Rust (drm2kgsl-sys, src/ffi.rs): only what Rust cannot do itself. virglrenderer's
// contexts and resources are C structures with function pointers (struct virgl_context, struct virgl_resource): their
// fields are read and their methods called here, by id, so no layout of theirs is copied into Rust. And the three
// functions the virglrenderer sources expect of their surroundings take a va_list or are variadic. Everything else that
// src/gpu3d_kgsl.c did (the stats, the GPU's policy, the chunk lists, the checks, the log lines) is Rust's
// (runtime/xnuhost/src/gpu3d_kgsl.rs). Compiled by build.rs with the renderer's own flags.
#include <errno.h>
#include <stdarg.h>
#include <stdbool.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

#include "virgl_context.h"
#include "virgl_resource.h"
#include "virgl_util.h"
#include "virgl_fence.h"
#include "drm_hw.h"
#include "drm2kgsl/drm2kgsl_renderer.h"
#include "oneios_drm2kgsl.h"
#include "oneios_gpu_policy.h"
#include "oneios_glue.h"

static oneios_d2k_line_fn line_sink;
static oneios_d2k_retire_fn retire_sink;

// A line for Rust's log: level 0 error, 1 info, 2 debug, "drm2kgsl: " and the text (at most 511 bytes, as say() did).
static void line(int level, const char *fmt, ...) __attribute__((format(printf, 2, 3)));
static void line(int level, const char *fmt, ...)
{
  char text[512];
  va_list ap;
  va_start(ap, fmt);
  vsnprintf(text, sizeof text, fmt, ap);
  va_end(ap);
  if (line_sink) line_sink(level, text);
}

// ---- what the virglrenderer sources expect of their surroundings --------------------------------------------------
// The library's log (drm_log, drm_err, drm_dbg), drm2kgsl's diagnostics (it writes them to Android's log, which a
// static binary has no library for) and mesa's two OS helpers (os_misc.c wants Android's liblog too).
static void virgl_line(enum virgl_log_level_flags level, const char *message, void *data)
{
  (void)data;
  // Only drm_err (ERROR) is an `xnuhost E` line. Our drm2kgsl patches print their once-per-event facts (a queue's KGSL
  // priority, the power constraint, the clock hold, the import's coherence, the vCPU uclamp) at WARNING so that the
  // library's threshold passes them: they are information, and as `E` lines they hid the real errors (28 of the 28 `E`
  // lines of out/session-logs/2026-11-22/kbd-debug/xnuhost.log were such facts).
  line(level >= VIRGL_LOG_LEVEL_ERROR ? 0 : level >= VIRGL_LOG_LEVEL_INFO ? 1 : 2, "drm2kgsl: %s", message);
}

int __android_log_vprint(int priority, const char *tag, const char *fmt, va_list ap);
int __android_log_vprint(int priority, const char *tag, const char *fmt, va_list ap)
{
  char text[512];
  (void)priority; (void)tag;
  vsnprintf(text, sizeof text, fmt, ap);
  line(2, "drm2kgsl: %s", text);
  return 0;
}

void os_log_message(const char *message);
void os_log_message(const char *message) { line(1, "drm2kgsl: %s", message); }
const char *os_get_option(const char *name);
const char *os_get_option(const char *name) { return getenv(name); }

// ---- the renderer -------------------------------------------------------------------------------------------------
static void fence_retired(struct virgl_context *ctx, uint32_t ring, uint64_t fence)
{
  if (retire_sink) retire_sink(ctx->ctx_id, ring, fence);
}

int oneios_d2k_init(oneios_d2k_line_fn on_line, oneios_d2k_retire_fn on_retire)
{
  line_sink = on_line;
  retire_sink = on_retire;
  virgl_log_set_handler(virgl_line, NULL, NULL);
  return virgl_context_table_init() || virgl_resource_table_init(NULL) || virgl_fence_table_init() ? -ENOMEM : 0;
}

int oneios_d2k_probe(int fd, void *capset, size_t max, struct oneios_d2k_caps *caps)
{
  struct virgl_renderer_capset_drm c;
  memset(&c, 0, sizeof c);
  c.context_type = VIRTGPU_DRM_CONTEXT_MSM;
  if (max < sizeof c) return -EINVAL;
  int r = drm2kgsl_renderer_probe(fd, &c);       // keeps its own descriptor for the VBO
  if (r) return r < 0 ? r : -r;
  memcpy(capset, &c, sizeof c);
  *caps = (struct oneios_d2k_caps){ .chip_id = c.u.msm.chip_id, .gpu_id = c.u.msm.gpu_id, .va_start = c.u.msm.va_start,
                                    .va_size = c.u.msm.va_size, .priorities = c.u.msm.priorities };
  return (int)sizeof c;
}

bool oneios_d2k_context_exists(uint32_t id) { return virgl_context_lookup(id) != NULL; }

int oneios_d2k_context_create(uint32_t id, int fd, const char *name, size_t name_length)
{
  struct virgl_context *ctx = drm2kgsl_renderer_create(fd, name_length, name);        // owns fd, also when it fails
  if (!ctx) return -ENOMEM;
  ctx->ctx_id = id;
  ctx->capset_id = VIRGL_RENDERER_CAPSET_DRM;
  ctx->fence_retire = fence_retired;
  if (virgl_context_add(ctx)) { ctx->destroy(ctx); return -ENOMEM; }
  return 0;
}

void oneios_d2k_context_destroy(uint32_t id) { virgl_context_remove(id); }

int oneios_d2k_submit(uint32_t id, const void *commands, size_t size)
{
  struct virgl_context *ctx = virgl_context_lookup(id);
  if (!ctx) return -EINVAL;
  // drm_context reads the stream as 32-bit words in place: the device's copy is aligned.
  return ctx->submit_cmd(ctx, commands, size) ? -EINVAL : 0;
}

int oneios_d2k_submit_fence(uint32_t id, uint32_t ring, uint64_t fence)
{
  struct virgl_context *ctx = virgl_context_lookup(id);
  if (!ctx) return -EINVAL;
  // Mergeable, as crosvm asks: a ring may retire only its latest fence, and the device returns every earlier one.
  return ctx->submit_fence(ctx, 1 /* VIRGL_RENDERER_FENCE_FLAG_MERGEABLE */, ring, fence);
}

void oneios_d2k_set_class(uint32_t id, unsigned cls, bool lowest)
{
  struct virgl_context *ctx = virgl_context_lookup(id);
  if (ctx) drm2kgsl_renderer_set_class(ctx, cls, lowest);
}

bool oneios_d2k_resource_exists(uint32_t res) { return virgl_resource_lookup(res) != NULL; }

int oneios_d2k_resource_dmabuf(uint32_t res, int fd, uint64_t size)
{
  struct virgl_resource *r = virgl_resource_create_from_fd(res, VIRGL_RESOURCE_FD_DMABUF, fd, NULL, 0, NULL);
  if (!r) return -ENOMEM;                         // fd stays the caller's
  r->map_info = VIRGL_RENDERER_MAP_CACHE_CACHED;
  r->map_size = size;
  return 0;
}

void oneios_d2k_resource_remove(uint32_t res) { virgl_resource_remove(res); }

int oneios_d2k_set_guest_blob_chunks(uint32_t id, uint64_t blob_id, const int *fds, const uint64_t *sizes, unsigned n)
{
  struct virgl_context *ctx = virgl_context_lookup(id);
  if (!ctx) {
    for (unsigned i = 0; i < n; i++) close(fds[i]);   // taken, also when it fails
    return -EINVAL;
  }
  return drm2kgsl_renderer_set_guest_blob_chunks(ctx, blob_id, fds, sizes, n);
}

int oneios_d2k_get_blob(uint32_t id, uint32_t res, uint64_t blob_id, uint64_t size, uint32_t flags)
{
  struct virgl_context *ctx = virgl_context_lookup(id);
  struct virgl_context_blob blob;
  memset(&blob, 0, sizeof blob);
  if (!ctx) return -EINVAL;
  int r = ctx->get_blob(ctx, res, blob_id, size, flags, &blob);
  if (r) return r < 0 ? r : -r;
  if (blob.type != VIRGL_RESOURCE_FD_DMABUF && blob.type != VIRGL_RESOURCE_FD_SHM) {
    // (src/gpu3d_kgsl.c left the fd of such a blob open; none comes from drm2kgsl, but it is closed here.)
    if (blob.type == VIRGL_RESOURCE_FD_OPAQUE && blob.u.fd >= 0) close(blob.u.fd);
    return -EINVAL;
  }
  struct virgl_resource *made = virgl_resource_create_from_fd(res, blob.type, blob.u.fd, NULL, 0, &blob.opaque_fd_metadata);
  if (!made) { close(blob.u.fd); return -ENOMEM; }
  made->map_info = blob.map_info;
  made->map_size = size;
  made->map_ptr = blob.map_ptr;
  made->fd_offset = blob.fd_offset;
  return 0;
}

int oneios_d2k_resource_map(uint32_t res, void **ptr, uint64_t *size, uint32_t *map_info)
{
  struct virgl_resource *r = virgl_resource_lookup(res);
  if (!r) return -EINVAL;
  *ptr = r->map_ptr;
  *size = r->map_size;
  *map_info = r->map_info;
  return 0;
}

int oneios_d2k_attach(uint32_t id, uint32_t res, bool on, const int *fds, const uint64_t *sizes, unsigned n)
{
  struct virgl_context *ctx = virgl_context_lookup(id);
  struct virgl_resource *r = virgl_resource_lookup(res);
  if (!ctx || !r) return -EINVAL;
  if (on && n) {
    // A resource of several udmabufs: the context imports each (nothing, when it made the resource itself).
    int error = drm2kgsl_renderer_attach_chunks(ctx, res, fds, sizes, n);
    if (error) return error;
  }
  if (on) ctx->attach_resource(ctx, r); else ctx->detach_resource(ctx, r);
  return 0;
}

static bool detach_one(struct virgl_context *ctx, void *data)
{
  ctx->detach_resource(ctx, data);
  return true;
}

void oneios_d2k_unref(uint32_t res)
{
  struct virgl_resource *r = virgl_resource_lookup(res);
  if (!r) return;
  struct virgl_context_foreach_args args = { .callback = detach_one, .data = r };
  virgl_context_foreach(&args);
  virgl_resource_remove(res);
}

// The two parts of oneios_gpu_policy.h the patched renderer uses itself, for the Rust test that checks Rust's policy
// (runtime/xnuhost/src/gpu3d_kgsl/policy.rs) says the same.
unsigned oneios_d2k_policy_priority(unsigned cls) { return og_kgsl_priority((enum og_class)cls); }
const char *oneios_d2k_policy_class_name(unsigned cls) { return og_class_name((enum og_class)cls); }
