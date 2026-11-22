// SPDX-License-Identifier: MIT
//! xnuhost: XNU as a process on Android's Linux kernel (docs/booter.md). One module per part of the machine.

pub mod session; // main.c: the session, the main loop, the stop
pub mod ret; // return.c: the return's graceful-shutdown state machine
pub mod ready; // ready.c: when the guest is up
pub mod perf; // perf.c: the perf: report
pub mod config; // config.c: the settings
pub mod mem; // mem.c: guest RAM and the I/O aperture
pub mod discard; // discard.c: pages the guest gave back
pub mod ramfix; // ramfix.c: the guest's RAM held fixed
pub mod usb; // usb.c: the USB gadget's log line
pub mod gic; // gic.c: the interrupt controller
pub mod sysreg; // sysreg.c: trapped system registers
pub mod uart; // uart.c: the PL011 console
pub mod blk; // blk.c: disks
pub mod imageset; // imageset.c: the image set
pub mod virtio_blk; // virtio_blk.c: the virtio block device
pub mod virtio_input; // virtio_input.c: the virtio input device
pub mod virtio_gpu; // virtio_gpu.c: the virtio GPU device
pub mod gpu3d; // gpu3d.c: the 3D renderer's front
pub mod gpu3d_kgsl; // gpu3d_kgsl.c: drm2kgsl, linked in
pub mod display; // display.c: the scanout and the display helper
pub mod input; // input.c: /dev/input into virtio-input
pub mod input_script; // input_script.c: scripted touches and keys
pub mod pen; // pen.c: the pen digitizer
pub mod cpu; // cpu.c: vCPUs
pub mod hostcalls; // hostcalls.c: the host-call vector
pub mod task; // task.c: iOS tasks as processes
pub mod arch; // arch_aarch64.S: context switches and trap entry
pub mod kctx; // kctx.c: code that runs in the kernel's context
