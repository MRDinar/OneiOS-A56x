// SPDX-License-Identifier: MIT
//
// OneiOS: each Metal client's command pools on its kernel text line, a lasting line. Appended to the Metal plug-in's
// NativeAdrenoMetalPlugin.mm by userland/metal/build.sh (stage), as DisplayWide.inc is; `oneios-tool ios metal-stage-edit`
// declares oneios_gpu_pool_row_start near the top of the file and calls it where a process gets its device (the render
// server's path and a client's).
//
// Why (2026-11-23, sessions s1824 and s2023): backboardd held 2,600-3,400 GPU blobs under 1 MiB, 1,400 of them under
// 16 KiB, growing by 200-600 in each 10 s an app opened, freed only at a memory pressure rise. Those are Turnip's command
// stream BOs that metal2vulkan's spare command pools keep (userland/metal/m2v-spare-command-pools.patch, docs/gpu.md
// "Who keeps a blob"). This line says how many command buffers the process's pools hold and how many the runtime let
// go, so the next boot shows whether the patch bounds them:
//   oneios-gpu-text: backboardd[64] CP 9/120 cut +2/80 idle +1/12 tuc 11M
// CP live pools / command buffers made from them; cut: pools released since the last row for holding past their recent
// use, with their command buffers; idle: pools the idle trim released, with theirs; tuc: Turnip's cache of freed BOs now.
// Every 60 s, only when the command buffers moved by 32 or more since the last row, or a pool went: the kernel prints at
// most 2000 text rows a boot (0039), and a quiet process sends none. Each row is about 60 marker calls, once a minute.
//
// The switch (docs/gpu.md, "Who keeps a blob"): the boot argument oneios-pool-cut=0 (xnuhost.conf
// boot-args-add=oneios-pool-cut=0) sets METAL2VULKAN_POOL_USE_WINDOW_S=0 in every process the plug-in loads in, before
// any device is made, so the runtime keeps its pools as before; the row stays. Without it the runtime's default (10 s).
#include <atomic>
#include <stdlib.h>
#include <string.h>
#include <sys/sysctl.h>
#include <thread>

// Inside the plug-in's own namespace (its Gpu, makeGpu, streamText and turnip::libraryPath are publish::'s); the
// extern "C" name is the one declared at the top of the file.
namespace publish {

__attribute__((constructor)) static void oneiosPoolCutSwitch() {
    char args[1024] = {};
    size_t size = sizeof(args) - 1;
    if (sysctlbyname("kern.bootargs", args, &size, nullptr, 0)) return;
    for (const char *at = strstr(args, "oneios-pool-cut=0"); at; at = strstr(at + 1, "oneios-pool-cut=0")) {
        const char end = at[strlen("oneios-pool-cut=0")];
        if ((at == args || at[-1] == ' ') && (end == 0 || end == ' ')) {
            setenv("METAL2VULKAN_POOL_USE_WINDOW_S", "0", 1);
            return;
        }
    }
}

extern "C" size_t m2v_command_pool_census(uint64_t *out, size_t count) __attribute__((weak));

extern "C" void oneios_gpu_pool_row_start(unsigned port) {
    if (!&m2v_command_pool_census) return;
    static std::atomic<bool> started{false};
    if (started.exchange(true)) return;
    Gpu *gpu = makeGpu(port);
    if (!gpu) return;
    std::thread([gpu]{
        pthread_setname_np("pool-reporter");
        void (*cacheStats)(uint64_t *) = nullptr;
        uint64_t shown[6] = {};
        for (;;) {
            sleep(60);
            if (!cacheStats) {
                char path[1024];
                if (turnip::libraryPath(path, sizeof path))
                    if (void *library = dlopen(path, RTLD_NOW | RTLD_NOLOAD))
                        cacheStats = (void (*)(uint64_t *))dlsym(library, "tu_darwin_bo_cache_stats");
            }
            uint64_t now[6] = {};
            if (m2v_command_pool_census(now, 6) != 6) continue;
            const uint64_t moved = now[1] > shown[1] ? now[1] - shown[1] : shown[1] - now[1];
            if (moved < 32 && now[2] == shown[2] && now[4] == shown[4]) continue;
            uint64_t cache[8] = {};
            if (cacheStats) cacheStats(cache);
            char row[64];
            snprintf(row, sizeof row, "CP %llu/%llu cut +%llu/%llu idle +%llu/%llu tuc %lluM", (unsigned long long)now[0],
                     (unsigned long long)now[1], (unsigned long long)(now[2] - shown[2]), (unsigned long long)(now[3] - shown[3]),
                     (unsigned long long)(now[4] - shown[4]), (unsigned long long)(now[5] - shown[5]),
                     (unsigned long long)(cache[5] >> 20));
            streamText(*gpu, row);
            memcpy(shown, now, sizeof shown);
        }
    }).detach();
}

}  // namespace publish
