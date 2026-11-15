// SPDX-License-Identifier: MIT
// The GPU's per-process policy (drm2kgsl/oneios_gpu_policy.h) on any host, no KGSL: the classes from names and the guest's
// hint, their KGSL priorities, and the level for frame streams of the kinds the phone draws (docs/gpu.md 2.2b). Then the
// same streams under the rule before 2026-11-15 (gpu-level=hold: every render-server frame at the top, to 150 ms after it)
// and under the deadline rule, side by side: the share of time the clock is held at the top and the frames over budget.
#include "oneios_gpu_policy.h"
#include <stdio.h>
#include <stdlib.h>

static unsigned checks, failed;
#define CHECK(x) do { checks++; if (!(x)) { failed++; printf("FAILED %s:%d: %s\n", __FILE__, __LINE__, #x); } } while (0)

static enum og_class cls(const char *name, uint8_t hint, uint8_t qos) { return og_class_of(name, strlen(name), hint, qos, NULL, NULL); }

static void classes(void)
{
  // The render server by name, whatever its role says.
  CHECK(cls("xnu:backboardd", 0, 0) == OG_CLASS_RENDER);
  CHECK(cls("xnu:backboardd", OG_HINT_VALID | OG_ROLE_BACKGROUND, OG_QOS_BACKGROUND) == OG_CLASS_RENDER);
  CHECK(cls("backboardd", 0, 0) == OG_CLASS_RENDER);
  // STOPGAP names: the owner's background list (p_name: at most 32 characters), also against a foreground role.
  CHECK(cls("xnu:WidgetRenderer_Default", 0, 0) == OG_CLASS_BACKGROUND);
  CHECK(cls("xnu:PosterBoard", OG_HINT_VALID | OG_ROLE_FOREGROUND, OG_QOS_USER_INTERACTIVE) == OG_CLASS_BACKGROUND);
  CHECK(cls("xnu:MercuryPosterExtension", 0, 0) == OG_CLASS_BACKGROUND);
  CHECK(cls("xnu:PhotosPosterProvider", 0, 0) == OG_CLASS_BACKGROUND);
  CHECK(cls("xnu:PhotosReliveWidget", 0, 0) == OG_CLASS_BACKGROUND);
  CHECK(cls("xnu:iconservicesagent", 0, 0) == OG_CLASS_BACKGROUND);
  CHECK(cls("xnu:assetsd", 0, 0) == OG_CLASS_BACKGROUND && cls("xnu:searchd", 0, 0) == OG_CLASS_BACKGROUND);
  CHECK(cls("xnu:analyticsd", 0, 0) == OG_CLASS_BACKGROUND);
  CHECK(cls("xnu:assetsdx", 0, 0) == OG_CLASS_DEFAULT);       // a name is the whole name
  CHECK(cls("xnu:SpringBoard", 0, 0) == OG_CLASS_FRONT);
  // No name rule: the guest's role, then the thread's QoS.
  CHECK(cls("xnu:MobileSafari", 0, 0) == OG_CLASS_DEFAULT);                                       // an older guest
  CHECK(cls("xnu:MobileSafari", OG_HINT_VALID | OG_ROLE_FOREGROUND, OG_QOS_USER_INTERACTIVE) == OG_CLASS_FRONT);
  CHECK(cls("xnu:MobileSafari", OG_HINT_VALID | OG_ROLE_BACKGROUND, OG_QOS_USER_INTERACTIVE) == OG_CLASS_BACKGROUND);
  CHECK(cls("xnu:Preferences", OG_HINT_VALID | OG_ROLE_FOREGROUND | OG_HINT_DARWINBG, 0) == OG_CLASS_BACKGROUND);
  CHECK(cls("xnu:mediaserverd", OG_HINT_VALID | OG_ROLE_UNSPECIFIED, OG_QOS_UTILITY) == OG_CLASS_BACKGROUND);
  CHECK(cls("xnu:mediaserverd", OG_HINT_VALID | OG_ROLE_UNSPECIFIED, OG_QOS_USER_INITIATED) == OG_CLASS_DEFAULT);
  CHECK(cls("xnu:mediaserverd", OG_HINT_VALID | OG_ROLE_DEFAULT, OG_QOS_UNSPECIFIED) == OG_CLASS_DEFAULT);
  CHECK(cls("xnu:Extension", OG_HINT_VALID | OG_ROLE_NONUI, OG_QOS_USER_INITIATED) == OG_CLASS_BACKGROUND);
  CHECK(cls("xnu:x", OG_HINT_VALID | OG_ROLE_GRAPHICS_SERVER, 0) == OG_CLASS_FRONT);
  // Lists of one's own replace the defaults.
  CHECK(og_class_of("xnu:Foo", 7, 0, 0, "Foo,Bar*", "") == OG_CLASS_BACKGROUND);
  CHECK(og_class_of("xnu:Barn", 8, 0, 0, "Foo,Bar*", "") == OG_CLASS_BACKGROUND);
  CHECK(og_class_of("xnu:PosterBoard", 15, 0, 0, "Foo", "") == OG_CLASS_DEFAULT);
  CHECK(og_class_of("xnu:SpringBoard", 15, 0, 0, "", "") == OG_CLASS_DEFAULT);
  CHECK(og_name_listed("abc", 3, "*b*") && og_name_listed("abc", 3, "*c") && !og_name_listed("abc", 3, "*d*") && !og_name_listed("ab", 2, "abc*"));
  CHECK(!og_name_listed("abc", 3, "") && !og_name_listed("abc", 3, ",,") && og_name_listed("abc", 3, ",abc,"));
  // Priorities: the render server and the front app above the default, background below; never KGSL's first ringbuffer.
  CHECK(og_kgsl_priority(OG_CLASS_RENDER) == 4 && og_kgsl_priority(OG_CLASS_FRONT) == 4);
  CHECK(og_kgsl_priority(OG_CLASS_DEFAULT) == 8 && og_kgsl_priority(OG_CLASS_BACKGROUND) == 12);
  for (unsigned c = 0; c < 4; c++) CHECK(og_kgsl_priority((enum og_class)c) >= 4 && og_kgsl_priority((enum og_class)c) <= 15);
}

#define MS 1000000ull
#define US 1000ull

// One frame: start, then done after `gpu_us`. Returns the action.
static enum og_action frame(struct og_policy *p, enum og_class c, uint64_t at, uint32_t deadline_us, uint32_t gpu_us)
{
  struct og_frame f;
  enum og_action a = og_frame_start(p, c, at, deadline_us, &f);
  og_frame_done(p, &f, gpu_us);
  return a;
}

static void levels(void)
{
  struct og_policy p;
  // A first frame (nothing known): the top. Easy frames at 120 Hz (0.3 ms at the top, 1.2 at the bottom): after 8 the governor.
  og_policy_init(&p, OG_MODE_DEADLINE);
  CHECK(frame(&p, OG_CLASS_RENDER, 1000 * MS, 0, 300) == OG_ACTION_TOP);
  uint64_t t = 1000 * MS;
  unsigned tops = 1;
  enum og_action a = OG_ACTION_NONE;
  for (unsigned i = 0; i < 7; i++) { t += 8333 * US; a = frame(&p, OG_CLASS_RENDER, t, 0, 300); tops += a == OG_ACTION_TOP; }
  CHECK(tops == 8 && p.level == OG_LEVEL_GOVERNOR);
  t += 8333 * US;
  CHECK(frame(&p, OG_CLASS_RENDER, t, 0, 1200) == OG_ACTION_GOVERNOR);       // the hold stops at once
  t += 8333 * US;
  CHECK(frame(&p, OG_CLASS_RENDER, t, 0, 1200) == OG_ACTION_NONE && p.level == OG_LEVEL_GOVERNOR);
  // A frame that nearly misses at the governor's clock (7 ms of 8.3, more than 80 %): the next ones at the top, and longer.
  t += 8333 * US;
  CHECK(frame(&p, OG_CLASS_RENDER, t, 0, 7000) == OG_ACTION_NONE && p.level == OG_LEVEL_TOP && p.misses == 1);
  t += 8333 * US;
  CHECK(frame(&p, OG_CLASS_RENDER, t, 0, 300) == OG_ACTION_TOP);
  CHECK(p.down_need == 32);
  for (unsigned i = 0; i < 31; i++) { t += 8333 * US; frame(&p, OG_CLASS_RENDER, t, 0, 300); }
  CHECK(p.level == OG_LEVEL_GOVERNOR);

  // Heavy frames at 120 Hz (3 ms at the top: 12 at the bottom, over 6.7): the top, for as long as they come.
  og_policy_init(&p, OG_MODE_DEADLINE);
  t = 5000 * MS;
  for (unsigned i = 0; i < 200; i++) { t += 8333 * US; frame(&p, OG_CLASS_RENDER, t, 0, 3000); }
  CHECK(p.level == OG_LEVEL_TOP && p.frames_top == 200 && p.late == 0);
  // The same frames at 60 Hz fit at the bottom (12 ms of 16.7): the governor's after 8.
  og_policy_init(&p, OG_MODE_DEADLINE);
  for (unsigned i = 0; i < 100; i++) { t += 16667 * US; frame(&p, OG_CLASS_RENDER, t, 0, p.level == OG_LEVEL_TOP ? 3000 : 12000); }
  CHECK(p.level == OG_LEVEL_GOVERNOR && p.frames_top < 12 && p.misses == 0 && p.interval_us > 16000 && p.interval_us < 17000);

  // After a pause the governor is at its bottom: a frame known to be easy there stays with it (a clock tick), a heavy one not.
  og_policy_init(&p, OG_MODE_DEADLINE);
  p.cold_us = 1000;
  t = 9000 * MS;
  CHECK(frame(&p, OG_CLASS_RENDER, t, 0, 1000) == OG_ACTION_NONE && p.level == OG_LEVEL_GOVERNOR);
  t += 1000 * MS;
  CHECK(frame(&p, OG_CLASS_RENDER, t, 0, 1000) == OG_ACTION_NONE);
  p.cold_us = 20000;
  t += 1000 * MS;
  CHECK(frame(&p, OG_CLASS_RENDER, t, 0, 3000) == OG_ACTION_TOP);
  // Unknown at the bottom but known at the top: x4 (KGSL's clock ratio) decides.
  og_policy_init(&p, OG_MODE_DEADLINE);
  p.top_us = 400;                                                            // 1.6 ms at the bottom: fits
  CHECK(frame(&p, OG_CLASS_RENDER, 20000 * MS, 0, 1600) == OG_ACTION_NONE);
  og_policy_init(&p, OG_MODE_DEADLINE);
  p.top_us = 2500;                                                           // 10 ms at the bottom: misses
  CHECK(frame(&p, OG_CLASS_RENDER, 20000 * MS, 0, 2500) == OG_ACTION_TOP);
  og_policy_init(&p, OG_MODE_DEADLINE);
  p.ratio_x16 = 16 * 903 / 231;                                              // KGSL's own list (903 and 231 MHz)
  p.top_us = 2000;
  CHECK(p.ratio_x16 == 62 && og_at_bottom(&p, 2000) == 7750);
  CHECK(frame(&p, OG_CLASS_RENDER, 20000 * MS, 0, 2000) == OG_ACTION_TOP);

  // The guest's deadline (the work interval's, from the CPU agent's signal) is the budget when it comes.
  og_policy_init(&p, OG_MODE_DEADLINE);
  p.cold_us = 12000;
  CHECK(frame(&p, OG_CLASS_RENDER, 30000 * MS, 30000, 12000) == OG_ACTION_NONE);   // 12 ms fits 30
  CHECK(frame(&p, OG_CLASS_RENDER, 31000 * MS, 8000, 12000) == OG_ACTION_TOP);     // not 8
  struct og_frame f;
  og_frame_start(&p, OG_CLASS_RENDER, 32000 * MS, 200, &f);
  CHECK(f.budget_us == 1000);                                                      // at least 1 ms
  og_frame_start(&p, OG_CLASS_RENDER, 32000 * MS + 10 * MS, 0, &f);
  CHECK(f.budget_us == 10000);                                                     // the cadence: 10 ms apart

  // Background contexts never count and never take the top; the lowest level for them once the screen is still 500 ms;
  // the first render-server frame after that gets the top (KGSL keeps the lowest until it expires otherwise).
  og_policy_init(&p, OG_MODE_DEADLINE);
  p.cold_us = 500;
  t = 40000 * MS;
  CHECK(frame(&p, OG_CLASS_RENDER, t, 0, 500) == OG_ACTION_NONE);
  CHECK(!og_background_min(&p, t + 100 * MS));
  CHECK(frame(&p, OG_CLASS_BACKGROUND, t + 100 * MS, 0, 50000) == OG_ACTION_NONE && p.frames == 1 && p.misses == 0);
  CHECK(og_background_min(&p, t + 600 * MS));
  CHECK(frame(&p, OG_CLASS_RENDER, t + 700 * MS, 0, 500) == OG_ACTION_TOP);
  CHECK(frame(&p, OG_CLASS_RENDER, t + 1500 * MS, 0, 500) == OG_ACTION_GOVERNOR);  // no background at the lowest since
  // Other contexts count within a second of the render server's last frame, and only then.
  og_policy_init(&p, OG_MODE_DEADLINE);
  CHECK(frame(&p, OG_CLASS_DEFAULT, 50000 * MS, 0, 30000) == OG_ACTION_NONE && p.frames == 0);
  frame(&p, OG_CLASS_RENDER, 50001 * MS, 0, 300);
  CHECK(frame(&p, OG_CLASS_FRONT, 50300 * MS, 0, 300) == OG_ACTION_GOVERNOR && p.frames == 2);   // easy: the hold goes
  CHECK(frame(&p, OG_CLASS_FRONT, 52000 * MS, 0, 300) == OG_ACTION_NONE && p.frames == 2);
  // The other modes.
  og_policy_init(&p, OG_MODE_HOLD);
  for (unsigned i = 0; i < 20; i++) CHECK(frame(&p, OG_CLASS_RENDER, 60000 * MS + i * 8333 * US, 0, 100) == OG_ACTION_TOP);
  CHECK(frame(&p, OG_CLASS_BACKGROUND, 61000 * MS, 0, 100) == OG_ACTION_NONE);
  og_policy_init(&p, OG_MODE_GOVERNOR);
  for (unsigned i = 0; i < 20; i++) CHECK(frame(&p, OG_CLASS_RENDER, 60000 * MS + i * 8333 * US, 0, 9000) == OG_ACTION_NONE);
}

// ---- before and after: the same streams under gpu-level=hold (0005/0006) and =deadline -------------------------------
// A stream: render-server frames at `hz` for `seconds` in bursts of `burst_ms` every `every_ms` (0: one burst), each frame
// `top_us` of GPU time at the top level and `ratio` times that at the governor's (its bottom, as after a pause).
typedef struct { const char *what; double hz, seconds, burst_ms, every_ms; uint32_t top_us; double ratio; } stream;

static void run(const stream *s, enum og_mode mode, double *held_pct, unsigned *late, unsigned *frames)
{
  struct og_policy p;
  og_policy_init(&p, mode);
  const uint64_t hold_ns = 150 * MS, period = (uint64_t)(1e9 / s->hz), end = (uint64_t)(s->seconds * 1e9), start0 = 100000 * MS;
  uint64_t held = 0, hold_until = 0;
  *late = *frames = 0;
  for (uint64_t at = 0; at < end; at += period) {
    if (s->every_ms > 0 && (double)(at % (uint64_t)(s->every_ms * 1e6)) >= s->burst_ms * 1e6) continue;
    const uint64_t now = start0 + at;
    struct og_frame f;
    enum og_action a = og_frame_start(&p, OG_CLASS_RENDER, now, 0, &f);
    if (a == OG_ACTION_GOVERNOR && hold_until > now) hold_until = now;
    if (a == OG_ACTION_TOP) {
      const uint64_t from = hold_until > now ? hold_until : now;
      held += now + hold_ns - from;
      hold_until = now + hold_ns;
    }
    // The frame runs at the top while the clock is held (this frame's or an earlier one's), else at the governor's bottom.
    const bool top = hold_until > now;
    const uint32_t gpu = top ? s->top_us : (uint32_t)(s->top_us * s->ratio);
    if (gpu > (period < 16667 * US ? period / 1000 : 16667)) (*late)++;
    (*frames)++;
    og_frame_done(&p, &(struct og_frame){ .level = top ? OG_LEVEL_TOP : OG_LEVEL_GOVERNOR, .cold = f.cold, .budget_us = f.budget_us }, gpu);
  }
  if (hold_until > start0 + end) held -= hold_until - (start0 + end);
  *held_pct = 100.0 * (double)held / (double)end;
}

static void before_after(void)
{
  static const stream streams[] = {
    { "still lock screen redrawing (45 frames/s, 0.4 ms each)", 45, 10, 0, 0, 400, 4 },
    { "clock tick (1 frame/s, 0.3 ms)", 1, 10, 1, 1000, 300, 4 },
    { "typing (a key every 150 ms: 3 frames at 120 Hz, 1 ms)", 120, 10, 25, 150, 1000, 4 },
    { "scrolling (120 frames/s, 3 ms each)", 120, 10, 0, 0, 3000, 4 },
    { "a light animation (60 frames/s, 0.8 ms)", 60, 10, 0, 0, 800, 4 },
  };
  printf("%-58s %22s %22s\n", "stream (10 s)", "hold: top / late", "deadline: top / late");
  for (unsigned i = 0; i < sizeof streams / sizeof streams[0]; i++) {
    double held[2];
    unsigned late[2], frames[2];
    run(&streams[i], OG_MODE_HOLD, &held[0], &late[0], &frames[0]);
    run(&streams[i], OG_MODE_DEADLINE, &held[1], &late[1], &frames[1]);
    printf("%-58s %10.1f %% %4u/%-5u %10.1f %% %4u/%-5u\n", streams[i].what, held[0], late[0], frames[0], held[1], late[1], frames[1]);
    CHECK(held[1] <= held[0] + 0.01);
    // Heavy frames keep the top; nothing is later than before by more than the one frame that teaches the policy.
    if (streams[i].top_us * streams[i].ratio > 8333 * 0.8 && streams[i].hz >= 100 && streams[i].every_ms == 0) CHECK(held[1] > 95);
    CHECK(late[1] <= late[0] + 1 + frames[1] / 40);
  }
}

int main(void)
{
  classes();
  levels();
  before_after();
  printf("test_gpu_policy: %u checks, %u failed\n", checks, failed);
  return failed ? 1 : 0;
}
