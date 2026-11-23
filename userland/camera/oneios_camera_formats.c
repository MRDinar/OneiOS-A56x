/*
 * SPDX-License-Identifier: MIT
 *
 * CMCapture 27's stream format rule (oneios_camera_formats.h). Each test below is one compare of
 * csu_resolveFormatIndexForNamedStreamFormat (24A446, CMCapture 0x1b07b7644): the format's value against the
 * named format's, with the default the resolver gives a key that is not there.
 */
#include "oneios_camera_formats.h"
#include <string.h>

static int
sifr_factor(const struct ocf_params *p)
{
	/* the named format's factor: SIFRBinningFactor, else 2 when binned and 1 when not (0x1b07b77b8..77cc) */
	return p->sifr_bf != 0 ? p->sifr_bf : p->binned ? 2 : 1;
}

void
ocf_format_for(const struct ocf_params *p, struct ocf_format *f)
{
	*f = (struct ocf_format){ 0 };
	f->sensor_w = p->sensor_w > 0 ? p->sensor_w : 1920;
	f->sensor_h = p->sensor_h > 0 ? p->sensor_h : 1080;
	if (p->fes && p->fes_w > 0 && p->fes_h > 0) {
		f->fes_out_w = p->fes_w;
		f->fes_out_h = p->fes_h;
	} else if (p->fes) {
		/* FES without a size: any front-end scaler output matches; the sensor's own size is one */
		f->fes_out_w = f->sensor_w;
		f->fes_out_h = f->sensor_h;
	}
	f->binned = p->binned;
	f->hdr_mode = p->hdr ? 1 : 0;
	f->max_fps = p->max_fps > 0 ? p->max_fps : 30;
	f->sifr = p->sifr;
	f->sifr_bf = p->sifr ? sifr_factor(p) : 0;
	f->qbf = p->qbf;
	f->mid_sync = p->mid_sync;
	f->signing = p->signing;
}

bool
ocf_matches(const struct ocf_params *p, const struct ocf_format *f)
{
	bool has_fes = f->fes_out_h > 0 && f->fes_out_w > 0;
	int want_qbf;

	if (f->experimental) {
		return false;                   /* skipped, counted (0x1b07b7900..7918) */
	}
	if (f->sensor_w != p->sensor_w || f->sensor_h != p->sensor_h) {
		return false;
	}
	if (f->binned != p->binned) {
		return false;
	}
	/* the quadra binning factor: none on either side, or the format's equal to the named one's (2 when it has none) */
	want_qbf = f->qbf == 0 ? p->qbf : p->qbf != 0 ? p->qbf : 2;
	if (f->qbf != want_qbf) {
		return false;
	}
	if ((f->hdr_mode > 0) != p->hdr) {
		return false;
	}
	if (has_fes != p->fes) {
		return false;
	}
	if (has_fes && p->fes_w >= 1 && p->fes_h >= 1 && (f->fes_out_w != p->fes_w || f->fes_out_h != p->fes_h)) {
		return false;
	}
	if (p->max_fps >= 1 && f->max_fps != p->max_fps) {
		return false;
	}
	if (p->sifr && f->sifr) {
		if (sifr_factor(p) != (f->sifr_bf > 1 ? f->sifr_bf : 1)) {
			return false;
		}
	} else if (f->sifr != p->sifr) {
		return false;
	}
	return f->signing == p->signing && f->mid_sync == p->mid_sync;
}

bool
ocf_same(const struct ocf_format *a, const struct ocf_format *b)
{
	return a->sensor_w == b->sensor_w && a->sensor_h == b->sensor_h && a->fes_out_w == b->fes_out_w &&
	    a->fes_out_h == b->fes_out_h && a->binned == b->binned && a->hdr_mode == b->hdr_mode && a->max_fps == b->max_fps &&
	    a->sifr == b->sifr && a->sifr_bf == b->sifr_bf && a->qbf == b->qbf && a->mid_sync == b->mid_sync &&
	    a->signing == b->signing && a->experimental == b->experimental;
}

/* ---- the keys by name, and the companions (oneios_camera_formats.h) ---- */
static const struct {
	const char *name;
	unsigned bit;
} keys[] = {
	{ "SensorWidth", OCF_HAS_SENSOR_W }, { "SensorHeight", OCF_HAS_SENSOR_H }, { "FES", OCF_HAS_FES },
	{ "FESWidth", OCF_HAS_FES_W }, { "FESHeight", OCF_HAS_FES_H }, { "Binned", OCF_HAS_BINNED }, { "HDR", OCF_HAS_HDR },
	{ "MaxFrameRate", OCF_HAS_MAX_FPS }, { "SIFREnabled", OCF_HAS_SIFR }, { "SIFRBinningFactor", OCF_HAS_SIFR_BF },
	{ "QuadraBinningFactor", OCF_HAS_QBF }, { "OptimizedForMidFrameSynchronization", OCF_HAS_MID_SYNC },
	{ "SecureSigningFaceIDSupported", OCF_HAS_SIGNING }, { "Unsupported", OCF_HAS_UNSUPPORTED },
};

unsigned
ocf_key_bit(const char *name)
{
	for (unsigned i = 0; name != NULL && i < sizeof(keys) / sizeof(keys[0]); i++) {
		if (strcmp(keys[i].name, name) == 0) {
			return keys[i].bit;
		}
	}
	return 0;
}

static int *
int_field(struct ocf_params *p, unsigned bit)
{
	switch (bit) {
	case OCF_HAS_SENSOR_W: return &p->sensor_w;
	case OCF_HAS_SENSOR_H: return &p->sensor_h;
	case OCF_HAS_FES_W: return &p->fes_w;
	case OCF_HAS_FES_H: return &p->fes_h;
	case OCF_HAS_MAX_FPS: return &p->max_fps;
	case OCF_HAS_SIFR_BF: return &p->sifr_bf;
	case OCF_HAS_QBF: return &p->qbf;
	default: return NULL;
	}
}

static bool *
bool_field(struct ocf_params *p, unsigned bit)
{
	switch (bit) {
	case OCF_HAS_FES: return &p->fes;
	case OCF_HAS_BINNED: return &p->binned;
	case OCF_HAS_HDR: return &p->hdr;
	case OCF_HAS_SIFR: return &p->sifr;
	case OCF_HAS_MID_SYNC: return &p->mid_sync;
	case OCF_HAS_SIGNING: return &p->signing;
	case OCF_HAS_UNSUPPORTED: return &p->unsupported;
	default: return NULL;
	}
}

void
ocf_set(struct ocf_params *p, unsigned bit, int value)
{
	int *i = int_field(p, bit);
	bool *b = bool_field(p, bit);

	if (i != NULL) {
		*i = value;
	} else if (b != NULL) {
		*b = value != 0;
	} else {
		return;
	}
	p->has |= bit;
}

void
ocf_unset(struct ocf_params *p, unsigned bit)
{
	int *i = int_field(p, bit);
	bool *b = bool_field(p, bit);

	if (i != NULL) {
		*i = 0;
	} else if (b != NULL) {
		*b = false;
	}
	p->has &= ~bit;
}

static int
value_of(const struct ocf_params *p, unsigned bit)
{
	int *i = int_field((struct ocf_params *)p, bit);
	bool *b = bool_field((struct ocf_params *)p, bit);

	return i != NULL ? *i : b != NULL ? *b : 0;
}

void
ocf_mark(struct ocf_params *p)
{
	for (unsigned i = 0; i < sizeof(keys) / sizeof(keys[0]); i++) {
		if (value_of(p, keys[i].bit) != 0) {
			p->has |= keys[i].bit;
		}
	}
}

/*
 * The companions of 24A446, in the order CMCapture makes them. The steps are its setObject:forKeyedSubscript: calls on the
 * parent's mutable copy: "if there" where it first asks the companion for the key (a cbz), "take" where it sets the
 * companion's value as it is (nil removes the key). The keys the resolver does not read (SensorCropWidth, CropWidthForFES,
 * CaptureStillsFromVideoStream, StereoVideoAEMaxGain) are in the table too: the plug-in's own stream format keeps them.
 */
static const struct ocf_companion companions[] = {
	/* an array: one each (0x1b07b41a0) */
	{ "FESCompanionFormats", "[FESCompanionFormat]", false, false,
	  { { "FES", OCF_SET_TRUE }, { "FESWidth", OCF_TAKE }, { "FESHeight", OCF_TAKE }, { "MaxFrameRate", OCF_TAKE },
	    { "CaptureStillsFromVideoStream", OCF_SET_FALSE } } },
	/* 0x1b07b45f8 */
	{ "NonSIFRCompanionFormat", "[NonSIFRCompanionFormat]", false, false,
	  { { "SIFREnabled", OCF_SET_FALSE }, { "SIFRBinningFactor", OCF_REMOVE }, { "SensorWidth", OCF_TAKE_IF_THERE },
	    { "SensorHeight", OCF_TAKE_IF_THERE }, { "FES", OCF_TAKE }, { "FESWidth", OCF_TAKE }, { "FESHeight", OCF_TAKE },
	    { "MaxFrameRate", OCF_TAKE } } },
	/* 0x1b07b4818: the factor is NSNumber 2 (0x1ec436bc8) */
	{ "FullBinSIFRCompanionFormat", "[FullBinSIFRCompanionFormat]", false, false, { { "SIFRBinningFactor", OCF_SET_TWO } } },
	/* 0x1b07b48b0 */
	{ "MidFrameSynchronizationOptimizedCompanionFormat", "[MidFrameSynchronizationOptimizedCompanionFormat]", false, true,
	  { { "SensorWidth", OCF_TAKE_IF_THERE }, { "SensorHeight", OCF_TAKE_IF_THERE }, { "FES", OCF_TAKE_IF_THERE },
	    { "FESWidth", OCF_TAKE_IF_THERE }, { "FESHeight", OCF_TAKE_IF_THERE }, { "SIFRBinningFactor", OCF_TAKE_IF_THERE },
	    { "OptimizedForMidFrameSynchronization", OCF_SET_TRUE } } },
	/* 0x1b07b4ac0 */
	{ "ActionCameraCompanionFormat", "[ActionCameraCompanionFormat]", false, false,
	  { { "SensorWidth", OCF_TAKE_IF_THERE }, { "SensorHeight", OCF_TAKE_IF_THERE }, { "SensorCropWidth", OCF_TAKE_IF_THERE },
	    { "SensorCropHeight", OCF_TAKE_IF_THERE }, { "SIFRBinningFactor", OCF_TAKE_IF_THERE }, { "FES", OCF_REMOVE } } },
	/* 0x1b07b4c70 */
	{ "DepthCompanionFormat", "[DepthCompanionFormat]", false, false,
	  { { "FESWidth", OCF_TAKE }, { "FESHeight", OCF_TAKE }, { "FES", OCF_SET_TRUE }, { "SensorWidth", OCF_TAKE_IF_THERE },
	    { "SensorHeight", OCF_TAKE_IF_THERE }, { "SIFRBinningFactor", OCF_TAKE_IF_THERE }, { "MaxFrameRate", OCF_TAKE_IF_THERE } } },
	/* 0x1b07b4f7c: as the depth companion */
	{ "FSDNetCompanionFormat", "[FSDNetCompanionFormat]", false, false,
	  { { "FESWidth", OCF_TAKE }, { "FESHeight", OCF_TAKE }, { "FES", OCF_SET_TRUE }, { "SensorWidth", OCF_TAKE_IF_THERE },
	    { "SensorHeight", OCF_TAKE_IF_THERE }, { "SIFRBinningFactor", OCF_TAKE_IF_THERE }, { "MaxFrameRate", OCF_TAKE_IF_THERE } } },
	/* 0x1b07b5288 */
	{ "StereoVideoCompanionFormat", "[StereoVideoCompanionFormat]", false, false,
	  { { "SensorWidth", OCF_TAKE_IF_THERE }, { "SensorHeight", OCF_TAKE_IF_THERE }, { "SensorCropWidth", OCF_TAKE_IF_THERE },
	    { "SensorCropHeight", OCF_TAKE_IF_THERE }, { "SIFRBinningFactor", OCF_TAKE_IF_THERE },
	    { "StereoVideoAEMaxGain", OCF_TAKE_IF_THERE }, { "FES", OCF_REMOVE }, { "FESWidth", OCF_REMOVE }, { "FESHeight", OCF_REMOVE },
	    { "CropWidthForFES", OCF_REMOVE }, { "CropHeightForFES", OCF_REMOVE } } },
	/* 0x1b07b54b8 */
	{ "FaceIDCompanionFormat", "[FaceIDCompanionFormat]", false, false, { { "SecureSigningFaceIDSupported", OCF_SET_TRUE } } },
	/* 0x1b07b5550: a flag of the parent */
	{ "HasSensorHDRCompanionFormat", "[SensorHDRCompanionFormat]", true, false, { { "HDR", OCF_SET_TRUE } } },
};

const struct ocf_companion *
ocf_companion_rule(const char *key)
{
	for (unsigned i = 0; key != NULL && i < sizeof(companions) / sizeof(companions[0]); i++) {
		if (strcmp(companions[i].key, key) == 0) {
			return &companions[i];
		}
	}
	return NULL;
}

void
ocf_companion_params(const struct ocf_companion *c, const struct ocf_params *parent, const struct ocf_params *companion,
    struct ocf_params *out)
{
	*out = *parent;
	ocf_unset(out, OCF_HAS_UNSUPPORTED);
	for (unsigned i = 0; i < sizeof(c->steps) / sizeof(c->steps[0]) && c->steps[i].key != NULL; i++) {
		unsigned bit = ocf_key_bit(c->steps[i].key);
		bool there = companion != NULL && (companion->has & bit) != 0;

		if (bit == 0) {
			continue;               /* a key the resolver does not read */
		}
		switch (c->steps[i].op) {
		case OCF_TAKE_IF_THERE:
			if (there) {
				ocf_set(out, bit, value_of(companion, bit));
			}
			break;
		case OCF_TAKE:
			if (there) {
				ocf_set(out, bit, value_of(companion, bit));
			} else {
				ocf_unset(out, bit);
			}
			break;
		case OCF_SET_TRUE: ocf_set(out, bit, 1); break;
		case OCF_SET_FALSE: ocf_set(out, bit, 0); break;
		case OCF_REMOVE: ocf_unset(out, bit); break;
		case OCF_SET_TWO: ocf_set(out, bit, 2); break;
		}
	}
}

double
ocf_wb_gain(int fixed)
{
	return (double)fixed / 4096.0;
}

int
ocf_resolve(const struct ocf_params *p, const struct ocf_format *fs, int n)
{
	if (p->unsupported) {
		return -1;
	}
	for (int i = 0; i < n; i++) {
		if (ocf_matches(p, &fs[i])) {
			return i;
		}
	}
	return -1;
}
