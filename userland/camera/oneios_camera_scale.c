/*
 * SPDX-License-Identifier: MIT
 *
 * The capture plug-in's scalers (OneiOSCamera.h): the kernel's NV12 frame (BT.601 full range) into the buffer of the
 * pool the capture graph gave an output, nearest pixel. Plain C, so the host tests (tests/test_reader.c) run them.
 * Moved out of OneiOSCaptureDevice.c on 2026-11-23; the 10-bit one is new.
 */
#include <errno.h>
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include "OneiOSCamera.h"

static inline uint8_t
clamp8(int v)
{
	return (uint8_t)(v < 0 ? 0 : v > 255 ? 255 : v);
}

int
oneios_camera_scale_nv12(const struct oneios_camera_frame *h, const uint8_t *picture, uint8_t *y, size_t y_stride, uint8_t *cbcr,
    size_t cbcr_stride, uint32_t width, uint32_t height, bool video_range)
{
	const uint8_t *src_c = picture + (size_t)h->stride * h->height;

	if (width < 2 || height < 2 || y_stride < width || cbcr_stride < width) {
		return -EINVAL;
	}
	for (uint32_t row = 0; row < height; row++) {
		const uint8_t *src = picture + (size_t)(row * h->height / height) * h->stride;
		uint8_t *dst = y + row * y_stride;

		for (uint32_t x = 0; x < width; x++) {
			int v = src[x * h->width / width];

			dst[x] = video_range ? (uint8_t)(16 + v * 219 / 255) : (uint8_t)v;
		}
	}
	for (uint32_t row = 0; row < height / 2; row++) {
		const uint8_t *src = src_c + (size_t)(row * (h->height / 2) / (height / 2)) * h->stride;
		uint8_t *dst = cbcr + row * cbcr_stride;

		for (uint32_t x = 0; x < width / 2; x++) {
			uint32_t sx = x * (h->width / 2) / (width / 2);
			int cb = src[2 * sx], cr = src[2 * sx + 1];

			dst[2 * x] = video_range ? (uint8_t)(16 + cb * 224 / 255) : (uint8_t)cb;
			dst[2 * x + 1] = video_range ? (uint8_t)(16 + cr * 224 / 255) : (uint8_t)cr;
		}
	}
	return 0;
}

int
oneios_camera_scale_10bit(const struct oneios_camera_frame *h, const uint8_t *picture, uint8_t *y, size_t y_stride, uint8_t *cbcr,
    size_t cbcr_stride, uint32_t width, uint32_t height, bool chroma_422)
{
	const uint8_t *src_c = picture + (size_t)h->stride * h->height;
	uint32_t chroma_rows = chroma_422 ? height : height / 2;

	if (width < 2 || height < 2 || y_stride < (size_t)width * 2 || cbcr_stride < (size_t)width * 2) {
		return -EINVAL;
	}
	/* 10-bit video range: Y 64-940, Cb Cr 64-960; each sample in the top 10 bits of a little-endian 16-bit word */
	for (uint32_t row = 0; row < height; row++) {
		const uint8_t *src = picture + (size_t)(row * h->height / height) * h->stride;
		uint8_t *dst = y + row * y_stride;

		for (uint32_t x = 0; x < width; x++) {
			uint16_t v = (uint16_t)((64 + src[x * h->width / width] * 876 / 255) << 6);

			dst[2 * x] = (uint8_t)v;
			dst[2 * x + 1] = (uint8_t)(v >> 8);
		}
	}
	for (uint32_t row = 0; row < chroma_rows; row++) {
		const uint8_t *src = src_c + (size_t)(row * (h->height / 2) / chroma_rows) * h->stride;
		uint8_t *dst = cbcr + row * cbcr_stride;

		for (uint32_t x = 0; x < width / 2; x++) {
			uint32_t sx = x * (h->width / 2) / (width / 2);
			uint16_t cb = (uint16_t)((64 + src[2 * sx] * 896 / 255) << 6), cr = (uint16_t)((64 + src[2 * sx + 1] * 896 / 255) << 6);

			dst[4 * x] = (uint8_t)cb;
			dst[4 * x + 1] = (uint8_t)(cb >> 8);
			dst[4 * x + 2] = (uint8_t)cr;
			dst[4 * x + 3] = (uint8_t)(cr >> 8);
		}
	}
	return 0;
}

int
oneios_camera_scale_bgra(const struct oneios_camera_frame *h, const uint8_t *picture, uint8_t *bgra, size_t stride, uint32_t width,
    uint32_t height)
{
	const uint8_t *src_c = picture + (size_t)h->stride * h->height;

	if (width < 1 || height < 1 || stride < (size_t)width * 4) {
		return -EINVAL;
	}
	for (uint32_t row = 0; row < height; row++) {
		uint32_t sy = row * h->height / height;
		const uint8_t *ys = picture + (size_t)sy * h->stride, *cs = src_c + (size_t)(sy / 2) * h->stride;
		uint8_t *dst = bgra + row * stride;

		for (uint32_t x = 0; x < width; x++) {
			uint32_t sx = x * h->width / width;
			int Y = ys[sx], cb = cs[sx & ~1u] - 128, cr = cs[(sx & ~1u) + 1] - 128;     /* BT.601 full range */

			dst[4 * x + 0] = clamp8(Y + (1772 * cb) / 1000);
			dst[4 * x + 1] = clamp8(Y - (344 * cb + 714 * cr) / 1000);
			dst[4 * x + 2] = clamp8(Y + (1402 * cr) / 1000);
			dst[4 * x + 3] = 255;
		}
	}
	return 0;
}
