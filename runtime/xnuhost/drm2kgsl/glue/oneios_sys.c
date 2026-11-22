// SPDX-License-Identifier: MIT
// The hooks of oneios_sys.h (host builds only): the real open(2) and ioctl(2) until a test sets its own.
#define _GNU_SOURCE
#include <fcntl.h>
#include <stdarg.h>
#include <stddef.h>
#include <sys/ioctl.h>

typedef int (*oneios_open_fn)(const char *path, int flags, unsigned mode);
typedef int (*oneios_ioctl_fn)(int fd, unsigned long request, void *arg);
static oneios_open_fn hook_open;
static oneios_ioctl_fn hook_ioctl;

void oneios_sys_set_hooks(oneios_open_fn o, oneios_ioctl_fn i);
void oneios_sys_set_hooks(oneios_open_fn o, oneios_ioctl_fn i)
{
  __atomic_store_n(&hook_open, o, __ATOMIC_RELEASE);
  __atomic_store_n(&hook_ioctl, i, __ATOMIC_RELEASE);
}

int oneios_sys_open(const char *path, int flags, ...);
int oneios_sys_open(const char *path, int flags, ...)
{
  unsigned mode = 0;
  if (flags & (O_CREAT | O_TMPFILE)) {
    va_list ap;
    va_start(ap, flags);
    mode = va_arg(ap, unsigned);
    va_end(ap);
  }
  oneios_open_fn o = __atomic_load_n(&hook_open, __ATOMIC_ACQUIRE);
  return o ? o(path, flags, mode) : open(path, flags, mode);
}

int oneios_sys_ioctl(int fd, unsigned long request, ...);
int oneios_sys_ioctl(int fd, unsigned long request, ...)
{
  va_list ap;
  va_start(ap, request);
  void *arg = va_arg(ap, void *);
  va_end(ap);
  oneios_ioctl_fn i = __atomic_load_n(&hook_ioctl, __ATOMIC_ACQUIRE);
  return i ? i(fd, request, arg) : ioctl(fd, request, arg);
}
