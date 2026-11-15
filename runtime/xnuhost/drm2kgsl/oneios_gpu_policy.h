// SPDX-License-Identifier: MIT
// OneiOS: the GPU's per-process policy, as pure functions with no KGSL call (unit-tested on any host:
// tests/test_gpu_policy.c). Two parts (docs/gpu.md 2.2b):
//
// 1. A context's CLASS, from its owner: the render server, the front app, the default, or a background process. The
//    class gives the KGSL priority of the context's queues (DRAWCTXT_CREATE's priority bits, fixed when a queue is
//    made) and decides whether its frames count for the GPU's level. The sources, in order:
//      - the name "xnu:backboardd" (the render server; the name the guest's DroidVMVirtioGpu3D.cpp gives a context);
//      - STOPGAP: process names (og_default_background, og_default_front, or ONEIOS_GPU_BACKGROUND/_FRONT). This is the
//        owner's list of 2026-11-15 (WidgetRenderer, PosterBoard, the photo and poster extensions low), kept only until
//        the CPU agent's thread-group role covers them;
//      - the guest's hint: the submitting task's effective role (TASK_POLICY_ROLE, RunningBoard's), whether it is
//        DarwinBG, and the submitting thread's QoS (OneiOSAdreno.cpp oneios_gpu_hint, one byte each in the virtio
//        header's padding of every SUBMIT_3D and in CTX_CREATE's context_init bits 8-23). When the CPU agent publishes
//        a thread group's role, that function is the one place to read it (docs/agents.md).
// 2. The GPU's LEVEL (og_policy): KGSL lets a client ask for its highest level (KGSL_CONSTRAINT_PWR_MAX), its lowest
//    (PWR_MIN), or nothing (the governor decides). The top level is asked only for a frame that would miss its
//    deadline otherwise: the work interval's deadline when the guest sends one, else the frame cadence (8.3 ms at
//    120 Hz, at most 16.7 ms), judged by the frames' own GPU time (submission or the previous retirement, whichever is
//    later, to the KGSL timestamp's retirement), per level. Easy frames, and every frame of a background context,
//    leave the clock to the governor; background contexts get the lowest level while the screen is still.
#ifndef ONEIOS_GPU_POLICY_H
#define ONEIOS_GPU_POLICY_H
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include <string.h>

enum og_class { OG_CLASS_DEFAULT = 0, OG_CLASS_RENDER = 1, OG_CLASS_FRONT = 2, OG_CLASS_BACKGROUND = 3 };

static inline const char *og_class_name(enum og_class c)
{
  return c == OG_CLASS_RENDER ? "render server" : c == OG_CLASS_FRONT ? "front" : c == OG_CLASS_BACKGROUND ? "background" : "default";
}

// The guest's hint (OneiOSAdreno.cpp oneios_gpu_hint): byte 0 the task, byte 1 the thread's QoS, byte 2 the deadline.
#define OG_HINT_VALID    0x80u   // byte 0: the guest sent it (0: an older guest, no hint)
#define OG_HINT_DARWINBG 0x40u   // byte 0: the task is DarwinBG
#define OG_HINT_ROLE     0x0fu   // byte 0: the task's effective role, task_role_t (0-9)
#define OG_HINT_DEADLINE_US 250u // byte 2: time to the frame's deadline in 250 us units (0: none; 255: 63.75 ms or more)
// task_role_t (osfmk/mach/task_policy.h)
enum { OG_ROLE_UNSPECIFIED = 0, OG_ROLE_FOREGROUND = 1, OG_ROLE_BACKGROUND = 2, OG_ROLE_CONTROL = 3, OG_ROLE_GRAPHICS_SERVER = 4,
       OG_ROLE_THROTTLE = 5, OG_ROLE_NONUI = 6, OG_ROLE_DEFAULT = 7, OG_ROLE_DARWINBG = 8, OG_ROLE_USER_INIT = 9 };
// THREAD_QOS_* (osfmk/mach/thread_policy_private.h)
enum { OG_QOS_UNSPECIFIED = 0, OG_QOS_MAINTENANCE = 1, OG_QOS_BACKGROUND = 2, OG_QOS_UTILITY = 3, OG_QOS_LEGACY = 4,
       OG_QOS_USER_INITIATED = 5, OG_QOS_USER_INTERACTIVE = 6 };

// STOPGAP process names (p_name, what proc_selfname gives the context: at most 32 characters). Forms: a name,
// "X*" (a prefix), "*X*" (a part), separated by commas.
static const char og_default_background[] =
  "WidgetRenderer*,*Widget*,*Poster*,iconservices*,photoanalysisd,mediaanalysisd,assetsd,searchd,analyticsd,suggestd,*Thumbnail*";
static const char og_default_front[] = "SpringBoard";

// Whether `name` (n bytes, no "xnu:") matches one comma-separated item of `list`.
static inline bool og_name_listed(const char *name, size_t n, const char *list)
{
  for (const char *item = list; item && *item;) {
    const char *end = strchr(item, ',');
    size_t len = end ? (size_t)(end - item) : strlen(item);
    bool lead = len && item[0] == '*', trail = len > (size_t)lead && item[len - 1] == '*';
    const char *word = item + lead;
    size_t w = len - lead - trail;
    bool hit = false;
    if (w && lead) {                                         // *X*: a part (or *X: also a part)
      for (size_t i = 0; i + w <= n && !hit; i++) hit = !memcmp(name + i, word, w);
    } else if (w && trail) hit = n >= w && !memcmp(name, word, w);   // X*
    else if (w) hit = n == w && !memcmp(name, word, w);              // X
    if (hit) return true;
    item = end ? end + 1 : NULL;
  }
  return false;
}

// The class of a context named `name` (n bytes, as CTX_CREATE gave it: "xnu:<process>") whose submitting task sent
// `hint` (byte 0) and `qos` (byte 1). `background` and `front` are the stopgap lists (NULL: the defaults above).
static inline enum og_class og_class_of(const char *name, size_t n, uint8_t hint, uint8_t qos, const char *background, const char *front)
{
  if (n >= 4 && !memcmp(name, "xnu:", 4)) { name += 4; n -= 4; }
  if (n == 10 && !memcmp(name, "backboardd", 10)) return OG_CLASS_RENDER;
  if (og_name_listed(name, n, background ? background : og_default_background)) return OG_CLASS_BACKGROUND;   // STOPGAP
  if (og_name_listed(name, n, front ? front : og_default_front)) return OG_CLASS_FRONT;                     // STOPGAP
  if (!(hint & OG_HINT_VALID)) return OG_CLASS_DEFAULT;
  if (hint & OG_HINT_DARWINBG) return OG_CLASS_BACKGROUND;
  switch (hint & OG_HINT_ROLE) {
  case OG_ROLE_FOREGROUND: case OG_ROLE_CONTROL: case OG_ROLE_GRAPHICS_SERVER: case OG_ROLE_USER_INIT: return OG_CLASS_FRONT;
  case OG_ROLE_BACKGROUND: case OG_ROLE_THROTTLE: case OG_ROLE_NONUI: case OG_ROLE_DARWINBG: return OG_CLASS_BACKGROUND;
  default: break;
  }
  qos &= 7;
  return qos >= OG_QOS_MAINTENANCE && qos <= OG_QOS_UTILITY ? OG_CLASS_BACKGROUND : OG_CLASS_DEFAULT;
}

// The KGSL priority (0 highest, 15 lowest; four to a ringbuffer, KGSL_CONTEXT_PRIORITY_SHIFT 12) of a class's queues.
// None takes the first ringbuffer (0-3, Android's compositor's). Every class gets an explicit one, so the order holds
// whatever the kernel's default for KGSL_CONTEXT_PRIORITY_UNDEF is.
static inline unsigned og_kgsl_priority(enum og_class c)
{
  return c == OG_CLASS_RENDER || c == OG_CLASS_FRONT ? 4u : c == OG_CLASS_BACKGROUND ? 12u : 8u;
}

// ---- the level -----------------------------------------------------------------------------------------------------
enum og_level { OG_LEVEL_GOVERNOR = 0, OG_LEVEL_TOP = 1 };
enum og_mode { OG_MODE_DEADLINE = 0, OG_MODE_HOLD = 1, OG_MODE_GOVERNOR = 2 };   // gpu-level=deadline|hold|governor
// What the caller does for a frame: nothing new, hold the top level (until hold_ms after this frame), or let it go now.
enum og_action { OG_ACTION_NONE = 0, OG_ACTION_TOP = 1, OG_ACTION_GOVERNOR = 2 };

struct og_policy {
  // settings (og_policy_init's defaults)
  enum og_mode mode;
  uint32_t budget_us;       // the budget of a frame without a deadline, at 120 Hz: 8333
  uint32_t slow_budget_us;  // the longest cadence budget (60 Hz): 16667
  uint32_t ratio_x16;       // the top clock over the lowest, x16 (KGSL's gpu_available_frequencies; 64 = 4.0)
  uint32_t fit_pct;         // a frame fits when it takes at most this share of its budget: 80
  uint32_t down_frames;     // easy frames in a row at the top before the governor gets the clock back: 8
  uint32_t idle_us;         // a pause after which the next frame starts a burst (the governor is at its bottom): 100 ms
  uint32_t follow_us;       // another context's frames count within this of a render-server frame: 1 s
  uint32_t quiet_us;        // the screen still this long: background contexts at the lowest level: 500 ms
  // state
  enum og_level level;      // the level now
  enum og_level told;       // the level the caller was last told
  uint64_t last_render_ns, last_frame_ns, last_bg_min_ns;
  uint32_t interval_us;     // the render server's frame interval in a burst (average)
  uint32_t top_us, gov_us, cold_us;   // GPU time of a frame (average) at the top, at the governor's, at a burst's start
  uint32_t easy, down_need;
  // counters (since the start)
  uint64_t frames, frames_top, ups, downs, misses, late;
};

struct og_frame { uint8_t level, cold; uint32_t budget_us; };   // what a frame was given, for og_frame_done

static inline void og_policy_init(struct og_policy *p, enum og_mode mode)
{
  memset(p, 0, sizeof *p);
  p->mode = mode;
  p->budget_us = 8333;
  p->slow_budget_us = 16667;
  p->ratio_x16 = 64;
  p->fit_pct = 80;
  p->down_frames = p->down_need = 8;
  p->idle_us = 100000;
  p->follow_us = 1000000;
  p->quiet_us = 500000;
}

static inline uint32_t og_avg(uint32_t avg, uint32_t sample) { return avg ? (uint32_t)(((uint64_t)avg * 3 + sample + 2) / 4) : sample ? sample : 1; }
static inline uint32_t og_fit(const struct og_policy *p, uint32_t budget_us) { return (uint32_t)((uint64_t)budget_us * p->fit_pct / 100); }
static inline uint32_t og_at_bottom(const struct og_policy *p, uint32_t top_us) { return (uint32_t)((uint64_t)top_us * p->ratio_x16 / 16); }

// A fenced submission of a context of class `cls` is about to reach KGSL at `now_ns`; `deadline_us` is the time to its
// frame's deadline from the guest (0: none). Returns what to do with the level, and fills *f for og_frame_done (f->budget_us
// 0: the frame does not count).
static inline enum og_action og_frame_start(struct og_policy *p, enum og_class cls, uint64_t now_ns, uint32_t deadline_us, struct og_frame *f)
{
  *f = (struct og_frame){ 0 };
  if (cls == OG_CLASS_BACKGROUND || p->mode == OG_MODE_GOVERNOR) return OG_ACTION_NONE;
  if (cls != OG_CLASS_RENDER && (!p->last_render_ns || now_ns - p->last_render_ns > (uint64_t)p->follow_us * 1000)) return OG_ACTION_NONE;
  const uint64_t gap_us = p->last_frame_ns ? (now_ns - p->last_frame_ns) / 1000 : UINT64_MAX;
  const bool cold = gap_us >= p->idle_us;
  if (cls == OG_CLASS_RENDER) {
    if (!cold && p->last_render_ns) {
      const uint64_t since = (now_ns - p->last_render_ns) / 1000;
      if (since >= 500) p->interval_us = og_avg(p->interval_us, since > UINT32_MAX ? UINT32_MAX : (uint32_t)since);   // a second submission of one frame is not an interval
    }
    p->last_render_ns = now_ns;
  }
  p->last_frame_ns = now_ns;
  uint32_t budget = p->budget_us;
  if (deadline_us) budget = deadline_us < 1000 ? 1000 : deadline_us;
  else if (!cold && p->interval_us > budget) budget = p->interval_us < p->slow_budget_us ? p->interval_us : p->slow_budget_us;
  const uint32_t fit = og_fit(p, budget);
  enum og_level want = p->level;
  if (p->mode == OG_MODE_HOLD) want = OG_LEVEL_TOP;                     // 0005's rule: every frame at the top
  else if (cold) {
    // The governor sits at its lowest level after a pause: a frame known to take that long there gets the top.
    const uint32_t predict = p->cold_us ? p->cold_us : p->top_us ? og_at_bottom(p, p->top_us) : 0;
    want = !predict || predict > fit ? OG_LEVEL_TOP : OG_LEVEL_GOVERNOR;
    // Background contexts were at the lowest level just now: KGSL keeps that until it expires unless a faster one comes.
    if (p->last_bg_min_ns && now_ns - p->last_bg_min_ns < 200000000ull) want = OG_LEVEL_TOP;
    p->easy = 0;
    p->down_need = p->down_frames;
  } else if (p->level == OG_LEVEL_GOVERNOR && p->gov_us > fit) want = OG_LEVEL_TOP;
  if (want != p->level) { if (want == OG_LEVEL_TOP) p->ups++; else p->downs++; p->easy = 0; }
  p->level = want;
  p->frames++;
  if (want == OG_LEVEL_TOP) p->frames_top++;
  *f = (struct og_frame){ .level = (uint8_t)want, .cold = cold, .budget_us = budget };
  enum og_action action = want == OG_LEVEL_TOP ? OG_ACTION_TOP : p->told == OG_LEVEL_TOP ? OG_ACTION_GOVERNOR : OG_ACTION_NONE;
  p->told = want;
  return action;
}

// The frame `f` is done: `gpu_us` from its submission (or the previous retirement of its ring, if later) to its KGSL
// timestamp's retirement.
static inline void og_frame_done(struct og_policy *p, const struct og_frame *f, uint32_t gpu_us)
{
  if (!f->budget_us) return;
  const uint32_t fit = og_fit(p, f->budget_us);
  if (gpu_us > f->budget_us) p->late++;
  if (f->level == OG_LEVEL_TOP) {
    p->top_us = og_avg(p->top_us, gpu_us);
    if (p->mode != OG_MODE_DEADLINE) return;
    p->easy = og_at_bottom(p, gpu_us) <= fit ? p->easy + 1 : 0;
    if (p->level == OG_LEVEL_TOP && p->easy >= p->down_need) { p->level = OG_LEVEL_GOVERNOR; p->downs++; p->easy = 0; }
    return;
  }
  p->gov_us = og_avg(p->gov_us, gpu_us);
  if (f->cold) p->cold_us = og_avg(p->cold_us, gpu_us);
  if (gpu_us > fit) {
    // It (nearly) missed at the governor's clock: the next frames at the top, and longer before trying again.
    p->misses++;
    if (p->level != OG_LEVEL_TOP) { p->level = OG_LEVEL_TOP; p->ups++; }
    p->easy = 0;
    p->down_need = p->down_need < 64 ? p->down_need * 4 : 64;
  }
}

// Whether background contexts get the lowest level now: no counted frame for quiet_us (the screen is still).
static inline bool og_background_min(struct og_policy *p, uint64_t now_ns)
{
  if (p->mode != OG_MODE_DEADLINE || (p->last_frame_ns && now_ns - p->last_frame_ns < (uint64_t)p->quiet_us * 1000)) return false;
  p->last_bg_min_ns = now_ns;
  return true;
}
#endif
