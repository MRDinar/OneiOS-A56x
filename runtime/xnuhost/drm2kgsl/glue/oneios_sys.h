// SPDX-License-Identifier: MIT
// Host builds only (the tests: drm2kgsl-sys's build.rs passes -include oneios_sys.h to the renderer's C, never in an
// Android build): every open(2) and ioctl(2) of the renderer goes through oneios_sys.c's table, which a test points at
// its stand-in for KGSL (drm2kgsl_sys::sys::set_hooks). The Rust side's own opens and ioctls of the same nodes go
// through it too (drm2kgsl_sys::sys::open, ::ioctl). What test_drm2kgsl.c did with -Wl,--wrap=open,--wrap=ioctl, without
// catching the rest of the process (Rust's std, the test harness).
#ifndef ONEIOS_SYS_H
#define ONEIOS_SYS_H
int oneios_sys_open(const char *path, int flags, ...);
int oneios_sys_ioctl(int fd, unsigned long request, ...);
#define open(...) oneios_sys_open(__VA_ARGS__)
#define ioctl(...) oneios_sys_ioctl(__VA_ARGS__)
#endif
