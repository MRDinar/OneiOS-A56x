// SPDX-License-Identifier: MIT
// Host test of the plug-in's stream formats (oneios_camera_formats.c: CMCapture 24A446's resolver rule) and of its
// scalers (oneios_camera_scale.c). The named formats are the numbers of V54's AVCaptureSession.plist stream formats
// (iPhone18,2, 24A446), written out by hand: no Apple file is read.
#include "oneios_camera_formats.h"
#include "OneiOSCamera.h"
#include <stdio.h>
#include <string.h>

static int failures, checks;
#define CHECK(cond) do { checks++; if (!(cond)) { failures++; printf("FAIL %s:%d: %s\n", __FILE__, __LINE__, #cond); } } while (0)

// V54's Back Camera stream formats that failed on the phone on 2026-11-23 ("failed to resolve format ..."), and companions
static const struct { const char *name; struct ocf_params p; } named[] = {
  { "Video-16by9-Binned-60", { .sensor_w = 2112, .sensor_h = 1188, .binned = true, .max_fps = 60, .sifr = true } },
  { "Video-16by9-Binned-FES-60", { .sensor_w = 2112, .sensor_h = 1188, .binned = true, .fes = true, .fes_w = 1536, .fes_h = 864,
                                   .max_fps = 60, .sifr = true } },
  { "Video-4by3-Low-Resolution", { .sensor_w = 4224, .sensor_h = 3024, .fes = true, .fes_w = 1440, .fes_h = 1080, .max_fps = 60,
                                   .sifr = true, .sifr_bf = 2 } },
  { "Video-4by3-Medium-Resolution", { .sensor_w = 4224, .sensor_h = 3024, .fes = true, .fes_w = 2464, .fes_h = 1764, .max_fps = 60,
                                      .sifr = true, .sifr_bf = 2 } },
  { "Photo", { .sensor_w = 4224, .sensor_h = 3024, .max_fps = 60, .sifr = true } },
  // Photo[UltraHighResCompanionFormat]: Photo's keys with the companion's over them
  { "Photo[UltraHighResCompanionFormat]", { .sensor_w = 8448, .sensor_h = 6048, .fes = true, .fes_w = 2464, .fes_h = 1764, .max_fps = 30,
                                            .sifr = true, .qbf = 1 } },
  { "Video-30[MidFrameSynchronizationOptimizedCompanionFormat]", { .sensor_w = 4224, .sensor_h = 2376, .max_fps = 30, .sifr = true,
                                                                   .mid_sync = true } },
  { "HDR", { .sensor_w = 4224, .sensor_h = 2376, .hdr = true, .max_fps = 30 } },
};
enum { N = sizeof named / sizeof named[0] };

static void formats(void)
{
  struct ocf_format f[N];
  for (int i = 0; i < N; i++) {
    ocf_format_for(&named[i].p, &f[i]);
    CHECK(ocf_matches(&named[i].p, &f[i]));
  }
  // each name resolves to its own format: the binned FES one is not the binned one, the medium FES not the low one
  for (int i = 0; i < N; i++) {
    int at = ocf_resolve(&named[i].p, f, N);
    if (at != i) printf("FAIL: %s resolves to %d (%s)\n", named[i].name, at, at >= 0 ? named[at].name : "none");
    CHECK(at == i);
  }
  // the formats of 2026-11-23: no front-end scaler output, no SIFR factor (the plist's keys copied as they were)
  struct ocf_format old = f[1];
  old.fes_out_w = old.fes_out_h = 0;
  CHECK(!ocf_matches(&named[1].p, &old));                   // FES asked, the format had none
  old = f[0];
  old.sifr_bf = 0;
  CHECK(!ocf_matches(&named[0].p, &old));                   // binned SIFR: factor 2 asked, the format said 1
  old = f[2];
  old.sifr_bf = 0;
  CHECK(!ocf_matches(&named[2].p, &old));
  // the compares one by one
  struct ocf_format g = f[4];
  g.max_fps = 30;
  CHECK(!ocf_matches(&named[4].p, &g));                     // MaxFrameRate 60 asked
  struct ocf_params any = named[4].p;
  any.max_fps = 0;
  CHECK(ocf_matches(&any, &g));                             // no MaxFrameRate: any rate
  g = f[4];
  g.experimental = true;
  CHECK(!ocf_matches(&named[4].p, &g));                     // an experimental format is skipped
  CHECK(ocf_resolve(&(struct ocf_params){ .sensor_w = 4224, .sensor_h = 3024, .max_fps = 60, .sifr = true, .unsupported = true }, f, N) == -1);
  g = f[5];
  g.qbf = 0;
  CHECK(!ocf_matches(&named[5].p, &g));                     // quadra binning 1 asked, none given
  struct ocf_params q2 = named[4].p;
  q2.qbf = 0;
  g = f[4];
  g.qbf = 2;
  CHECK(ocf_matches(&q2, &g));                              // a format's factor 2 is what a name without one means
  g = f[7];
  g.hdr_mode = 0;
  CHECK(!ocf_matches(&named[7].p, &g));                     // HDR asked, SensorHDRMode 0
  g = f[6];
  g.mid_sync = false;
  CHECK(!ocf_matches(&named[6].p, &g));
  g = f[4];
  g.signing = true;
  CHECK(!ocf_matches(&named[4].p, &g));                     // SensorSigningEnabled against SecureSigningFaceIDSupported
  struct ocf_params fes_any = { .sensor_w = 4224, .sensor_h = 3024, .fes = true, .max_fps = 30 };
  ocf_format_for(&fes_any, &g);
  CHECK(ocf_matches(&fes_any, &g));                         // FES with no size: any scaler output
  CHECK(!ocf_same(&f[0], &f[1]) && ocf_same(&f[0], &f[0]));
}

// The companions as CMCapture 24A446 makes them (ocf_companion_rule), on V54's Back Camera numbers written out by hand.
static struct ocf_params marked(struct ocf_params p) { ocf_mark(&p); return p; }

static struct ocf_params companion_of(const char *key, struct ocf_params parent, struct ocf_params comp)
{
  struct ocf_params out;
  const struct ocf_companion *c = ocf_companion_rule(key);
  CHECK(c != NULL);
  if (c == NULL) return parent;
  ocf_companion_params(c, &parent, &comp, &out);
  return out;
}

// The plug-in's companion before the fix of 2026-11-23's boot: the companion's keys laid over the parent's, nothing else
// (by hand: ocf_companion_params is the rule under test).
static struct ocf_params laid_over(struct ocf_params parent, struct ocf_params comp)
{
  if (comp.has & OCF_HAS_SENSOR_W) parent.sensor_w = comp.sensor_w;
  if (comp.has & OCF_HAS_SENSOR_H) parent.sensor_h = comp.sensor_h;
  if (comp.has & OCF_HAS_FES) parent.fes = comp.fes;
  if (comp.has & OCF_HAS_FES_W) parent.fes_w = comp.fes_w;
  if (comp.has & OCF_HAS_FES_H) parent.fes_h = comp.fes_h;
  if (comp.has & OCF_HAS_MAX_FPS) parent.max_fps = comp.max_fps;
  if (comp.has & OCF_HAS_SIFR_BF) parent.sifr_bf = comp.sifr_bf;
  if (comp.has & OCF_HAS_QBF) parent.qbf = comp.qbf;
  return parent;
}

static bool same_params(const struct ocf_params *a, const struct ocf_params *b)
{
  return a->sensor_w == b->sensor_w && a->sensor_h == b->sensor_h && a->fes == b->fes && a->fes_w == b->fes_w && a->fes_h == b->fes_h &&
         a->binned == b->binned && a->hdr == b->hdr && a->max_fps == b->max_fps && a->sifr == b->sifr && a->sifr_bf == b->sifr_bf &&
         a->qbf == b->qbf && a->mid_sync == b->mid_sync && a->signing == b->signing;
}

static void companions(void)
{
  // V54 Back Camera (24A446 AVCaptureSession.plist), the keys the resolver reads
  const struct ocf_params photo = marked((struct ocf_params){ .sensor_w = 4224, .sensor_h = 3024, .max_fps = 60, .sifr = true });
  const struct ocf_params video30 = marked((struct ocf_params){ .sensor_w = 4224, .sensor_h = 2640, .max_fps = 30, .sifr = true, .sifr_bf = 2 });
  const struct ocf_params full_fes30 = marked((struct ocf_params){ .sensor_w = 4224, .sensor_h = 3024, .fes = true, .fes_w = 2464, .fes_h = 1764,
                                                                  .max_fps = 30, .sifr = true, .sifr_bf = 2 });
  const struct ocf_params fes60 = marked((struct ocf_params){ .sensor_w = 4224, .sensor_h = 2640, .fes = true, .fes_w = 2464, .fes_h = 1540,
                                                             .max_fps = 60, .sifr = true, .sifr_bf = 2 });
  const struct ocf_params none = { 0 };
  const struct ocf_params fes_comp = marked((struct ocf_params){ .max_fps = 30, .fes_w = 2464, .fes_h = 1764 });
  const struct ocf_params mid_comp = marked((struct ocf_params){ .sensor_w = 4224, .sensor_h = 2640, .fes = true, .fes_w = 2464, .fes_h = 1540 });
  const struct ocf_params action_comp = marked((struct ocf_params){ .sensor_w = 4224, .sensor_h = 3024 });
  const struct ocf_params depth_comp = marked((struct ocf_params){ .fes = true, .fes_w = 1920, .fes_h = 1440 });

  // Photo[FESCompanionFormat]: FES set (the companion names only its size and rate), the rate the companion's
  struct ocf_params p = companion_of("FESCompanionFormats", photo, fes_comp);
  struct ocf_params want = marked((struct ocf_params){ .sensor_w = 4224, .sensor_h = 3024, .fes = true, .fes_w = 2464, .fes_h = 1764,
                                                      .max_fps = 30, .sifr = true });
  CHECK(same_params(&p, &want));
  struct ocf_format old, now;
  struct ocf_params old_p = laid_over(photo, fes_comp);
  ocf_format_for(&old_p, &old);
  CHECK(!ocf_matches(&p, &old));                            // 2026-11-23: "failed to resolve format 'Photo[FESCompanionFormat]'"
  ocf_format_for(&p, &now);
  CHECK(ocf_matches(&p, &now));
  // an FES companion without a rate takes the parent's away (nil set): any rate then
  p = companion_of("FESCompanionFormats", photo, marked((struct ocf_params){ .fes_w = 2496, .fes_h = 2496 }));
  CHECK(p.max_fps == 0 && !(p.has & OCF_HAS_MAX_FPS) && p.fes);

  // Video-30[MidFrameSynchronizationOptimizedCompanionFormat]: an empty companion; OptimizedForMidFrameSynchronization set
  p = companion_of("MidFrameSynchronizationOptimizedCompanionFormat", video30, none);
  want = video30;
  want.mid_sync = true;
  CHECK(same_params(&p, &want));
  old_p = laid_over(video30, none);
  ocf_format_for(&old_p, &old);
  CHECK(!ocf_matches(&p, &old));                            // 2026-11-23: the plug-in's was Video-30's own format
  // Video-FullSensor-FES-30[MidFrame...]: the companion's sensor and scaler sizes, the parent's rate and SIFR factor
  p = companion_of("MidFrameSynchronizationOptimizedCompanionFormat", full_fes30, mid_comp);
  want = marked((struct ocf_params){ .sensor_w = 4224, .sensor_h = 2640, .fes = true, .fes_w = 2464, .fes_h = 1540, .max_fps = 30,
                                    .sifr = true, .sifr_bf = 2, .mid_sync = true });
  CHECK(same_params(&p, &want));
  old_p = laid_over(full_fes30, mid_comp);
  ocf_format_for(&old_p, &old);
  CHECK(!ocf_matches(&p, &old));
  CHECK(ocf_companion_rule("MidFrameSynchronizationOptimizedCompanionFormat")->skip_unsupported);

  // Video-FES-60[ActionCameraCompanionFormat]: the full sensor, FES taken away
  p = companion_of("ActionCameraCompanionFormat", fes60, action_comp);
  CHECK(!p.fes && p.sensor_h == 3024 && p.max_fps == 60 && p.sifr_bf == 2);
  // Photo[FullBinSIFRCompanionFormat]: SIFR factor 2
  p = companion_of("FullBinSIFRCompanionFormat", photo, none);
  CHECK(p.sifr && p.sifr_bf == 2 && p.max_fps == 60);
  // Photo[DepthCompanionFormat]: FES with the companion's size, the parent's rate
  p = companion_of("DepthCompanionFormat", photo, depth_comp);
  CHECK(p.fes && p.fes_w == 1920 && p.fes_h == 1440 && p.max_fps == 60 && p.sensor_w == 4224);
  p = companion_of("FSDNetCompanionFormat", photo, depth_comp);
  CHECK(p.fes && p.fes_w == 1920);
  // NonSIFR: SIFR off and its factor gone; FES, its size and the rate are the companion's (none: gone)
  p = companion_of("NonSIFRCompanionFormat", fes60, none);
  CHECK(!p.sifr && p.sifr_bf == 0 && !p.fes && p.fes_w == 0 && p.max_fps == 0 && p.sensor_w == 4224);
  // StereoVideo: no FES
  p = companion_of("StereoVideoCompanionFormat", fes60, none);
  CHECK(!p.fes && p.fes_w == 0 && p.max_fps == 60);
  // FaceID and the sensor HDR flag
  p = companion_of("FaceIDCompanionFormat", photo, none);
  CHECK(p.signing);
  const struct ocf_companion *hdr = ocf_companion_rule("HasSensorHDRCompanionFormat");
  CHECK(hdr != NULL && hdr->flag && strcmp(hdr->suffix, "[SensorHDRCompanionFormat]") == 0);
  p = companion_of("HasSensorHDRCompanionFormat", photo, none);
  CHECK(p.hdr);
  // the names CMCapture logs
  CHECK(strcmp(ocf_companion_rule("FESCompanionFormats")->suffix, "[FESCompanionFormat]") == 0);
  CHECK(ocf_companion_rule("UltraHighResCompanionFormat") == NULL);   // not one CMCapture resolves there
  CHECK(ocf_key_bit("FESWidth") == OCF_HAS_FES_W && ocf_key_bit("SensorCropWidth") == 0);

  // The plug-in's way: one format per distinct set of compared keys, the named ones then the companions; every name resolves
  const struct ocf_params all[] = {
    photo, video30, full_fes30, fes60,
    companion_of("FESCompanionFormats", photo, fes_comp),
    companion_of("MidFrameSynchronizationOptimizedCompanionFormat", video30, none),
    companion_of("MidFrameSynchronizationOptimizedCompanionFormat", full_fes30, mid_comp),
    companion_of("ActionCameraCompanionFormat", fes60, action_comp),
    companion_of("FullBinSIFRCompanionFormat", photo, none),
    companion_of("DepthCompanionFormat", photo, depth_comp),
  };
  enum { NA = sizeof all / sizeof all[0] };
  struct ocf_format fs[NA];
  int n = 0;
  for (int i = 0; i < NA; i++) {
    struct ocf_format f;
    bool known = false;
    ocf_format_for(&all[i], &f);
    for (int k = 0; k < n && !known; k++) known = ocf_same(&fs[k], &f);
    if (!known) fs[n++] = f;
  }
  for (int i = 0; i < NA; i++) {
    int at = ocf_resolve(&all[i], fs, n);
    if (at < 0) printf("FAIL: companion %d resolves to none\n", i);
    CHECK(at >= 0);
  }
}

// The front stream: V54's front devices keep their named formats in their device info bases, and every device on the
// PortTypeFrontSuperWide port resolves its own against the one stream (numbers written out by hand).
static void front_devices(void)
{
  // FrontUltraWideCamera-DeviceInfoBase.plist and the session plist's own entry of "Front Ultra Wide Camera" (ProRes RAW)
  const struct ocf_params fuw_16x10_fes30 = { .sensor_w = 4896, .sensor_h = 4896, .fes = true, .fes_w = 2496, .fes_h = 2496, .max_fps = 30,
                                              .sifr = true, .sifr_bf = 2 };
  const struct ocf_params fuw_photo = { .sensor_w = 4896, .sensor_h = 4896, .fes = true, .fes_w = 4608, .fes_h = 4608, .sifr = true };
  const struct ocf_params fuw_binned30 = { .sensor_w = 2448, .sensor_h = 2448, .binned = true, .fes = true, .fes_w = 2448, .fes_h = 2448,
                                           .max_fps = 30, .sifr = true };
  const struct ocf_params fuw_raw30 = { .sensor_w = 4896, .sensor_h = 4896, .fes = true, .fes_w = 4224, .fes_h = 3024, .max_fps = 30,
                                        .sifr = true, .sifr_bf = 2 };
  // FrontVirtualCamera-DeviceInfoBase.plist ("Front Camera", the Camera app's front camera)
  const struct ocf_params fv_fes60 = { .sensor_w = 3072, .sensor_h = 4896, .fes = true, .fes_w = 1320, .fes_h = 2112, .max_fps = 60 };
  const struct ocf_params fv_photo = { .sensor_w = 4896, .sensor_h = 4896, .fes = true, .fes_w = 4032, .fes_h = 4032, .sifr = true };
  // FrontMetadataCamera-DeviceInfoBase.plist
  const struct ocf_params meta_binned30 = { .sensor_w = 2448, .sensor_h = 2448, .binned = true, .max_fps = 30, .sifr = true };
  const struct ocf_params *every[] = { &fuw_16x10_fes30, &fuw_photo, &fuw_binned30, &fuw_raw30, &fv_fes60, &fv_photo, &meta_binned30 };
  enum { NE = sizeof every / sizeof every[0] };
  struct ocf_format fs[NE], raw_only;
  for (int i = 0; i < NE; i++) ocf_format_for(every[i], &fs[i]);
  // 2026-11-23: the stream had the session plist's ProRes RAW formats only ("failed to resolve format 'Video-16x10-FES-30'",
  // and every format of "Front Camera")
  ocf_format_for(&fuw_raw30, &raw_only);
  CHECK(ocf_resolve(&fuw_16x10_fes30, &raw_only, 1) == -1 && ocf_resolve(&fv_fes60, &raw_only, 1) == -1);
  // the three devices' formats on the one stream: each name resolves to its own, "Photo" of each device to its own scaler size
  for (int i = 0; i < NE; i++) {
    int at = ocf_resolve(every[i], fs, NE);
    if (at != i) printf("FAIL: front format %d resolves to %d\n", i, at);
    CHECK(at == i);
  }
}

static void white_balance(void)
{
  // CMCapture reads each gain as an integer with 12 fraction bits; AVFoundation's gains are 1 and above
  CHECK(ocf_wb_gain(OCF_WB_GAIN_ONE) == 1.0);
  CHECK(ocf_wb_gain(2 * OCF_WB_GAIN_ONE + OCF_WB_GAIN_ONE / 2) == 2.5);
}

static void scalers(void)
{
  enum { W = 8, H = 4 };
  static uint8_t picture[W * H * 3 / 2];
  struct oneios_camera_frame h = { .width = W, .height = H, .stride = W };
  memset(picture, 255, W * H);                              // white luma
  memset(picture + W * H, 128, W * H / 2);                  // grey chroma
  picture[0] = 0;                                           // one black pixel
  // x420: 16-bit samples, the 10 bits at the top, video range
  static uint8_t y16[H][2 * W], c16[H][2 * W];
  CHECK(oneios_camera_scale_10bit(&h, picture, &y16[0][0], 2 * W, &c16[0][0], 2 * W, W, H, false) == 0);
  CHECK((y16[0][0] | y16[0][1] << 8) == 64 << 6);           // black: 64
  CHECK((y16[0][2] | y16[0][3] << 8) == 940 << 6);          // white: 940
  CHECK((c16[0][0] | c16[0][1] << 8) == (64 + 128 * 896 / 255) << 6);
  CHECK((c16[1][2] | c16[1][3] << 8) == (64 + 128 * 896 / 255) << 6);
  // x422: a chroma row for every luma row
  memset(c16, 0, sizeof c16);
  CHECK(oneios_camera_scale_10bit(&h, picture, &y16[0][0], 2 * W, &c16[0][0], 2 * W, W, H, true) == 0);
  CHECK((c16[H - 1][0] | c16[H - 1][1] << 8) == (64 + 128 * 896 / 255) << 6);
  CHECK(oneios_camera_scale_10bit(&h, picture, &y16[0][0], W, &c16[0][0], 2 * W, W, H, false) == -22);    // a stride too small
  // NV12 (420v levels) and BGRA, as before the move
  static uint8_t y8[H][W], c8[H / 2][W], bgra[H][4 * W];
  CHECK(oneios_camera_scale_nv12(&h, picture, &y8[0][0], W, &c8[0][0], W, W, H, true) == 0);
  CHECK(y8[0][0] == 16 && y8[0][1] == 235 && c8[0][0] == 16 + 128 * 224 / 255);
  CHECK(oneios_camera_scale_bgra(&h, picture, &bgra[0][0], 4 * W, W, H) == 0);
  CHECK(bgra[0][0] == 0 && bgra[0][4] == 255 && bgra[0][3] == 255);
}

int main(void)
{
  formats();
  companions();
  front_devices();
  white_balance();
  scalers();
  printf("%s: %d formats and scalers checks, %d failed\n", failures ? "FAILED" : "ok", checks, failures);
  return failures != 0;
}
