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
  scalers();
  printf("%s: %d formats and scalers checks, %d failed\n", failures ? "FAILED" : "ok", checks, failures);
  return failures != 0;
}
