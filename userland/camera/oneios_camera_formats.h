/*
 * SPDX-License-Identifier: MIT
 *
 * The stream formats CMCapture 27 matches: what a named stream format of the device's AVCaptureSession.plist
 * (MultiStream FigCaptureStreamFormats) asks for, the keys of our stream format made from it, and CMCapture's own
 * rule for "this stream format is that named format". The rule is csu_resolveFormatIndexForNamedStreamFormat of the
 * 24A446 shared cache (CMCapture 0x1b07b7644), read instruction by instruction (userland/camera/README.md, "How
 * CMCapture picks a stream format"). Plain C: the plug-in uses it, and tests/test_formats.c checks it on the host.
 */
#ifndef ONEIOS_CAMERA_FORMATS_H
#define ONEIOS_CAMERA_FORMATS_H

#include <stdbool.h>

/* A named stream format of the plist, by the keys the resolver reads (0 or false: the key is not there). */
struct ocf_params {
	int sensor_w, sensor_h;         /* SensorWidth, SensorHeight */
	bool fes;                       /* FES */
	int fes_w, fes_h;               /* FESWidth, FESHeight */
	bool binned;                    /* Binned */
	bool hdr;                       /* HDR */
	int max_fps;                    /* MaxFrameRate */
	bool sifr;                      /* SIFREnabled */
	int sifr_bf;                    /* SIFRBinningFactor */
	int qbf;                        /* QuadraBinningFactor */
	bool mid_sync;                  /* OptimizedForMidFrameSynchronization */
	bool signing;                   /* SecureSigningFaceIDSupported */
	bool unsupported;               /* Unsupported: the resolver skips the name */
};

/* The keys of one of our stream formats (SupportedFormatsArray) that the resolver compares, named as in CMCaptureCore. */
struct ocf_format {
	int sensor_w, sensor_h;         /* kFigCaptureStreamVideoFormatKey_SensorWidth, _SensorHeight */
	int fes_out_w, fes_out_h;       /* _FrontEndScalerOutputWidth, _FrontEndScalerOutputHeight (0: not put) */
	bool binned;                    /* kFigSupportedFormat_VideoIsBinned */
	int hdr_mode;                   /* _SensorHDRMode (0: not put) */
	int max_fps;                    /* kFigSupportedFormat_VideoMaxFrameRate */
	bool sifr;                      /* _SIFREnabled */
	int sifr_bf;                    /* _SIFRBinningFactor (0: not put) */
	int qbf;                        /* _QuadraBinningFactor (0: not put) */
	bool mid_sync;                  /* _OptimizedForMidFrameSynchronization */
	bool signing;                   /* _SensorSigningEnabled */
	bool experimental;              /* kFigSupportedFormat_Experimental: the resolver skips the format */
};

/* The format that the resolver matches to p (a named format without a sensor size gets 1920x1080; MaxFrameRate 30). */
void ocf_format_for(const struct ocf_params *p, struct ocf_format *f);

/* CMCapture's rule: true when the resolver takes f for p. */
bool ocf_matches(const struct ocf_params *p, const struct ocf_format *f);

/* Two formats the resolver cannot tell apart. */
bool ocf_same(const struct ocf_format *a, const struct ocf_format *b);

/* The index of the first format of fs that the resolver takes for p (as CMCapture: the first match), or -1. */
int ocf_resolve(const struct ocf_params *p, const struct ocf_format *fs, int n);

#endif
