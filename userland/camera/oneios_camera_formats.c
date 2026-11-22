/*
 * SPDX-License-Identifier: MIT
 *
 * CMCapture 27's stream format rule (oneios_camera_formats.h). Each test below is one compare of
 * csu_resolveFormatIndexForNamedStreamFormat (24A446, CMCapture 0x1b07b7644): the format's value against the
 * named format's, with the default the resolver gives a key that is not there.
 */
#include "oneios_camera_formats.h"

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
