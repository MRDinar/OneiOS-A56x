// SPDX-License-Identifier: MIT
// Host builds only: the sizes, offsets and constants of drm2kgsl's msm_kgsl.h that xnuhost's stand-in for KGSL mirrors
// in Rust (runtime/xnuhost/src/gpu3d_kgsl/tests/kgsl.rs), in the order its layout test lists them.
#include <stddef.h>
#include <stdint.h>
#include <linux/types.h>
#define __user
#include "drm2kgsl/msm_kgsl.h"

static const uint64_t layout[] = {
  // sizes
  sizeof(struct kgsl_device_getproperty), sizeof(struct kgsl_devinfo), sizeof(struct kgsl_gpuobj_alloc),
  sizeof(struct kgsl_gpuobj_info), sizeof(struct kgsl_gpuobj_import), sizeof(struct kgsl_gpuobj_import_dma_buf),
  sizeof(struct kgsl_gpuobj_free), sizeof(struct kgsl_gpumem_bind_ranges), sizeof(struct kgsl_gpumem_bind_range),
  sizeof(struct kgsl_device_constraint), sizeof(struct kgsl_device_constraint_pwrlevel), sizeof(struct kgsl_drawctxt_create),
  sizeof(struct kgsl_timestamp_event), sizeof(struct kgsl_timestamp_event_fence),
  // offsets
  offsetof(struct kgsl_device_getproperty, value), offsetof(struct kgsl_device_getproperty, sizebytes),
  offsetof(struct kgsl_devinfo, chip_id), offsetof(struct kgsl_devinfo, mmu_enabled), offsetof(struct kgsl_devinfo, gpu_id),
  offsetof(struct kgsl_devinfo, gmem_sizebytes), offsetof(struct kgsl_gpuobj_alloc, id),
  offsetof(struct kgsl_gpuobj_info, size), offsetof(struct kgsl_gpuobj_info, id), offsetof(struct kgsl_gpuobj_import, type),
  offsetof(struct kgsl_gpuobj_import, id), offsetof(struct kgsl_gpuobj_free, id), offsetof(struct kgsl_gpumem_bind_ranges, id),
  offsetof(struct kgsl_gpumem_bind_range, child_id), offsetof(struct kgsl_device_constraint, data),
  offsetof(struct kgsl_timestamp_event, priv),
  // the requests' numbers (by _IOC_NR) and type
  KGSL_IOC_TYPE, _IOC_NR(IOCTL_KGSL_DEVICE_GETPROPERTY), _IOC_NR(IOCTL_KGSL_DEVICE_WAITTIMESTAMP_CTXTID),
  _IOC_NR(IOCTL_KGSL_DRAWCTXT_CREATE), _IOC_NR(IOCTL_KGSL_DRAWCTXT_DESTROY), _IOC_NR(IOCTL_KGSL_SETPROPERTY),
  _IOC_NR(IOCTL_KGSL_TIMESTAMP_EVENT), _IOC_NR(IOCTL_KGSL_GPUOBJ_ALLOC), _IOC_NR(IOCTL_KGSL_GPUOBJ_FREE),
  _IOC_NR(IOCTL_KGSL_GPUOBJ_INFO), _IOC_NR(IOCTL_KGSL_GPUOBJ_IMPORT), _IOC_NR(IOCTL_KGSL_GPU_COMMAND),
  _IOC_NR(IOCTL_KGSL_GPUMEM_BIND_RANGES),
  // constants
  KGSL_PROP_DEVICE_INFO, KGSL_PROP_PWR_CONSTRAINT, KGSL_MEMFLAGS_VBO, KGSL_MEMFLAGS_IOCOHERENT, KGSL_USER_MEM_TYPE_DMABUF,
  KGSL_GPUMEM_RANGE_OP_BIND, KGSL_GPUMEM_RANGE_OP_UNBIND, KGSL_CONTEXT_PRIORITY_MASK, KGSL_CONTEXT_PRIORITY_SHIFT,
  KGSL_CONTEXT_PWR_CONSTRAINT, KGSL_TIMESTAMP_EVENT_FENCE,
};

size_t oneios_kgsl_layout(const uint64_t **out);
size_t oneios_kgsl_layout(const uint64_t **out)
{
  *out = layout;
  return sizeof layout / sizeof *layout;
}
