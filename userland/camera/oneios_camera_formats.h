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

/* The keys of a named format the resolver reads, as bits of ocf_params.has (the entry has the key). */
enum {
	OCF_HAS_SENSOR_W = 1u << 0, OCF_HAS_SENSOR_H = 1u << 1, OCF_HAS_FES = 1u << 2, OCF_HAS_FES_W = 1u << 3,
	OCF_HAS_FES_H = 1u << 4, OCF_HAS_BINNED = 1u << 5, OCF_HAS_HDR = 1u << 6, OCF_HAS_MAX_FPS = 1u << 7,
	OCF_HAS_SIFR = 1u << 8, OCF_HAS_SIFR_BF = 1u << 9, OCF_HAS_QBF = 1u << 10, OCF_HAS_MID_SYNC = 1u << 11,
	OCF_HAS_SIGNING = 1u << 12, OCF_HAS_UNSUPPORTED = 1u << 13,
};

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
	unsigned has;                   /* OCF_HAS_*: which keys the entry has (a companion's "take it when there") */
};

/* The bit of a key's name ("SensorWidth": OCF_HAS_SENSOR_W), or 0 for a key the resolver does not read. */
unsigned ocf_key_bit(const char *name);
/* Sets the key of `bit` to value (a boolean: 0 or 1) and marks it there; ocf_unset takes it away. */
void ocf_set(struct ocf_params *p, unsigned bit, int value);
void ocf_unset(struct ocf_params *p, unsigned bit);
/* Marks every key whose value is not 0 as there (for entries written as C literals). */
void ocf_mark(struct ocf_params *p);

/*
 * How CMCapture 24A446 makes a companion's named format (csu_createMultiStreamSourceFormatsArrayAndAddAVCompressionSettingsAttribute,
 * 0x1b07b2f7c, read instruction by instruction; userland/camera/README.md "How CMCapture builds a companion"): a mutable copy of the
 * parent's entry, then these steps with the companion's entry, then the resolver under the name parent + suffix. A companion
 * key that is not in the table (UltraHighResCompanionFormat) is not resolved there.
 */
enum ocf_op {
	OCF_TAKE_IF_THERE,              /* the companion's value, when the companion has the key */
	OCF_TAKE,                       /* the companion's value, or none (the key removed) when it has not */
	OCF_SET_TRUE, OCF_SET_FALSE,
	OCF_REMOVE,
	OCF_SET_TWO,                    /* the number 2 (FullBinSIFR's SIFRBinningFactor) */
};
struct ocf_step {
	const char *key;
	enum ocf_op op;
};
struct ocf_companion {
	const char *key;                /* the parent's key: a dictionary (an array of them for FESCompanionFormats), or a flag */
	const char *suffix;             /* CMCapture's name of it: the parent's name and this */
	bool flag;                      /* key is a boolean of the parent (HasSensorHDRCompanionFormat), not a companion entry */
	bool skip_unsupported;          /* a companion entry with Unsupported is skipped */
	struct ocf_step steps[12];      /* ends at the first key NULL */
};
/* The rule for a parent's key ("FESCompanionFormats", "HasSensorHDRCompanionFormat"), or NULL. */
const struct ocf_companion *ocf_companion_rule(const char *key);
/* The companion's named format as CMCapture's resolver reads it: the rule's steps on the parent's keys. */
void ocf_companion_params(const struct ocf_companion *c, const struct ocf_params *parent, const struct ocf_params *companion,
    struct ocf_params *out);

/* The stream's WhiteBalanceGain: { RedGain, GreenGain, BlueGain } integers with 12 fraction bits (CMCapture reads each with
 * scvtf #12 into the source's DefaultWhiteBalanceGains, 0x1b07ace0c). The picture comes white-balanced: each gain is 1. */
#define OCF_WB_GAIN_ONE 4096
/* A gain as CMCapture reads the integer. */
double ocf_wb_gain(int fixed);

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
