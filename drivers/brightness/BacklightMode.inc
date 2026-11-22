// SPDX-License-Identifier: MIT
//
// OneiOS: backboardd's brightness control, the physical one. Appended to the Metal plug-in's NativeAdrenoMetalPlugin.mm by
// userland/metal/build.sh (stage): the plug-in is the OneiOS code that runs inside backboardd, loaded on backboardd's main
// thread when the window server makes its Metal device, before the same thread instantiates BrightnessSystem (every
// session of 2026-11-12 with the plug-in: "native-metal: ..." 20-90 ms before "instantiating BrightnessSystem").
//
// Why [read in 24A446's CoreBrightness, drivers/brightness/README.md has the addresses]: -[BrightnessSystemInternal init]
// asks _CBU_IsVirtualMachine (MobileGestalt IsVirtualDevice, which is kern.hv_vmm_present, which is the device tree's
// /defaults/vmm-present) and passes initBLControl: mode 3 on a virtual device, else 1 (2, test mode, on internal builds
// only). Mode 3 makes VMBLControl, which serves a Virtualization.framework host over remote XPC
// (com.apple.virtualization.corebrightness) and never sets a property on the backlight service: the slider did nothing,
// SpringBoard got "No container for handle Display(id: 1)", and the guest driver logged no request. vmm-present stays (AMFI,
// VirtualAudio, bluetoothd read it; hiding it from backboardd changes QuartzCore's display paths, and droidvm and sheng saw
// backboardd crash so), so only this one choice changes: mode 3 becomes 1, BLControl, as on an iPhone. The same fix as
// UniXNU's scripts/appsync/BacklightMode.m (iPadOS 27.0.1, the sheng board: the slider works there).
//
// What it logs (iOS's log, process backboardd): "OneiOS backlight: initBLControl: mode 3 -> 1 (BLControl, not VMBLControl)"
// when CoreBrightness asks, "OneiOS backlight: physical brightness control selected" at the swizzle, or why not. CoreBrightness
// itself then says "Set Default 1 BLControl" where it said "Set VM Mode 3 BLControl". oneios-bl-mode=0 in the kernel's boot
// arguments leaves CoreBrightness alone.
#include <objc/runtime.h>
#include <os/log.h>

static BOOL (*gOneiOSInitBLControl)(id, SEL, long long);

static BOOL oneiosInitBLControl(id self, SEL command, long long mode) {
    os_log(OS_LOG_DEFAULT, "OneiOS backlight: initBLControl: mode %lld -> %lld%s", mode, mode == 3 ? 1LL : mode,
           mode == 3 ? " (BLControl, not VMBLControl)" : "");
    return gOneiOSInitBLControl(self, command, mode == 3 ? 1 : mode);
}

__attribute__((constructor)) static void oneiosBacklightMode() {
    if (strcmp(getprogname(), "backboardd")) return;
    char args[1024] = {};
    size_t size = sizeof(args) - 1;
    if (!sysctlbyname("kern.bootargs", args, &size, nullptr, 0) && strstr(args, "oneios-bl-mode=0")) {
        os_log(OS_LOG_DEFAULT, "OneiOS backlight: oneios-bl-mode=0: CoreBrightness keeps its own mode");
        return;
    }
    @autoreleasepool {
        dlopen("/System/Library/PrivateFrameworks/CoreBrightness.framework/CoreBrightness", RTLD_NOW);
        Class cls = objc_getClass("BrightnessSystemInternal");
        Method method = cls ? class_getInstanceMethod(cls, sel_registerName("initBLControl:")) : nullptr;
        char type[16] = "";
        if (method) method_getArgumentType(method, 2, type, sizeof(type));
        if (!method || method_getNumberOfArguments(method) != 3 || !type[0] || !strchr("qQlL", type[0])) {
            os_log_error(OS_LOG_DEFAULT, "OneiOS backlight: no -[BrightnessSystemInternal initBLControl:] of the known shape (%s, type '%s'): "
                         "inactive, brightness stays with VMBLControl", cls ? "class found" : "no class", type);
            return;
        }
        gOneiOSInitBLControl = reinterpret_cast<BOOL (*)(id, SEL, long long)>(
            method_setImplementation(method, reinterpret_cast<IMP>(oneiosInitBLControl)));
        os_log(OS_LOG_DEFAULT, "OneiOS backlight: physical brightness control selected (initBLControl: mode 3 becomes 1)");
    }
}

// The brightness level's path (fix-brightness-path, 2026-11-23). Read in 24A446's CoreBrightness [F]: the method
// useSyncBrightnessTransactionForDisplay: (IMP 0x22aa98098; selector found by its relative method-list entry
// 0x22ab635b4) answers, for a CAWindowServerDisplay whose displayType is 0 (the built-in one),
// MGGetBoolAnswer("DeviceSupportsFrameSynchronousBrightness") (_CBU_IsSyncBrightnessTransactionsSupported 0x22aa40f88)
// or else [display brightnessAvailable]. With YES CoreBrightness gives every level to CoreAnimation's frame-synchronous
// brightness (a DCP's brightness_update on an iPhone). Here QuartzCore has no handler for it ("Display 1, no brightness
// handler registered"), so the backlight service got only DisplayBrightness {Brightness = a Float32 IOKit truncates to
// 0} and DisplayBrightnessSliderPosition 0: every level of the slider put the panel at its floor (phone, 2026-11-23:
// "Display ID: 1, UseSyncBrightnessTransactionForDisplay: 1"; requests 7 ... 448 all "level 0"). With NO it takes the
// path of a device without that (BLControl's own), which sets brightness-nits, signed 16.16 nits, on the service: the
// one brightness-nits of that boot moved the panel (request 3, level 22020). oneios-bl-sync=1 in the kernel's boot
// arguments keeps iOS's answer.
// Lines (iOS's log, process backboardd): "OneiOS backlight: useSyncBrightnessTransactionForDisplay: answered NO (iOS said
// YES) for display ..." at each call that changes an answer (CoreBrightness asks once per display at its setup), and
// "OneiOS backlight: frame-synchronous brightness off (useSyncBrightnessTransactionForDisplay: of CLASS)" at the swizzle.
static BOOL (*gOneiOSUseSync)(id, SEL, id);
static volatile int gOneiOSUseSyncLogged;

static BOOL oneiosUseSync(id self, SEL command, id display) {
    BOOL ios = gOneiOSUseSync(self, command, display);
    if (ios && __atomic_fetch_add(&gOneiOSUseSyncLogged, 1, __ATOMIC_RELAXED) < 16)
        os_log(OS_LOG_DEFAULT, "OneiOS backlight: useSyncBrightnessTransactionForDisplay: answered NO (iOS said YES) for display %{public}@: "
               "levels go to the backlight service as brightness-nits", display ? NSStringFromClass([display class]) : @"(nil)");
    return NO;
}

__attribute__((constructor)) static void oneiosBacklightSync() {
    if (strcmp(getprogname(), "backboardd")) return;
    char args[1024] = {};
    size_t size = sizeof(args) - 1;
    if (!sysctlbyname("kern.bootargs", args, &size, nullptr, 0) && strstr(args, "oneios-bl-sync=1")) {
        os_log(OS_LOG_DEFAULT, "OneiOS backlight: oneios-bl-sync=1: CoreBrightness keeps frame-synchronous brightness");
        return;
    }
    @autoreleasepool {
        static const char image[] = "/System/Library/PrivateFrameworks/CoreBrightness.framework/CoreBrightness";
        dlopen(image, RTLD_NOW);
        SEL selector = sel_registerName("useSyncBrightnessTransactionForDisplay:");
        // BLControl first (where 24A446 has it); else the class of CoreBrightness that defines it itself.
        Class found = nullptr;
        Class first = objc_getClass("BLControl");
        if (first && class_getInstanceMethod(first, selector)) found = first;
        unsigned count = 0;
        const char **names = found ? nullptr : objc_copyClassNamesForImage(image, &count);
        for (unsigned i = 0; names && i < count && !found; i++) {
            Class cls = objc_getClass(names[i]);
            unsigned n = 0;
            Method *methods = cls ? class_copyMethodList(cls, &n) : nullptr;
            for (unsigned j = 0; j < n && !found; j++)
                if (method_getName(methods[j]) == selector) found = cls;
            free(methods);
        }
        free(names);
        Method method = found ? class_getInstanceMethod(found, selector) : nullptr;
        char type[16] = "";
        if (method) method_getReturnType(method, type, sizeof(type));
        if (!method || method_getNumberOfArguments(method) != 3 || !strchr("Bc", type[0])) {
            os_log_error(OS_LOG_DEFAULT, "OneiOS backlight: no useSyncBrightnessTransactionForDisplay: of the known shape (%s, returns '%s'): "
                         "frame-synchronous brightness stays (the slider may not move the panel)", found ? class_getName(found) : "no class", type);
            return;
        }
        gOneiOSUseSync = reinterpret_cast<BOOL (*)(id, SEL, id)>(method_setImplementation(method, reinterpret_cast<IMP>(oneiosUseSync)));
        os_log(OS_LOG_DEFAULT, "OneiOS backlight: frame-synchronous brightness off (useSyncBrightnessTransactionForDisplay: of %s)",
               class_getName(found));
    }
}
