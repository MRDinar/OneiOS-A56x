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

#define FORMAT_BGRA_8888 5              // HAL_PIXEL_FORMAT_BGRA_8888: the guest's bytes as they are
#define FORMAT_RGBX_8888 2

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
// Pacing: the next buffer is only handed out when surfaceflinger has taken the last one, so
// ANativeWindow_lock blocks to the panel's vsync, and every flush that came meanwhile is one post.
// A direct frame (xnuhost's "frame") is copied from the guest's own pages into the buffer, and answered ("done") once
// the buffer is posted: xnuhost then answers the guest's flush, so the guest may draw into that surface again.
JNIEXPORT jint JNICALL Java_OneiOSDisplay_nRun(JNIEnv *env, jclass type, jlong handle, jobject surface, jint format)
{
  (void)type;
  struct context *c = (struct context *)(intptr_t)handle;
  const struct xhd_frame *f = &c->frame;
  ANativeWindow *window = ANativeWindow_fromSurface(env, surface);
  if (!window) { fprintf(stderr, "oneios-display: no native window for the layer\n"); return -1; }
  ANativeWindow_setBuffersGeometry(window, (int32_t)f->width, (int32_t)f->height, format);
  struct xhd_rect pending = { 0, 0, f->width, f->height };
  struct xhd_direct *direct = &c->direct;
  unsigned long frames = 0, direct_frames = 0, refused = 0;
  bool announced = false;
  struct { unsigned long frames, direct; long long lock_us, copy_us, copy_max, since, cpu; } st = { 0, 0, 0, 0, 0, now_us(), cpu_us() };
  int result;
  for (;;) {
    struct pollfd fds[2] = { { c->fd, POLLIN, 0 }, { signal_pipe[0], POLLIN, 0 } };
    int ready = poll(fds, 2, pending.w || direct->pending ? 0 : -1);
    if (ready < 0 && errno != EINTR) { result = -3; break; }
    if (stopping || (ready > 0 && fds[1].revents)) { result = 1; break; }
    if (xhd_drain_direct(c->fd, f, &pending, direct) < 0) { result = 0; break; }
    if (!pending.w && !direct->pending) continue;
    // The first posts fill whole buffers; after that the window copies back what the last frame
    // had and hands out the rectangle that has to be drawn (it may be larger than what was asked).
    struct xhd_rect want = pending;
    if (direct->pending) {
      const struct xhd_slot *s = (const struct xhd_slot *)(direct->table + (size_t)direct->slot * direct->slot_bytes);
      struct xhd_rect r = { s->x, s->y, s->w, s->h };
      if (xhd_rect_clip(&r, f->width, f->height)) xhd_rect_union(&want, &r);
    }
    ARect dirty = { (int32_t)want.x, (int32_t)want.y, (int32_t)(want.x + want.w), (int32_t)(want.y + want.h) };
    if (frames < 4 || !want.w) dirty = (ARect){ 0, 0, (int32_t)f->width, (int32_t)f->height };
    ANativeWindow_Buffer buffer;
    long long t0 = now_us();
    int error = ANativeWindow_lock(window, &buffer, &dirty);
    long long t1 = now_us();
    if (error) {
      if (frames == 0 && format != FORMAT_RGBX_8888) {         // no B,G,R,A buffers here: R,G,B,X and a swap
        fprintf(stderr, "oneios-display: no buffer in format %d (%d), using R,G,B,X\n", format, error);
        format = FORMAT_RGBX_8888;
        ANativeWindow_setBuffersGeometry(window, (int32_t)f->width, (int32_t)f->height, format);
        continue;
      }
      fprintf(stderr, "oneios-display: the layer's buffer could not be locked (%d)\n", error);
      if (direct->pending) xhd_send_done(c->fd, direct->seq, XHD_DONE_FAILED);
      result = -2;
      break;
    }
    struct xhd_rect r = { 0, 0, 0, 0 };
    if (dirty.left < 0) dirty.left = 0;
    if (dirty.top < 0) dirty.top = 0;
    uint32_t width = (uint32_t)buffer.width < f->width ? (uint32_t)buffer.width : f->width;
    uint32_t height = (uint32_t)buffer.height < f->height ? (uint32_t)buffer.height : f->height;
    bool usable = buffer.width > 0 && buffer.height > 0 && buffer.stride >= buffer.width;
    int swap = buffer.format != FORMAT_BGRA_8888;
    // What only xnuhost's frame has (its own copies, the boot console) comes from the memfd: the first whole buffers, the
    // window's dirty rectangle while every frame is xnuhost's, and only xnuhost's own damage once frames come direct
    // (the memfd does not have them, so it must not paint over what the window kept of the last one).
    if (usable && (pending.w || frames < 4)) {
      if (frames < 4) r = (struct xhd_rect){ 0, 0, f->width, f->height };
      else if (direct->pending || direct_frames) r = pending;
      else if (dirty.right > dirty.left && dirty.bottom > dirty.top)
        r = (struct xhd_rect){ (uint32_t)dirty.left, (uint32_t)dirty.top, (uint32_t)(dirty.right - dirty.left), (uint32_t)(dirty.bottom - dirty.top) };
      if (xhd_rect_clip(&r, width, height)) xhd_copy(buffer.bits, (size_t)buffer.stride * 4, f->pixels, f->stride, r, swap);
    }
    int direct_failed = 0;
    if (direct->pending) {
      struct xhd_rect done;
      int copied = usable ? xhd_copy_direct(direct, buffer.bits, (size_t)buffer.stride * 4, width, height, swap, &done) : -EINVAL;
      if (copied) { refused++; direct_failed = 1; } else { direct_frames++; st.direct++; }
      if (copied && refused == 1) fprintf(stderr, "oneios-display: a direct frame could not be copied (%s)\n", strerror(-copied));
    }
    long long t2 = now_us();
    ANativeWindow_unlockAndPost(window);
    if (direct->pending) { xhd_send_done(c->fd, direct->seq, direct_failed ? XHD_DONE_FAILED : 0); direct->pending = 0; }
    if (frames++ == 0)
      fprintf(stderr, "oneios-display: first frame posted (%dx%d buffer, stride %d, format %d)\n", buffer.width, buffer.height, buffer.stride, buffer.format);
    if (direct_frames && !announced) { announced = true; fprintf(stderr, "oneios-display: frames come straight from guest RAM now (one copy, none in xnuhost)\n"); }
    pending = (struct xhd_rect){ 0, 0, 0, 0 };
    st.frames++; st.lock_us += t1 - t0; st.copy_us += t2 - t1;
    if (t2 - t1 > st.copy_max) st.copy_max = t2 - t1;
    if (t2 - st.since >= 10000000) {
      long long cpu = cpu_us();
      fprintf(stderr, "oneios-display: 10 s: %lu frames (%lu from guest RAM), %.1f/s; buffer wait mean %.1f ms; copy mean %.1f ms, max %.1f ms; "
              "this process %.0f%% of a core\n", st.frames, st.direct, st.frames * 1e6 / (double)(t2 - st.since), st.lock_us / 1000.0 / st.frames,
              st.copy_us / 1000.0 / st.frames, st.copy_max / 1000.0, (cpu - st.cpu) * 100.0 / (double)(t2 - st.since));
      st = (__typeof__(st)){ 0, 0, 0, 0, 0, t2, cpu };
    }
  }
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
