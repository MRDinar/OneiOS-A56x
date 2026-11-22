// SPDX-License-Identifier: MIT
// The device-side encoder backend (encoder.h) that runs the real encoder in the oneios-encoder helper process
// (helper.c), because MediaCodec is a shared library and xnuhost is static. It spawns the helper once, multiplexes
// the encoder sessions over a UNIX socket (encoder_link.h), and buffers the coded access units the helper returns for
// poll(). Static, libc only, like the rest of libonebridges.
#define _GNU_SOURCE
#include "encoder.h"
#include "encoder_link.h"
#include <errno.h>
#include <pthread.h>
#include <signal.h>
#include <spawn.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <sys/socket.h>
#include <sys/time.h>
#include <sys/wait.h>
#include <unistd.h>

extern char **environ;

enum { HELPER_MAX_SESSIONS = 8, HELPER_CODED_RING = 32 };

struct coded { uint8_t *bytes; uint32_t size; int64_t pts; int key, eos; };

struct br_enc_session {
  uint32_t id;
  bool used, open_ok, open_done;
  struct coded ring[HELPER_CODED_RING];
  unsigned head, tail;
  bool got_eos;
};

static struct {
  int fd;
  pid_t pid;
  pthread_t thread;
  pthread_mutex_t lock;
  pthread_cond_t cond;                     // open replies and new coded buffers (CLOCK_MONOTONIC)
  bool started, gone, lock_init;
  bool killed;                             // this helper was killed (it hung): its receive thread sees the end soon
  unsigned hangs;                          // helpers killed for a hang this session
  bool off_said;
  uint32_t next_id;                        // session ids are never reused: a late reply cannot be taken for a new open
  struct br_enc_session sessions[HELPER_MAX_SESSIONS];
  char path[512];
} H;

static int write_all(int fd, const void *p, size_t n) {
  const uint8_t *b = p;
  while (n) { ssize_t w = write(fd, b, n); if (w <= 0) { if (w < 0 && errno == EINTR) continue; return -1; } b += w; n -= (size_t)w; }
  return 0;
}
static int read_all(int fd, void *p, size_t n) {
  uint8_t *b = p;
  while (n) { ssize_t r = read(fd, b, n); if (r <= 0) { if (r < 0 && errno == EINTR) continue; return -1; } b += r; n -= (size_t)r; }
  return 0;
}

static struct br_enc_session *session_by_id(uint32_t id) {
  for (unsigned i = 0; i < HELPER_MAX_SESSIONS; i++) if (H.sessions[i].used && H.sessions[i].id == id) return &H.sessions[i];
  return NULL;
}

// The receive loop: open replies, coded buffers and EOS from the helper.
static void *receive(void *arg) {
  (void)arg;
  for (;;) {
    struct enc_msg m;
    if (read_all(H.fd, &m, sizeof m)) break;
    uint8_t *payload = NULL;
    if (m.len) { payload = malloc(m.len); if (!payload) break; if (read_all(H.fd, payload, m.len)) { free(payload); break; } }
    pthread_mutex_lock(&H.lock);
    struct br_enc_session *s = session_by_id(m.session);
    if (m.type == ENC_MSG_OPEN_REPLY && s) { s->open_ok = m.session != 0 && m.a != 0; s->open_done = true; }
    else if (m.type == ENC_MSG_CODED && s && payload) {
      if (s->head - s->tail < HELPER_CODED_RING) {
        s->ring[s->head % HELPER_CODED_RING] = (struct coded){ payload, m.len, m.pts, (int)(m.a & 1), (int)((m.a >> 1) & 1) };
        s->head++;
        payload = NULL;
      }
    } else if (m.type == ENC_MSG_EOS && s) s->got_eos = true;
    pthread_cond_broadcast(&H.cond);
    pthread_mutex_unlock(&H.lock);
    free(payload);
  }
  pthread_mutex_lock(&H.lock);
  H.gone = true;
  pthread_cond_broadcast(&H.cond);
  pthread_mutex_unlock(&H.lock);
  return NULL;
}

// A vCPU calls the backend (the device's commands are answered on the vCPU, media_device.c), so nothing here may
// wait for the helper without a limit: a vCPU in a host call takes no interrupt, and the guest's debugger cross-call
// (a stackshot: spindump, ReportCrash) panics after debug-ack (5 s on the phone, hosted 0017) when one vCPU does not
// answer. Two phone boots of v0.1.16 (2026-11-23) froze that way: the helper never answered an OPEN (MediaCodec's
// open hung in it), the vCPU waited here for ever with the virtio-media device held, xnuhost's main loop then waited
// for that device, and 80-90 s later a stackshot panicked ("Debugger synch pending on cpu 1", then "on cpu 2"). The
// limits: the HELLO and each write HELPER_IO_MS, an open's reply br_encoder_helper_open_ms; a helper that misses an
// open's reply is killed (it is stuck in MediaCodec), and the next open starts a new one, HELPER_HANGS times at most.
enum { HELPER_IO_MS = 1000, HELPER_HANGS = 2 };
unsigned br_encoder_helper_open_ms = 2500;

static void set_timeout(int fd, int which, unsigned ms) {
  struct timeval tv = { (time_t)(ms / 1000), (suseconds_t)(ms % 1000) * 1000 };
  (void)setsockopt(fd, SOL_SOCKET, which, &tv, sizeof tv);
}

// What the helper's main thread waits in (its /proc wchan and state), for the line that says it hung.
static void helper_wait_state(pid_t pid, char *out, size_t max) {
  char path[64], wchan[64] = "?", stat[256] = "";
  snprintf(path, sizeof path, "/proc/%d/wchan", (int)pid);
  FILE *f = fopen(path, "r");
  if (f) { if (!fgets(wchan, sizeof wchan, f)) snprintf(wchan, sizeof wchan, "?"); fclose(f); }
  snprintf(path, sizeof path, "/proc/%d/stat", (int)pid);
  f = fopen(path, "r");
  char state = '?';
  if (f) { if (fgets(stat, sizeof stat, f)) { char *close = strrchr(stat, ')'); if (close && close[1] == ' ') state = close[2]; } fclose(f); }
  snprintf(out, max, "state %c, waiting in %s", state, wchan[0] ? wchan : "?");
}

// The helper hung: killed. Its receive thread sees the socket's end and marks it gone; the next open starts another.
static void helper_kill(const char *why) {
  char state[96];
  helper_wait_state(H.pid, state, sizeof state);
  H.hangs++;
  br_log(0, "encoder: the helper (pid %d) %s (%s): killed (%u of %u); the next open starts a new one", (int)H.pid, why, state, H.hangs, HELPER_HANGS);
  kill(H.pid, SIGKILL);
  H.killed = true;
  shutdown(H.fd, SHUT_RDWR);
}

// Spawns the helper and waits for its HELLO, or starts a new one after the last one ended. 0 or -errno. H.lock held.
static int helper_start(void) {
  if (H.started && !H.gone) return H.killed ? -EPIPE : 0;     // a killed helper is not gone yet: this open fails
  if (H.started) {
    // The last helper ended (it was killed, or it died): its receive thread has returned or is about to.
    pthread_mutex_unlock(&H.lock);
    pthread_join(H.thread, NULL);
    pthread_mutex_lock(&H.lock);
    close(H.fd);
    (void)waitpid(H.pid, NULL, WNOHANG);
    H.started = H.gone = H.killed = false;
    for (unsigned i = 0; i < HELPER_MAX_SESSIONS; i++) {
      struct br_enc_session *s = &H.sessions[i];
      while (s->tail != s->head) { free(s->ring[s->tail % HELPER_CODED_RING].bytes); s->tail++; }
      s->used = false;
    }
  }
  if (H.hangs >= HELPER_HANGS) {
    if (!H.off_said) br_log(0, "encoder: the helper hung %u times: no more helpers this session (VideoToolbox keeps its software encoder)", H.hangs);
    H.off_said = true;
    return -EIO;
  }
  int sv[2];
  if (socketpair(AF_UNIX, SOCK_STREAM | SOCK_CLOEXEC, 0, sv)) return -errno;
  posix_spawn_file_actions_t fa;
  posix_spawn_file_actions_init(&fa);
  posix_spawn_file_actions_adddup2(&fa, sv[1], 3);
  char three[] = "3";
  char *const argv[] = { H.path, three, NULL };
  int rc = posix_spawn(&H.pid, H.path, &fa, NULL, argv, environ);
  posix_spawn_file_actions_destroy(&fa);
  close(sv[1]);
  if (rc) { close(sv[0]); return -rc; }
  H.fd = sv[0];
  set_timeout(H.fd, SO_RCVTIMEO, HELPER_IO_MS);           // the HELLO only: the receive thread waits without a limit
  set_timeout(H.fd, SO_SNDTIMEO, HELPER_IO_MS);           // every write: a helper that stops reading fails it
  struct enc_msg hello;
  if (read_all(H.fd, &hello, sizeof hello) || hello.type != ENC_MSG_HELLO || hello.a != ENC_LINK_VERSION) {
    br_log(0, "encoder: the helper %s (pid %d) sent no HELLO in %u ms: killed", H.path, (int)H.pid, HELPER_IO_MS);
    kill(H.pid, SIGKILL);
    (void)waitpid(H.pid, NULL, 0);
    close(H.fd);
    return -EPROTO;
  }
  set_timeout(H.fd, SO_RCVTIMEO, 0);
  if (pthread_create(&H.thread, NULL, receive, NULL)) {
    kill(H.pid, SIGKILL);
    (void)waitpid(H.pid, NULL, 0);
    close(H.fd);
    return -EAGAIN;
  }
  H.started = true;
  br_log(1, "encoder: helper %s (pid %d)", H.path, (int)H.pid);
  return 0;
}

static br_enc_session *helper_open(uint32_t codec, uint32_t width, uint32_t height) {
  pthread_mutex_lock(&H.lock);
  if (helper_start()) { pthread_mutex_unlock(&H.lock); return NULL; }
  struct br_enc_session *s = NULL;
  for (unsigned i = 0; i < HELPER_MAX_SESSIONS; i++) if (!H.sessions[i].used) { s = &H.sessions[i]; break; }
  if (!s) { pthread_mutex_unlock(&H.lock); return NULL; }
  memset(s, 0, sizeof *s);
  s->used = true;
  s->id = ++H.next_id ? H.next_id : ++H.next_id;            // 0 is no session
  struct enc_msg m = { ENC_MSG_OPEN, s->id, 0, codec, width, height, 0 };
  if (write_all(H.fd, &m, sizeof m)) {
    s->used = false;
    helper_kill("took no OPEN");
    pthread_mutex_unlock(&H.lock);
    return NULL;
  }
  struct timespec until;
  clock_gettime(CLOCK_MONOTONIC, &until);
  until.tv_sec += br_encoder_helper_open_ms / 1000;
  until.tv_nsec += (long)(br_encoder_helper_open_ms % 1000) * 1000000L;
  if (until.tv_nsec >= 1000000000L) { until.tv_sec++; until.tv_nsec -= 1000000000L; }
  int rc = 0;
  while (!s->open_done && !H.gone && rc != ETIMEDOUT) rc = pthread_cond_timedwait(&H.cond, &H.lock, &until);
  bool ok = s->open_done && s->open_ok;
  if (!s->open_done && !H.gone) {
    char why[64];
    snprintf(why, sizeof why, "did not answer an open in %u ms", br_encoder_helper_open_ms);
    helper_kill(why);
  }
  if (!ok) s->used = false;
  pthread_mutex_unlock(&H.lock);
  return ok ? s : NULL;
}

static int helper_feed(br_enc_session *s, const uint8_t *nv12, uint32_t len, int64_t pts_us, int force_key) {
  struct enc_msg m = { ENC_MSG_FEED, s->id, pts_us, (uint32_t)(force_key ? 1 : 0), 0, 0, len };
  pthread_mutex_lock(&H.lock);
  int rc = (write_all(H.fd, &m, sizeof m) || write_all(H.fd, nv12, len)) ? -EIO : 0;
  if (rc && H.started && !H.gone && !H.killed) helper_kill("did not take a frame");
  pthread_mutex_unlock(&H.lock);
  return rc;
}

static int helper_drain(br_enc_session *s) {
  struct enc_msg m = { ENC_MSG_DRAIN, s->id, 0, 0, 0, 0, 0 };
  pthread_mutex_lock(&H.lock);
  int rc = write_all(H.fd, &m, sizeof m) ? -EIO : 0;
  if (rc && H.started && !H.gone && !H.killed) helper_kill("did not take a drain");
  pthread_mutex_unlock(&H.lock);
  return rc;
}

static int helper_poll(br_enc_session *s, uint8_t *buf, uint32_t cap, int *key, int64_t *pts_us, int *eos) {
  *key = 0; *eos = 0; *pts_us = 0;
  pthread_mutex_lock(&H.lock);
  int bytes = 0;
  if (s->tail != s->head) {
    struct coded *c = &s->ring[s->tail % HELPER_CODED_RING];
    // A unit larger than the guest's coded buffer is dropped and said once: left at the ring's head it came back as
    // an error at every poll of the device's worker, for as long as the session lived, and no later frame got out.
    if (c->size > cap) {
      free(c->bytes); c->bytes = NULL;
      s->tail++;
      pthread_mutex_unlock(&H.lock);
      return -ENOSPC;
    }
    memcpy(buf, c->bytes, c->size);
    *key = c->key; *eos = c->eos; *pts_us = c->pts;
    bytes = (int)c->size;
    free(c->bytes); c->bytes = NULL;
    s->tail++;
  } else if (s->got_eos) { *eos = 1; s->got_eos = false; }
  pthread_mutex_unlock(&H.lock);
  return bytes;
}

static void helper_close(br_enc_session *s) {
  struct enc_msg m = { ENC_MSG_CLOSE, s->id, 0, 0, 0, 0, 0 };
  pthread_mutex_lock(&H.lock);
  if (H.started && !H.gone) write_all(H.fd, &m, sizeof m);
  while (s->tail != s->head) { free(s->ring[s->tail % HELPER_CODED_RING].bytes); s->tail++; }
  s->used = false;
  pthread_mutex_unlock(&H.lock);
}

static br_encoder_backend helper_backend = {
  .name = "mediacodec-helper", .open = helper_open, .feed = helper_feed, .drain = helper_drain,
  .poll = helper_poll, .close = helper_close,
};

const br_encoder_backend *br_encoder_backend_helper(const char *path) {
  if (!H.lock_init) {
    pthread_mutex_init(&H.lock, NULL);
    pthread_condattr_t attr;
    pthread_condattr_init(&attr);
    pthread_condattr_setclock(&attr, CLOCK_MONOTONIC);
    pthread_cond_init(&H.cond, &attr);
    pthread_condattr_destroy(&attr);
    H.lock_init = true;
  }
  snprintf(H.path, sizeof H.path, "%s", path);
  return &helper_backend;
}

// xnuhost's stop: the helper goes with the session, also one that hangs in MediaCodec and would never see its socket's
// end. Never waits: the lock is only tried (a vCPU may be in a bounded wait), the kill is SIGKILL.
void br_encoder_helper_stop(void) {
  if (!H.lock_init) return;
  bool locked = pthread_mutex_trylock(&H.lock) == 0;
  if (H.started && !H.gone && H.pid > 0) {
    kill(H.pid, SIGKILL);
    (void)waitpid(H.pid, NULL, WNOHANG);
    H.killed = true;
  }
  if (locked) pthread_mutex_unlock(&H.lock);
}
