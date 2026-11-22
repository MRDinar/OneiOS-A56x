// SPDX-License-Identifier: MIT
// liboneios-display.so: the native half of OneiOSDisplay (java/OneiOSDisplay.java). It owns the
// socket, the mapped frame and the copy into the layer's buffers; the Java half owns the layers.
#define _GNU_SOURCE
#include "xhd.h"
#include <android/native_window.h>
#include <android/native_window_jni.h>
#include <errno.h>
#include <fcntl.h>
#include <jni.h>
#include <poll.h>
#include <signal.h>
#include <stdbool.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/resource.h>
#include <time.h>
#include <unistd.h>

#define FORMAT_BGRA_8888 XHD_BUFFER_BGRA_8888    // HAL_PIXEL_FORMAT_BGRA_8888: the guest's 8-bit bytes as they are
#define FORMAT_RGBX_8888 XHD_BUFFER_RGBX_8888
#define FORMAT_RGBA_FP16 XHD_BUFFER_RGBA_FP16    // the guest's half floats as they are

// ANativeWindow_setBuffersDataSpace and _get are API 28 and libnativewindow's (the Makefile links it: libandroid does
// not have them); the library is built for 30, so they are there.
struct context { int fd; struct xhd_frame frame; struct xhd_direct direct; };

// SIGTERM, SIGINT and SIGHUP end the run loop through a pipe, so the Java half removes the layers.
static int signal_pipe[2] = { -1, -1 };
static volatile int stopping;
static void on_signal(int number)
{
  int saved = errno;
  char byte = (char)number;
  stopping = 1;
  if (signal_pipe[1] >= 0 && write(signal_pipe[1], &byte, 1) < 0) { }
  errno = saved;
}

JNIEXPORT jlong JNICALL Java_OneiOSDisplay_nOpen(JNIEnv *env, jclass type, jstring socket_path, jint wait_ms)
{
  (void)type;
  if (signal_pipe[0] < 0 && !pipe2(signal_pipe, O_CLOEXEC | O_NONBLOCK)) {
    struct sigaction action = { .sa_handler = on_signal, .sa_flags = SA_RESTART };
    sigemptyset(&action.sa_mask);
    sigaction(SIGTERM, &action, NULL);
    sigaction(SIGINT, &action, NULL);
    sigaction(SIGHUP, &action, NULL);
  }
  const char *path = (*env)->GetStringUTFChars(env, socket_path, NULL);
  if (!path) return 0;
  struct context *c = calloc(1, sizeof *c);
  int fd = c ? xhd_connect(path, wait_ms, &stopping) : -ENOMEM;
  if (fd < 0) fprintf(stderr, "oneios-display: %s: %s\n", path, stopping ? "stopped while waiting" : strerror(-fd));
  (*env)->ReleaseStringUTFChars(env, socket_path, path);
  if (fd < 0) { free(c); return 0; }
  c->fd = fd;
  c->frame.memfd = -1;
  xhd_direct_init(&c->direct);
  return (jlong)(intptr_t)c;
}

// Reports the panel's mode, then takes xnuhost's hello. JNI_FALSE: no usable hello (nClose is still the caller's).
JNIEXPORT jboolean JNICALL Java_OneiOSDisplay_nHello(JNIEnv *env, jclass type, jlong handle, jint width, jint height, jint hz, jboolean direct)
{
  (void)env; (void)type;
  struct context *c = (struct context *)(intptr_t)handle;
  int error = xhd_send_panel(c->fd, (uint32_t)width, (uint32_t)height, (uint32_t)hz, direct ? XHD_PANEL_DIRECT : 0);
  if (error) fprintf(stderr, "oneios-display: the panel's mode could not be sent: %s\n", strerror(-error));
  error = xhd_recv_hello(c->fd, &c->frame, 10000);
  if (error) fprintf(stderr, "oneios-display: no usable hello from xnuhost: %s\n", strerror(-error));
  return error ? JNI_FALSE : JNI_TRUE;
}

JNIEXPORT jintArray JNICALL Java_OneiOSDisplay_nInfo(JNIEnv *env, jclass type, jlong handle)
{
  (void)type;
  struct context *c = (struct context *)(intptr_t)handle;
  jint values[4] = { (jint)c->frame.width, (jint)c->frame.height, (jint)c->frame.stride, (jint)c->frame.format };
  jintArray array = (*env)->NewIntArray(env, 4);
  if (array) (*env)->SetIntArrayRegion(env, array, 0, 4, values);
  return array;
}

JNIEXPORT jintArray JNICALL Java_OneiOSDisplay_nFit(JNIEnv *env, jclass type, jint sw, jint sh, jint dw, jint dh)
{
  (void)type;
  struct xhd_fit fit;
  xhd_fit((uint32_t)sw, (uint32_t)sh, (uint32_t)dw, (uint32_t)dh, &fit);
  jint values[4] = { (jint)fit.x, (jint)fit.y, (jint)fit.w, (jint)fit.h };
  jintArray array = (*env)->NewIntArray(env, 4);
  if (array) (*env)->SetIntArrayRegion(env, array, 0, 4, values);
  return array;
}

static long long now_us(void)
{
  struct timespec t;
  clock_gettime(CLOCK_MONOTONIC, &t);
  return (long long)t.tv_sec * 1000000 + t.tv_nsec / 1000;
}

static long long cpu_us(void)
{
  struct rusage u;
  getrusage(RUSAGE_SELF, &u);
  return (long long)(u.ru_utime.tv_sec + u.ru_stime.tv_sec) * 1000000 + u.ru_utime.tv_usec + u.ru_stime.tv_usec;
}

// Posts frames until the socket closes (0) or a signal asks to stop (1); negative on an error.
// A direct frame (xnuhost's "frame") is hashed against what the layer shows (xhd_direct_damage): unchanged, it is
// answered at once and nothing is locked or posted, so surfaceflinger has nothing to compose and the panel's refresh
// rate can drop by itself; changed, the changed tiles are copied from the guest's pages into the snapshot at once (the
// guest completes its swap without waiting for us and draws into that surface again a few frames later: xhd.h), then
// only their rectangle is locked and filled from the snapshot, the window keeping the rest from the last buffer
// (Surface::lock copies back the last post's damage into the buffer it hands out, whichever of the queue's buffers
// that is). The answer ("done") goes after the post: xnuhost then answers the guest's flush.

// Pacing: the next buffer is only handed out when surfaceflinger has taken the last one, so
// ANativeWindow_lock blocks to the panel's vsync, and every flush that came meanwhile is one post.
// The buffer layout and dataspace a frame format gets (xhd_buffer_for): {buffer, dataspace}.
JNIEXPORT jintArray JNICALL Java_OneiOSDisplay_nBuffer(JNIEnv *env, jclass type, jint format)
{
  (void)type;
  int dataspace = 0;
  jint values[2] = { (jint)xhd_buffer_for((uint32_t)format, &dataspace), (jint)dataspace };
  jintArray array = (*env)->NewIntArray(env, 2);
  if (array) (*env)->SetIntArrayRegion(env, array, 0, 2, values);
  return array;
}

// Tags the window's buffers with the dataspace: what the numbers in them mean, so that surfaceflinger turns them into
// the panel's colours (One UI's colour mode decides how; nothing of it is changed here). Logs what the window has.
static void set_dataspace(ANativeWindow *window, int dataspace)
{
  int error = ANativeWindow_setBuffersDataSpace(window, dataspace);
  int now = ANativeWindow_getBuffersDataSpace(window);
  if (error) fprintf(stderr, "oneios-display: the dataspace %s (%#x) was refused (%d); the buffers say %s (%#x)\n",
                     xhd_dataspace_name(dataspace), (unsigned)dataspace, error, xhd_dataspace_name(now), (unsigned)now);
  else fprintf(stderr, "oneios-display: buffers tagged %s (dataspace %#x)\n", xhd_dataspace_name(now), (unsigned)now);
}

static const char *format_name(int format)
{
  return format == FORMAT_BGRA_8888 ? "BGRA_8888" : format == FORMAT_RGBX_8888 ? "RGBX_8888" : format == FORMAT_RGBA_FP16 ? "RGBA_FP16" : "other";
}

static double seconds_since(const struct timespec *t0)
{
  struct timespec t;
  clock_gettime(CLOCK_MONOTONIC, &t);
  return (double)(t.tv_sec - t0->tv_sec) + (double)(t.tv_nsec - t0->tv_nsec) / 1e9;
}

// ready_path: SESSION/ready (--session), or NULL (--socket): the first post waits for it (xhd_hold_first). Until the
// first post the layer has no buffer and the OneOS boot screen under it shows.
JNIEXPORT jint JNICALL Java_OneiOSDisplay_nRun(JNIEnv *env, jclass type, jlong handle, jobject surface, jint format, jint dataspace,
                                               jstring ready_path)
{
  (void)type;
  struct context *c = (struct context *)(intptr_t)handle;
  const struct xhd_frame *f = &c->frame;
  ANativeWindow *window = ANativeWindow_fromSurface(env, surface);
  if (!window) { fprintf(stderr, "oneios-display: no native window for the layer\n"); return -1; }
  ANativeWindow_setBuffersGeometry(window, (int32_t)f->width, (int32_t)f->height, format);
  set_dataspace(window, dataspace);
  char ready_file[600] = "";
  if (ready_path) {
    const char *p = (*env)->GetStringUTFChars(env, ready_path, NULL);
    if (p) { snprintf(ready_file, sizeof ready_file, "%s", p); (*env)->ReleaseStringUTFChars(env, ready_path, p); }
  }
  struct xhd_rect pending = { 0, 0, f->width, f->height };
  struct xhd_direct *direct = &c->direct;
  struct xhd_damage damage;
  bool hashing = !xhd_damage_init(&damage, f->width, f->height);
  if (!hashing) fprintf(stderr, "oneios-display: no memory for the damage hashes: every direct frame is copied whole\n");
  // The guest's pages are read only while the frame is new (xhd.h, struct xhd_snapshot): made at the first direct frame.
  struct xhd_snapshot snapshot = { 0 };
  bool snapshot_tried = false;
  unsigned long frames = 0, direct_frames = 0, refused = 0, held_flushes = 0;
  bool announced = false, held = false;
  struct timespec hold_start;
  struct { unsigned long posted, direct, unchanged; long long lock_us, copy_us, copy_max, hash_us, rows, since, cpu, read_us, read_max; } st =
    { 0, 0, 0, 0, 0, 0, 0, 0, now_us(), cpu_us(), 0, 0 };
  int result;
  for (;;) {
    // Held: the guest's frame is not shown yet, the damage adds up, and `ready` is looked for at every flush and
    // every XHD_HOLD_POLL_MS between them (a guest that drew its frame with content and then nothing more).
    bool hold = frames == 0 && xhd_hold_first(ready_file);
    if (hold && !held) {
      held = true;
      clock_gettime(CLOCK_MONOTONIC, &hold_start);
      fprintf(stderr, "oneios-display: the first frame waits for %s (xnuhost's first frame of the guest's own with content); the boot screen stays\n", ready_file);
    }
    struct pollfd fds[2] = { { c->fd, POLLIN, 0 }, { signal_pipe[0], POLLIN, 0 } };
    int ready = poll(fds, 2, hold ? XHD_HOLD_POLL_MS : (pending.w || direct->pending) ? 0 : -1);
    if (ready < 0 && errno != EINTR) { result = -3; break; }
    if (stopping || (ready > 0 && fds[1].revents)) { result = 1; break; }
    if (xhd_drain_direct(c->fd, f, &pending, direct) < 0) { result = 0; break; }
    if (hold) { if (ready > 0) held_flushes++; continue; }
    if (!pending.w && !direct->pending) continue;
    // What has to be drawn: xnuhost's own damage (the memfd), and the direct frame's changed tiles.
    struct xhd_rect want = pending, changed = { 0, 0, 0, 0 };
    int hashed = -1, snapped = -1;
    long long th = now_us();
    if (direct->pending) {
      hashed = hashing ? xhd_direct_damage(direct, &damage, &changed) : -ENOMEM;
      if (hashed) changed = (struct xhd_rect){ 0, 0, f->width, f->height };        // copied whole (or refused below)
      st.hash_us += now_us() - th;
      if (!hashed && !changed.w && !pending.w && frames >= 4) {
        xhd_send_done(c->fd, direct->seq, XHD_DONE_UNCHANGED);                   // the layer shows it already
        direct->pending = 0;
        st.unchanged++;
        goto stats;
      }
      xhd_rect_union(&want, &changed);
      // The changed tiles into the snapshot now, while the guest has not drawn into this surface again; the layer's
      // buffer (whose wait is the long part) is filled from the snapshot below.
      if (!snapshot_tried) {
        snapshot_tried = true;
        if (xhd_snapshot_init(&snapshot, f->width, f->height))
          fprintf(stderr, "oneios-display: no memory for the snapshot: direct frames are copied from guest RAM after the buffer wait\n");
      }
      snapped = snapshot.pixels ? xhd_snapshot_take(&snapshot, direct, &changed) : -ENOMEM;
      long long read = now_us() - th;
      st.read_us += read;
      if (read > st.read_max) st.read_max = read;
    }
    ARect dirty = { (int32_t)want.x, (int32_t)want.y, (int32_t)(want.x + want.w), (int32_t)(want.y + want.h) };
    if (frames < 4 || !want.w) dirty = (ARect){ 0, 0, (int32_t)f->width, (int32_t)f->height };
    ANativeWindow_Buffer buffer;
    long long t0 = now_us();
    int error = ANativeWindow_lock(window, &buffer, &dirty);
    long long t1 = now_us();
    if (error) {
      if (frames == 0 && format != FORMAT_RGBX_8888) {
        // No B,G,R,A buffers here: R,G,B,X and a swap. No RGBA_FP16 ones: B,G,R,A, the half floats encoded to sRGB
        // and clamped (what is outside sRGB is lost), tagged sRGB.
        int next = format == FORMAT_RGBA_FP16 ? FORMAT_BGRA_8888 : FORMAT_RGBX_8888;
        fprintf(stderr, "oneios-display: no buffer in format %d %s (%d), using %s\n", format, format_name(format), error, format_name(next));
        format = next;
        ANativeWindow_setBuffersGeometry(window, (int32_t)f->width, (int32_t)f->height, format);
        if (dataspace == XHD_DATASPACE_SCRGB_LINEAR || dataspace == XHD_DATASPACE_SCRGB) set_dataspace(window, dataspace = XHD_DATASPACE_SRGB);
        continue;
      }
      fprintf(stderr, "oneios-display: the layer's buffer could not be locked (%d)\n", error);
      if (direct->pending) xhd_send_done(c->fd, direct->seq, XHD_DONE_FAILED);
      result = -2;
      break;
    }
    uint32_t width = (uint32_t)buffer.width < f->width ? (uint32_t)buffer.width : f->width;
    uint32_t height = (uint32_t)buffer.height < f->height ? (uint32_t)buffer.height : f->height;
    bool usable = buffer.width > 0 && buffer.height > 0 && buffer.stride >= buffer.width;
    int swap = buffer.format != FORMAT_BGRA_8888;
    // The rectangle the window wants drawn (it may hand out more than was asked: a new buffer, the first ones).
    if (dirty.left < 0) dirty.left = 0;
    if (dirty.top < 0) dirty.top = 0;
    struct xhd_rect fill = { 0, 0, 0, 0 };
    if (dirty.right > dirty.left && dirty.bottom > dirty.top)
      fill = (struct xhd_rect){ (uint32_t)dirty.left, (uint32_t)dirty.top, (uint32_t)(dirty.right - dirty.left), (uint32_t)(dirty.bottom - dirty.top) };
    if (frames < 4) fill = (struct xhd_rect){ 0, 0, f->width, f->height };
    int direct_failed = 0;
    // The buffer's own format decides the copy (RGBA_8888 and RGBX_8888 are both R,G,B bytes).
    int layout = buffer.format == FORMAT_BGRA_8888 || buffer.format == FORMAT_RGBA_FP16 ? buffer.format : FORMAT_RGBX_8888;
    size_t bytes = layout == FORMAT_RGBA_FP16 ? 8 : 4;
    if (usable && xhd_rect_clip(&fill, width, height)) {
      if (direct->pending && bytes == 4) {
        // The guest's surface has the whole frame: all of `fill` comes from it. What xnuhost drew itself in the memfd
        // (pending) goes over it, and the layer then no longer matches the hashes there.
        struct xhd_rect done = fill;
        int copied;
        if (!snapped && snapshot.full) {
          // From the snapshot (the newest frame in every pixel): the guest's pages are not read again here.
          copied = xhd_copy(buffer.bits, (size_t)buffer.stride * 4, layout, snapshot.pixels, snapshot.stride, XHD_FORMAT_BGRX, fill);
        } else copied = xhd_copy_direct(direct, buffer.bits, (size_t)buffer.stride * 4, width, height, swap, &fill, &done);
        if (copied) { refused++; direct_failed = 1; if (refused == 1) fprintf(stderr, "oneios-display: a direct frame could not be copied (%s)\n", strerror(-copied)); }
        else { direct_frames++; st.direct++; st.rows += done.h; }
        struct xhd_rect own = pending;
        if (own.w && xhd_rect_clip(&own, width, height)) xhd_copy(buffer.bits, (size_t)buffer.stride * bytes, layout, f->pixels, f->stride, f->format, own);
        if (copied || pending.w) damage.valid = 0;
      } else {
        // Every pixel is xnuhost's: the window's rectangle comes from the memfd. Once frames come direct the memfd is
        // stale, so then only xnuhost's own damage does. (A direct frame with a half-float buffer is not copied from
        // guest RAM: refused, so xnuhost copies the next ones itself.)
        if (direct->pending) { refused++; direct_failed = 1; }
        struct xhd_rect r = direct_frames ? pending : fill;
        if (r.w && xhd_rect_clip(&r, width, height)) xhd_copy(buffer.bits, (size_t)buffer.stride * bytes, layout, f->pixels, f->stride, f->format, r);
        damage.valid = 0;
      }
    }
    long long t2 = now_us();
    ANativeWindow_unlockAndPost(window);
    if (direct->pending) {
      if (!direct_failed && !hashed && !pending.w) xhd_damage_commit(&damage);
      xhd_send_done(c->fd, direct->seq, direct_failed ? XHD_DONE_FAILED : 0);
      direct->pending = 0;
    }
    if (frames++ == 0) {
      char waited[96] = "";
      if (held) snprintf(waited, sizeof waited, "; held %.1f s for session/ready, %lu wakes for flushes not shown", seconds_since(&hold_start), held_flushes);
      fprintf(stderr, "oneios-display: first frame posted (%dx%d buffer, stride %d, format %d %s, dataspace %#x %s; the guest's frame: %s%s)\n",
              buffer.width, buffer.height, buffer.stride, buffer.format, format_name(buffer.format), (unsigned)ANativeWindow_getBuffersDataSpace(window),
              xhd_dataspace_name(ANativeWindow_getBuffersDataSpace(window)), xh_pixels_name(f->format), waited);
    }
    if (direct_frames && !announced) {
      announced = true;
      fprintf(stderr, "oneios-display: frames come straight from guest RAM now, only their changed tiles copied and posted\n");
    }
    pending = (struct xhd_rect){ 0, 0, 0, 0 };
    st.posted++; st.lock_us += t1 - t0; st.copy_us += t2 - t1;
    if (t2 - t1 > st.copy_max) st.copy_max = t2 - t1;
  stats:;
    // Reported when a frame comes and 10 s have passed since the last report: no timer, so nothing at all when idle.
    long long now = now_us();
    if (now - st.since >= 10000000) {
      long long cpu = cpu_us();
      unsigned long seen = st.posted + st.unchanged;
      fprintf(stderr, "oneios-display: %.0f s: %lu frames from xnuhost, %lu unchanged (nothing posted), %lu posted (%lu from guest RAM, "
              "%.0f%% of the rows copied); hash mean %.1f ms; guest pages read for mean %.1f ms, max %.1f ms (hash and snapshot%s); "
              "buffer wait mean %.1f ms; copy mean %.1f ms, max %.1f ms; this process %.0f%% of a core\n",
              (now - st.since) / 1e6, seen, st.unchanged, st.posted, st.direct,
              st.direct ? st.rows * 100.0 / ((double)st.direct * f->height) : 0.0, seen ? st.hash_us / 1000.0 / seen : 0.0,
              seen ? st.read_us / 1000.0 / seen : 0.0, st.read_max / 1000.0, snapshot.pixels ? "" : "; NO snapshot: also the copy after the wait",
              st.posted ? st.lock_us / 1000.0 / st.posted : 0.0, st.posted ? st.copy_us / 1000.0 / st.posted : 0.0, st.copy_max / 1000.0,
              (cpu - st.cpu) * 100.0 / (double)(now - st.since));
      st = (__typeof__(st)){ 0, 0, 0, 0, 0, 0, 0, 0, now, cpu, 0, 0 };
    }
  }
  xhd_damage_free(&damage);
  xhd_snapshot_free(&snapshot);
  ANativeWindow_release(window);
  fprintf(stderr, "oneios-display: %s after %lu frames\n", result == 0 ? "the socket closed" : result == 1 ? "asked to stop" : "error", frames);
  return result;
}

JNIEXPORT void JNICALL Java_OneiOSDisplay_nClose(JNIEnv *env, jclass type, jlong handle)
{
  (void)env; (void)type;
  struct context *c = (struct context *)(intptr_t)handle;
  if (!c) return;
  close(c->fd);
  xhd_frame_close(&c->frame);
  xhd_direct_close(&c->direct);
  free(c);
}
