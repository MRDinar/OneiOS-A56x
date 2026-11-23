// SPDX-License-Identifier: MIT
// oneios_glue.c's functions: what drm2kgsl-sys's src/ffi.rs declares (the same names and types, by hand).
#ifndef ONEIOS_GLUE_H
#define ONEIOS_GLUE_H
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

typedef void (*oneios_d2k_line_fn)(int level, const char *line);
typedef void (*oneios_d2k_retire_fn)(uint32_t ctx, uint32_t ring, uint64_t fence);
struct oneios_d2k_caps { uint64_t chip_id, va_start, va_size; uint32_t gpu_id, priorities; };

int  oneios_d2k_init(oneios_d2k_line_fn on_line, oneios_d2k_retire_fn on_retire);
int  oneios_d2k_probe(int fd, void *capset, size_t max, struct oneios_d2k_caps *caps);
bool oneios_d2k_context_exists(uint32_t id);
int  oneios_d2k_context_create(uint32_t id, int fd, const char *name, size_t name_length);
void oneios_d2k_context_destroy(uint32_t id);
int  oneios_d2k_submit(uint32_t id, const void *commands, size_t size);
int  oneios_d2k_submit_fence(uint32_t id, uint32_t ring, uint64_t fence);
void oneios_d2k_set_class(uint32_t id, unsigned cls, bool lowest);
bool oneios_d2k_resource_exists(uint32_t res);
int  oneios_d2k_resource_dmabuf(uint32_t res, int fd, uint64_t size);
void oneios_d2k_resource_remove(uint32_t res);
int  oneios_d2k_set_guest_blob_chunks(uint32_t id, uint64_t blob_id, const int *fds, const uint64_t *sizes, unsigned n);
int  oneios_d2k_get_blob(uint32_t id, uint32_t res, uint64_t blob_id, uint64_t size, uint32_t flags);
int  oneios_d2k_resource_map(uint32_t res, void **ptr, uint64_t *size, uint32_t *map_info);
int  oneios_d2k_attach(uint32_t id, uint32_t res, bool on, const int *fds, const uint64_t *sizes, unsigned n);
void oneios_d2k_unref(uint32_t res);
unsigned oneios_d2k_policy_priority(unsigned cls);
const char *oneios_d2k_policy_class_name(unsigned cls);
#endif
