// SPDX-License-Identifier: MIT
// Command line, environment and the image set's optional xnuhost.conf.
#include "xnuhost.h"
#include <ctype.h>
#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <strings.h>
#include <sys/stat.h>
#include <unistd.h>

void xh_config_defaults(xh_config *c)
{
  memset(c, 0, sizeof *c);
  c->session_abi = 1;
  strcpy(c->mode, "takeover");
  // One vCPU per core of the host, up to the 8 the phone has (the bridges give them its cluster types).
  long cores = sysconf(_SC_NPROCESSORS_ONLN);
  c->ncpu = cores < 1 ? 1 : cores > XH_MAX_CPUS ? XH_MAX_CPUS : (unsigned)cores;
  c->ram_bytes = 0;                       // ram=auto
  c->ram_percent = -1;
  c->ram_host_floor = -1;
  c->host_mem_log = -1;
  c->usb_log = -1;
  c->ram_giveback = c->ram_prefault = c->ram_lock = -1;
  c->ram_session_reserve = c->ram_unlock_below = c->ram_gpu_reserve = -1;
  c->ram_fit_on = c->ram_pagetable_percent = c->ram_huge = -1;
  c->return_graceful_ms = -1;
  c->ready_content_wait = -1;
  strcpy(c->display, "shm");
  // gpu: "" until xh_config_finish (the command line, the image set, the host device's default, else auto)
  c->input = true;
  c->display_direct = -1;
  c->touch_exclusive = false;  // the touchscreen grab: a bench preview, or session-mode=takeover (which turns it on)
  strcpy(c->session_mode, "hosted");   // the default session, byte for byte as before session-mode existed
  c->touch_sleep = true;
  c->tasks = true;
  c->task_uid = -1;
  c->log_level = XH_LOG_INFO;
  c->bridges = -1;
  c->scale_fixup = -1;
  c->setway_skip = -1;
  c->blk_late_flush = -1;
  c->perf_log = c->spin_host = c->spin_task = c->map_defer = c->cpu_pin = -1;
  c->gpu_priority = -1;
  strcpy(c->bluetooth, "auto");
  strcpy(c->haptics, "auto");
  c->haptics_native_impacts = -1;
  strcpy(c->pen, "off");
  strcpy(c->pen_rotate, "auto");
  strcpy(c->encoder, "off");
  strcpy(c->camera_vm, "hide");
  strcpy(c->camera_model, "OneiOSAP");   // XL_CAMERA_MODEL (xnuloader/machine.h)
  strcpy(c->wifi, "auto");
  br_hw_settings_init(&c->hw);            // every bridge samsung unless hw-backend says otherwise
}

uint64_t xh_parse_size(const char *text, bool *ok)
{
  char *end;
  errno = 0;
  uint64_t v = strtoull(text, &end, 0);
  *ok = end != text && errno == 0;
  switch (*end) {
  case 'k': case 'K': v <<= 10; end++; break;
  case 'm': case 'M': v <<= 20; end++; break;
  case 'g': case 'G': v <<= 30; end++; break;
  }
  if (*end) *ok = false;
  return v;
}

static int copy(char *to, size_t size, const char *from)
{
  if (strlen(from) >= size) return -1;
  strcpy(to, from);
  return 0;
}

static int flag(const char *value, bool *out)
{
  if (!strcmp(value, "1") || !strcmp(value, "yes") || !strcmp(value, "true")) { *out = true; return 0; }
  if (!strcmp(value, "0") || !strcmp(value, "no") || !strcmp(value, "false")) { *out = false; return 0; }
  return -1;
}

// camera-model=: 3 to 15 letters and digits that end in "AP" (CMCapture takes the name less "AP" as a folder name; the
// kernel, hosted 0088, makes a link of that name), so nothing else reaches the device tree or a path.
static bool camera_model_ok(const char *value)
{
  size_t n = strlen(value);
  if (n < 3 || n > 15 || strcmp(value + n - 2, "AP")) return false;
  for (size_t i = 0; i < n; i++) if (!isalnum((unsigned char)value[i])) return false;
  return true;
}

// A whole decimal number in [lo, hi].
static bool number(const char *value, int lo, int hi, int *out)
{
  char *end;
  long v = strtol(value, &end, 10);
  if (end == value || *end || v < lo || v > hi) return false;
  *out = (int)v;
  return true;
}

int xh_config_set(xh_config *c, const char *key, const char *value)
{
  bool ok = true;
  if (!strcmp(key, "session-abi")) c->session_abi = atoi(value);
  else if (!strcmp(key, "mode")) return copy(c->mode, sizeof c->mode, value);
  else if (!strcmp(key, "runtime")) return copy(c->runtime, sizeof c->runtime, value);
  else if (!strcmp(key, "image")) return copy(c->image, sizeof c->image, value);
  else if (!strcmp(key, "data")) return copy(c->data, sizeof c->data, value);
  else if (!strcmp(key, "session")) return copy(c->session, sizeof c->session, value);
  else if (!strcmp(key, "kernel")) return copy(c->kernel, sizeof c->kernel, value);
  else if (!strcmp(key, "identity")) return copy(c->identity, sizeof c->identity, value);
  else if (!strcmp(key, "boot-args")) return copy(c->boot_args, sizeof c->boot_args, value);
  else if (!strcmp(key, "boot-args-add")) return copy(c->boot_args_add, sizeof c->boot_args_add, value);
  else if (!strcmp(key, "display")) return copy(c->display, sizeof c->display, value);
  else if (!strcmp(key, "display-format")) {
    ok = !strcmp(value, "bgrx") || !strcmp(value, "rgba16f");
    if (ok) c->display_format = !strcmp(value, "bgrx") ? XH_PIXELS_BGRX8 : XH_PIXELS_RGBA16F;
  }
  else if (!strcmp(key, "gpu")) { ok = !strcmp(value, "auto") || !strcmp(value, "3d") || !strcmp(value, "2d"); if (ok) return copy(c->gpu, sizeof c->gpu, value); }
  else if (!strcmp(key, "gpu-level")) {
    ok = !strcmp(value, "deadline") || !strcmp(value, "hold") || !strcmp(value, "governor");
    if (ok) return copy(c->gpu_level, sizeof c->gpu_level, value);
  }
  else if (!strcmp(key, "gpu-priority")) { ok = number(value, 0, 1, &c->gpu_priority); }
  else if (!strcmp(key, "cpus")) { c->ncpu = (unsigned)atoi(value); ok = c->ncpu >= 1 && c->ncpu <= XH_MAX_CPUS; }
  else if (!strcmp(key, "ram")) {
    c->ram_bytes = strcmp(value, "auto") ? xh_parse_size(value, &ok) : 0;
    ok = ok && (c->ram_bytes || !strcmp(value, "auto"));
    c->ram_given = ok;
  }
  else if (!strcmp(key, "ram-view")) { c->ram_view = !strcmp(value, "off") ? 0 : !strcmp(value, "auto") ? XH_RAM_VIEW_AUTO : xh_parse_size(value, &ok); ok = ok && c->ram_view < (1ull << 40); c->ram_view_given = ok; }
  else if (!strcmp(key, "ram-percent")) { ok = number(value, 10, 100, &c->ram_percent); }
  else if (!strcmp(key, "ram-host-floor")) { uint64_t v = xh_parse_size(value, &ok); ok = ok && v < (1ull << 40); if (ok) c->ram_host_floor = (int64_t)v; }
  else if (!strcmp(key, "ram-meminfo")) return copy(c->ram_meminfo, sizeof c->ram_meminfo, value);
  else if (!strcmp(key, "host-mem-log")) { ok = number(value, 0, 86400, &c->host_mem_log); }
  else if (!strcmp(key, "usb-log")) { ok = number(value, 0, 3600, &c->usb_log); }
  else if (!strcmp(key, "ram-giveback")) { bool on; if (flag(value, &on)) return -1; c->ram_giveback = on; }
  else if (!strcmp(key, "ram-prefault")) { bool on; if (flag(value, &on)) return -1; c->ram_prefault = on; }
  else if (!strcmp(key, "ram-lock")) { bool on; if (flag(value, &on)) return -1; c->ram_lock = on; }
  else if (!strcmp(key, "ram-session-reserve")) { uint64_t v = xh_parse_size(value, &ok); ok = ok && v < (1ull << 40); if (ok) c->ram_session_reserve = (int64_t)v; }
  else if (!strcmp(key, "ram-fit")) { bool on; if (flag(value, &on)) return -1; c->ram_fit_on = on; }
  else if (!strcmp(key, "ram-huge")) { bool on; if (flag(value, &on)) return -1; c->ram_huge = on; }
  else if (!strcmp(key, "ram-gpu-reserve")) { uint64_t v = xh_parse_size(value, &ok); ok = ok && v < (1ull << 40); if (ok) c->ram_gpu_reserve = (int64_t)v; }
  else if (!strcmp(key, "ram-pagetable-percent")) { ok = number(value, 0, 50, &c->ram_pagetable_percent); }
  else if (!strcmp(key, "ram-unlock-below")) { uint64_t v = xh_parse_size(value, &ok); ok = ok && v < (1ull << 40); if (ok) c->ram_unlock_below = (int64_t)v; }
  else if (!strcmp(key, "return-graceful-ms")) { ok = number(value, 0, 120000, &c->return_graceful_ms); }
  else if (!strcmp(key, "ready-content-wait")) { ok = number(value, 0, 480, &c->ready_content_wait); }
  else if (!strcmp(key, "width")) c->width = (unsigned)atoi(value);
  else if (!strcmp(key, "height")) c->height = (unsigned)atoi(value);
  else if (!strcmp(key, "hz")) { c->hz = (unsigned)atoi(value); ok = c->hz == 0 || (c->hz >= 24 && c->hz <= 240); }
  else if (!strcmp(key, "panel-fallback")) {
    // WxH or WxH@HZ: the mode a session assumes when the panel cannot be read (xh_panel_fallback)
    unsigned w = 0, h = 0, hz = 0;
    int n = 0, end = 0;
    if ((sscanf(value, "%5ux%5u%n", &w, &h, &end) == 2 && !value[end] && (n = 2)) ||
        (sscanf(value, "%5ux%5u@%3u%n", &w, &h, &hz, &end) == 3 && !value[end] && (n = 3)))
      ok = w && h && w <= 16384 && h <= 16384 && (n == 2 || (hz >= 24 && hz <= 240));
    else ok = false;
    if (ok) { c->fallback_width = w; c->fallback_height = h; c->fallback_hz = hz; }
  }
  else if (!strcmp(key, "scale")) { c->scale = (unsigned)atoi(value); ok = c->scale == 2 || c->scale == 3; }
  else if (!strcmp(key, "scale-fixup")) { bool on; if (flag(value, &on)) return -1; c->scale_fixup = on; }
  else if (!strcmp(key, "text-fixup")) {
    // FILE,OFFSET,ORIGINAL,REPLACEMENT in hex, as UniXNU's [payload] text_fixups (the kernel checks them again)
    size_t n = strlen(value);
    unsigned commas = 0;
    for (size_t i = 0; i < n; i++) commas += value[i] == ',';
    if (commas != 3 || n >= 160 || c->text_fixups_length + n + 1 > sizeof c->text_fixups) return -1;
    memcpy(c->text_fixups + c->text_fixups_length, value, n + 1);
    c->text_fixups_length += (unsigned)n + 1;
  }
  else if (!strcmp(key, "input")) return flag(value, &c->input);
  else if (!strcmp(key, "display-direct")) { bool on; if (flag(value, &on)) return -1; c->display_direct = on; }
  else if (!strcmp(key, "touch-exclusive")) { if (flag(value, &c->touch_exclusive)) return -1; c->touch_exclusive_given = true; }
  else if (!strcmp(key, "session-mode")) {
    // hosted (the default) or takeover (docs/booter.md 2.9d): xh_config_finish checks the host table and the mode
    ok = !strcmp(value, "hosted") || !strcmp(value, "takeover");
    if (ok) { strcpy(c->session_mode, value); c->session_mode_given = true; }
  }
  else if (!strcmp(key, "input-script")) return copy(c->input_script, sizeof c->input_script, value);
  else if (!strcmp(key, "tasks")) return flag(value, &c->tasks);
  else if (!strcmp(key, "task-uid")) c->task_uid = atoi(value);
  else if (!strcmp(key, "touch-sleep")) return flag(value, &c->touch_sleep);
  else if (!strcmp(key, "pen")) { ok = !strcmp(value, "auto") || !strcmp(value, "off"); if (ok) { strcpy(c->pen, value); c->pen_given = true; } }
  else if (!strcmp(key, "pen-rotate")) { ok = !strcmp(value, "auto") || !strcmp(value, "0") || !strcmp(value, "90") || !strcmp(value, "180") || !strcmp(value, "270"); if (ok) { strcpy(c->pen_rotate, value); c->pen_rotate_given = true; } }
  else if (!strcmp(key, "pen-mirror")) { if (flag(value, &c->pen_mirror)) return -1; c->pen_mirror_given = true; }
  else if (!strcmp(key, "encoder")) { ok = !strcmp(value, "auto") || !strcmp(value, "off") || !strcmp(value, "sim"); if (ok) { strcpy(c->encoder, value); c->encoder_given = true; } }
  else if (!strcmp(key, "camera-vm")) { ok = !strcmp(value, "hide") || !strcmp(value, "show"); if (ok) { strcpy(c->camera_vm, value); c->camera_vm_given = true; } }
  else if (!strcmp(key, "camera-model")) { ok = camera_model_ok(value); if (ok) { strcpy(c->camera_model, value); c->camera_model_given = true; } }
  else if (!strcmp(key, "bridges")) { bool on; if (flag(value, &on)) return -1; c->bridges = on; }
  else if (!strcmp(key, "clusters")) { ok = strlen(value) <= XH_MAX_CPUS && strspn(value, "EP") == strlen(value); if (ok) strcpy(c->clusters, value); }
  else if (!strcmp(key, "wifi")) { ok = !strcmp(value, "auto") || !strcmp(value, "off") || !strcmp(value, "tap") || !strcmp(value, "sim"); if (ok) { strcpy(c->wifi, value); c->wifi_given = true; } }
  else if (!strcmp(key, "bluetooth")) { ok = !strcmp(value, "auto") || !strcmp(value, "virtual") || !strcmp(value, "hidl") || !strcmp(value, "sim"); if (ok) { strcpy(c->bluetooth, value); c->bluetooth_given = true; } }
  else if (!strcmp(key, "haptics")) { ok = !strcmp(value, "auto") || !strcmp(value, "off") || !strcmp(value, "sim"); if (ok) { strcpy(c->haptics, value); c->haptics_given = true; } }
  else if (!strcmp(key, "haptics-native-impacts")) { bool on; if (flag(value, &on)) return -1; c->haptics_native_impacts = on; }
  else if (!strcmp(key, "legacy-boot-args")) return flag(value, &c->legacy_boot_args);
  else if (!strcmp(key, "setway-skip")) { bool on; if (flag(value, &on)) return -1; c->setway_skip = on; }
  else if (!strcmp(key, "blk-late-flush")) { bool on; if (flag(value, &on)) return -1; c->blk_late_flush = on; }
  else if (!strcmp(key, "perf-log")) { ok = number(value, 0, 86400, &c->perf_log); }
  else if (!strcmp(key, "spin-host")) { ok = number(value, 0, 1000, &c->spin_host); }
  else if (!strcmp(key, "spin-task")) { ok = number(value, 0, 1000, &c->spin_task); }
  else if (!strcmp(key, "cpu-pin")) { if (!strcmp(value, "2")) c->cpu_pin = 2; else { bool on; if (flag(value, &on)) return -1; c->cpu_pin = on; } }
  else if (!strcmp(key, "map-defer")) { bool on; if (flag(value, &on)) return -1; c->map_defer = on; }
  else if (!strncmp(key, "hw-backend", 10)) {
    // hw-backend, hw-backend-<bridge>, hw-backend-fallback (bridges/hw_backend.h)
    int taken = br_hw_setting(&c->hw, key, value);
    if (taken > 0) return -1;
    ok = taken == 0;
  }
  else if (!strcmp(key, "host-device")) {
    // auto, none or a name of the host table (bridges/host_device.h)
    ok = !strcasecmp(value, "auto") || !strcasecmp(value, "none") || br_host_find(value);
    if (ok) return copy(c->host_device, sizeof c->host_device, value);
  }
  else if (!strcmp(key, "host-props")) return copy(c->host_props, sizeof c->host_props, value);
  else if (!strcmp(key, "host-root")) return copy(c->host_root, sizeof c->host_root, value);
  else if (!strcmp(key, "log-level")) c->log_level = atoi(value);
  else if (!strcmp(key, "disk") || !strcmp(key, "disk-rw") || !strcmp(key, "data-store")) {
    // disk=FILE (read-only), disk-rw=FILE[,SEED], data-store=FILE[,SEED]
    if (c->ndisk == XH_MAX_DISKS) return -1;
    xh_disk_config *d = &c->disk[c->ndisk];
    memset(d, 0, sizeof *d);
    const char *comma = strchr(value, ',');
    size_t n = comma ? (size_t)(comma - value) : strlen(value);
    if (n == 0 || n >= sizeof d->path) return -1;
    memcpy(d->path, value, n);
    if (comma && copy(d->seed, sizeof d->seed, comma + 1)) return -1;
    d->writable = strcmp(key, "disk") != 0;
    d->data_store = !strcmp(key, "data-store");
    if (comma && !d->writable) return -1;
    c->ndisk++;
  } else return -1;
  return ok ? 0 : -1;
}

int xh_config_file(xh_config *c, const char *path)
{
  FILE *f = fopen(path, "r");
  if (!f) return errno == ENOENT ? 0 : -1;
  char line[1400];
  int rc = 0, number = 0;
  while (fgets(line, sizeof line, f)) {
    number++;
    char *p = line;
    while (isspace((unsigned char)*p)) p++;
    size_t n = strlen(p);
    while (n && isspace((unsigned char)p[n - 1])) p[--n] = 0;
    if (!*p || *p == '#') continue;
    char *eq = strchr(p, '=');
    if (!eq) { rc = -1; break; }
    *eq = 0;
    if (xh_config_set(c, p, eq + 1)) { fprintf(stderr, "xnuhost: %s:%d: bad setting %s\n", path, number, p); rc = -1; break; }
  }
  fclose(f);
  return rc;
}

// haptics-native-impacts=1 (docs/hardware.md 5, "Native impacts"): the guest's half of the opt-in, oneios_native_impacts=1,
// added to the boot arguments as they are (never in place of them). 0, or -1 when they already say
// oneios_native_impacts= with another value or twice, or there is no room.
static int native_impacts_boot_arg(char *args, size_t size)
{
  static const char key[] = "oneios_native_impacts=";
  const size_t klen = sizeof key - 1;
  bool seen = false;
  for (const char *p = args; *p;) {
    while (*p == ' ') p++;
    size_t len = strcspn(p, " ");
    if (len >= klen && !strncmp(p, key, klen)) {
      if (seen || len != klen + 1 || p[klen] != '1') return -1;
      seen = true;
    }
    p += len;
  }
  if (seen) return 0;
  const char *add = *args ? " oneios_native_impacts=1" : "oneios_native_impacts=1";
  if (strlen(args) + strlen(add) >= size) return -1;
  strcat(args, add);
  return 0;
}

// Whether `args` has a boot argument named `name` (its first `n` bytes): a word equal to it, or one that starts with
// it and then '='.
static bool boot_arg_named(const char *args, const char *name, size_t n)
{
  for (const char *p = args; *p;) {
    while (*p == ' ') p++;
    if (!*p) break;
    size_t len = strcspn(p, " ");
    if (len >= n && !strncmp(p, name, n) && (len == n || p[n] == '=')) return true;
    p += len;
  }
  return false;
}

// boot-args-add (ported from YumikoHayamato/OneiOS, s25-ultra-sm8750 440abb2): each of its words is added to `args` unless
// `args` names that argument already, so what the command line or the image set wrote in boot-args wins, and a profile
// adds one argument (oneios-disable-jobs=..., oneios-ubt=...) without repeating the defaults: a boot-args= line that
// replaced them was the cause of logo-only stalls on the S25 Ultra (docs/s25.md). -1 when the result does not fit.
static int add_boot_args(char *args, size_t capacity, const char *add)
{
  for (const char *p = add; *p;) {
    while (*p == ' ') p++;
    if (!*p) break;
    size_t len = strcspn(p, " "), name = strcspn(p, "= ");
    if (!boot_arg_named(args, p, name < len ? name : len)) {
      size_t at = strlen(args), gap = at ? 1 : 0;
      if (at + gap + len >= capacity) return -1;
      if (gap) args[at++] = ' ';
      memcpy(args + at, p, len);
      args[at + len] = 0;
    }
    p += len;
  }
  return 0;
}

static void usage(void)
{
  fputs("usage: xnuhost --image DIR [--kernel FILE] [--data DIR] [--session DIR] [--runtime DIR]\n"
        "               [--session-abi 1] [--mode takeover|bench] [--cpus N] [--ram SIZE|auto]\n"
        "               [--boot-args TEXT] [--display shm|drm|none] [--gpu auto|3d|2d] [--width W --height H]\n"
        "               [--hz N] [--scale 2|3] [--display-format bgrx|rgba16f]\n"
        "               [--hw-backend samsung|treble] [--hw-backend-BRIDGE samsung|treble] [--hw-backend-fallback 0|1]\n"
        "               [--set haptics-native-impacts=0|1] (1: needs hw-backend-haptics=treble, hw-backend-fallback=0)\n"
        "               [--host-device auto|none|NAME] [--host-props FILE] [--host-root DIR]\n"
        "               [--no-input] [--input-script FILE] [--set touch-exclusive=1] [--no-tasks] [--config FILE] [--set KEY=VALUE] [-v]\n"
        "               [--set session-mode=hosted|takeover]\n"
        "       xnuhost --selftest      checks of this host (trap layer, PAC, memory), no kernel\n"
        "       xnuhost --host-info [--host-device ...] [--host-props FILE]   this phone's row of the host table, one line\n"
        "docs/booter.md describes the image set's files, the session files and the exit codes.\n", stderr);
}

int xh_config_args(xh_config *c, int argc, char **argv)
{
  static const struct { const char *env, *key; } envs[] = {
    { "ONEIOS_SESSION_ABI", "session-abi" }, { "ONEIOS_MODE", "mode" }, { "ONEIOS_RUNTIME", "runtime" },
    { "ONEIOS_IMAGE", "image" }, { "ONEIOS_DATA", "data" }, { "ONEIOS_SESSION", "session" },
  };
  for (unsigned i = 0; i < sizeof envs / sizeof *envs; i++) {
    const char *v = getenv(envs[i].env);
    if (v && *v && xh_config_set(c, envs[i].key, v)) { fprintf(stderr, "xnuhost: bad %s\n", envs[i].env); return -1; }
  }
  for (int i = 1; i < argc; i++) {
    const char *a = argv[i];
    if (!strcmp(a, "-v")) { c->log_level = XH_LOG_DEBUG; continue; }
    if (!strcmp(a, "--selftest")) { c->selftest = true; continue; }
    if (!strcmp(a, "--host-info")) { c->host_info = true; continue; }
    if (!strcmp(a, "--no-input")) { c->input = false; continue; }
    if (!strcmp(a, "--no-tasks")) { c->tasks = false; continue; }
    if (!strcmp(a, "-h") || !strcmp(a, "--help")) { usage(); return -1; }
    if (strncmp(a, "--", 2) || i + 1 >= argc) { fprintf(stderr, "xnuhost: unknown or incomplete argument %s\n", a); usage(); return -1; }
    const char *v = argv[++i];
    if (!strcmp(a, "--config")) {
      if (xh_config_file(c, v)) { fprintf(stderr, "xnuhost: cannot use %s\n", v); return -1; }
    } else if (!strcmp(a, "--set")) {
      char pair[1200];
      const char *eq = strchr(v, '=');
      if (!eq || strlen(v) >= sizeof pair) { fprintf(stderr, "xnuhost: --set takes KEY=VALUE\n"); return -1; }
      strcpy(pair, v);
      pair[eq - v] = 0;
      if (xh_config_set(c, pair, eq + 1)) { fprintf(stderr, "xnuhost: bad setting %s\n", pair); return -1; }
    } else if (xh_config_set(c, a + 2, v)) {
      fprintf(stderr, "xnuhost: bad value for %s\n", a);
      return -1;
    }
  }
  return 0;
}

static bool exists(const char *dir, const char *name, char *out, size_t size)
{
  struct stat st;
  if (snprintf(out, size, "%s/%s", dir, name) >= (int)size) return false;
  return stat(out, &st) == 0 && S_ISREG(st.st_mode);
}

// The host phone's layer (bridges/host_device.h; docs/hardware.md, "The host device"): `device` gets the table entry's
// xnuhost settings over the defaults, and c->host and c->host_line say what was found and what of it is taken. Only the
// keys below are merged as device defaults (xh_config_finish: gpu, hw-backend*); a table entry with another key is
// refused here, so a new default cannot be added to the table without its merge (test_xnuhost checks every entry).
static bool device_key(const char *key) { return !strcmp(key, "gpu") || !strcmp(key, "panel-fallback") || !strncmp(key, "hw-backend", 10); }

// Whether the command line or the image set (`over`, the two already merged for hw) gives the key of setting `key=value`.
static bool given_above(const xh_config *c, const xh_config *image, const br_hw_settings *over, const char *key, const char *value)
{
  if (!strcmp(key, "gpu")) return *c->gpu || *image->gpu;
  if (!strcmp(key, "panel-fallback")) return c->fallback_width || image->fallback_width;
  br_hw_settings one;
  br_hw_settings_init(&one);
  if (br_hw_setting(&one, key, value)) return false;
  if (one.all && over->all) return true;
  if (one.fallback >= 0 && over->fallback >= 0) return true;
  for (unsigned i = 0; i < BR_HW_NBRIDGE; i++) if (one.one[i] && over->one[i]) return true;
  return false;
}

static int host_device_layer(xh_config *c, const xh_config *image, xh_config *device)
{
  xh_config_defaults(device);
  if (!*c->host_device) strcpy(c->host_device, *image->host_device ? image->host_device : "auto");
  if (!*c->host_props) strcpy(c->host_props, image->host_props);
  if (!*c->host_root) strcpy(c->host_root, image->host_root);
  br_host_facts f;
  char why[260], read[400], names[200];
  c->host = NULL;
  if (br_host_choose(c->host_device, c->host_props, c->host_root, &f, &c->host, why, sizeof why)) {
    br_host_names(names, sizeof names);
    fprintf(stderr, "xnuhost: %s (host-device: auto, none or one of %s)\n", why, names);
    return -1;
  }
  if (!c->host) {
    if (!strcasecmp(c->host_device, "none")) snprintf(c->host_line, sizeof c->host_line, "host: detection off (host-device=none): today's defaults");
    else {
      br_host_names(names, sizeof names);
      br_host_describe(&f, read, sizeof read);
      snprintf(c->host_line, sizeof c->host_line, "host: unknown phone (read: %s): today's defaults (host-device=NAME forces one of %s)", read, names);
    }
    return 0;
  }
  br_hw_settings over = c->hw;
  br_hw_settings_merge(&over, &image->hw);
  char taken[300] = "", env[200];
  size_t used = 0;
  for (unsigned i = 0; i < BR_HOST_SETTINGS && c->host->config[i]; i++) {
    const char *pair = c->host->config[i], *eq = strchr(pair, '=');
    char key[64];
    if (!eq || (size_t)(eq - pair) >= sizeof key) { fprintf(stderr, "xnuhost: host table: %s: %s is not KEY=VALUE\n", c->host->name, pair); return -1; }
    memcpy(key, pair, (size_t)(eq - pair));
    key[eq - pair] = 0;
    if (!device_key(key) || xh_config_set(device, key, eq + 1)) {
      fprintf(stderr, "xnuhost: host table: %s: %s is not a device default xnuhost takes\n", c->host->name, pair);
      return -1;
    }
    bool above = given_above(c, image, &over, key, eq + 1);
    if (used < sizeof taken)
      used += (size_t)snprintf(taken + used, sizeof taken - used, "%s%s%s", used ? ", " : "", pair, above ? " not taken (the command line or the image set gives it)" : "");
  }
  br_host_env_describe(c->host, env, sizeof env);
  br_host_describe(&f, read, sizeof read);
  snprintf(c->host_line, sizeof c->host_line, "host: %s (%s), %s; read: %s; defaults: %s%s%s", c->host->name, c->host->label, why, read,
           *taken || *env ? taken : "none (today's)", *taken && *env ? ", " : "", env);
  return 0;
}

int xh_config_finish(xh_config *c)
{
  char path[512];
  if (c->session_abi != 1) { fprintf(stderr, "xnuhost: session ABI %d is not supported (this build: 1)\n", c->session_abi); return -1; }
  if (strcmp(c->mode, "takeover") && strcmp(c->mode, "bench")) { fprintf(stderr, "xnuhost: mode %s is not supported\n", c->mode); return -1; }
  if (strcmp(c->display, "shm") && strcmp(c->display, "drm") && strcmp(c->display, "none")) { fprintf(stderr, "xnuhost: display %s is not supported\n", c->display); return -1; }
  // The image set may carry settings of its own; the command line was applied first and a setting
  // there wins for single values (disks add up). Not read for a selftest, which needs no image.
  xh_config from_image;
  xh_config_defaults(&from_image);
  if (!c->selftest) {
    if (!*c->image) { fprintf(stderr, "xnuhost: --image DIR is required\n"); return -1; }
    if (snprintf(path, sizeof path, "%s/xnuhost.conf", c->image) < (int)sizeof path && xh_config_file(&from_image, path)) return -1;
    // The userland hook's settings of this image (the UI scale its Data seed was made for), under the same rule.
    if (snprintf(path, sizeof path, "%s/overlay/userland/xnuhost.conf", c->image) < (int)sizeof path && xh_config_file(&from_image, path)) return -1;
  }
  // The host bridges change the phone's state (a tap, routing rules, the motor): on in a session. bridges=, wifi=,
  // bluetooth= and haptics= of the image set apply when the command line has none (until 2026-11-21 they were read
  // and then ignored, though docs/booter.md and main.c's Bluetooth line said otherwise).
  if (c->bridges < 0) c->bridges = from_image.bridges >= 0 ? from_image.bridges : !strcmp(c->mode, "takeover");
  if (!c->wifi_given && from_image.wifi_given) strcpy(c->wifi, from_image.wifi);
  if (!c->bluetooth_given && from_image.bluetooth_given) strcpy(c->bluetooth, from_image.bluetooth);
  if (!c->haptics_given && from_image.haptics_given) strcpy(c->haptics, from_image.haptics);
  // pen=, pen-rotate=, pen-mirror= of the image set (its overlay/userland/xnuhost.conf on the phone). Until 2026-11-22
  // they were read and then dropped: the phone's pen=auto never turned the pen on (no `pen:` line in any session).
  if (!c->pen_given && from_image.pen_given) strcpy(c->pen, from_image.pen);
  if (!c->pen_rotate_given && from_image.pen_rotate_given) strcpy(c->pen_rotate, from_image.pen_rotate);
  if (!c->pen_mirror_given && from_image.pen_mirror_given) c->pen_mirror = from_image.pen_mirror;
  // encoder= of the image set: the way a phone session turns the video encoder on (the OneOS app passes no --set
  // encoder=). Until 2026-11-22 it was read and then dropped, so encoder=auto in xnuhost.conf did nothing.
  if (!c->encoder_given && from_image.encoder_given) strcpy(c->encoder, from_image.encoder);
  if (!c->camera_vm_given && from_image.camera_vm_given) strcpy(c->camera_vm, from_image.camera_vm);
  if (!c->camera_model_given && from_image.camera_model_given) strcpy(c->camera_model, from_image.camera_model);
  // session-mode= of the image set (its overlay/userland/xnuhost.conf) when the command line has none.
  if (!c->session_mode_given && from_image.session_mode_given) strcpy(c->session_mode, from_image.session_mode);
  const bool takeover_mode = !strcmp(c->session_mode, "takeover");
  // The touchscreen grab, outside the opt-in Android takeover, is a bounded non-takeover diagnostic only: never let an
  // input-grabbing guest take control of Android outside a SHM bench.
  if (c->touch_exclusive && !takeover_mode && (strcmp(c->mode, "bench") || strcmp(c->display, "shm") ||
                                               !c->input || c->bridges)) {
    fprintf(stderr, "xnuhost: touch-exclusive=1 requires session-mode=takeover, or mode=bench, display=shm, input=1, bridges=0\n");
    return -1;
  }
  if (c->selftest) { if (!*c->gpu) strcpy(c->gpu, "auto"); return 0; }
  // The host phone's defaults (bridges/host_device.h), under both: the command line > the image set > the table > built in.
  xh_config device;
  if (host_device_layer(c, &from_image, &device)) return -1;
  // session-mode=takeover (docs/booter.md 2.9d): only in the OneOS session, only where the host table allows it (the
  // switch asked --host-info before it stopped One UI; this is the same rule, for a command line that did not), and
  // with input: it turns the native touchscreen's grab on unless touch-exclusive= was given.
  if (takeover_mode) {
    if (strcmp(c->mode, "takeover")) { fprintf(stderr, "xnuhost: session-mode=takeover needs mode=takeover (the OneOS session)\n"); return -1; }
    if (!c->host || !c->host->takeover) {
      fprintf(stderr, "xnuhost: session-mode=takeover is not supported on this phone (%s): the host table "
              "(runtime/bridges/hw/host_device.c) does not allow it; session-mode=hosted is the session as before\n",
              c->host ? c->host->name : "no row of the host table");
      return -1;
    }
    if (!c->input) { fprintf(stderr, "xnuhost: session-mode=takeover needs input=1 (its touchscreen grab)\n"); return -1; }
    if (!c->touch_exclusive_given) c->touch_exclusive = true;
  }
  if (!*c->gpu) strcpy(c->gpu, *from_image.gpu ? from_image.gpu : *device.gpu ? device.gpu : "auto");
  if (!*c->boot_args) strcpy(c->boot_args, from_image.boot_args);
  if (!c->width) c->width = from_image.width;
  if (c->setway_skip < 0) c->setway_skip = from_image.setway_skip < 0 ? 1 : from_image.setway_skip;
  if (c->blk_late_flush < 0) c->blk_late_flush = from_image.blk_late_flush < 0 ? 1 : from_image.blk_late_flush;
  if (c->perf_log < 0) c->perf_log = from_image.perf_log;
  if (c->spin_host < 0) c->spin_host = from_image.spin_host;
  if (!*c->gpu_level) strcpy(c->gpu_level, *from_image.gpu_level ? from_image.gpu_level : "deadline");
  if (c->gpu_priority < 0) c->gpu_priority = from_image.gpu_priority < 0 ? 1 : from_image.gpu_priority;
  if (c->spin_task < 0) c->spin_task = from_image.spin_task;
  if (c->map_defer < 0) c->map_defer = from_image.map_defer;
  if (c->cpu_pin < 0) c->cpu_pin = from_image.cpu_pin < 0 ? 1 : from_image.cpu_pin;
  br_hw_settings_merge(&c->hw, &from_image.hw);     // each hw-backend setting: the command line's, else the image set's,
  br_hw_settings_merge(&c->hw, &device.hw);         // else the host phone's
  // haptics-native-impacts: the command line's, else the image set's, else unset (-1: ONEIOS_HAPTIC_NATIVE_IMPACTS
  // decides, as before). 1 only with the vibrator HAL and no fallback to evdev: the presets are the HAL's.
  if (c->haptics_native_impacts < 0) c->haptics_native_impacts = from_image.haptics_native_impacts;
  if (c->haptics_native_impacts == 1 &&
      (!c->bridges || strcmp(c->haptics, "auto") || br_hw_choose(&c->hw, BR_HW_HAPTICS, BR_HW_UNSET, NULL, 0) != BR_HW_TREBLE ||
       br_hw_fallback_allowed(&c->hw))) {
    fprintf(stderr, "xnuhost: haptics-native-impacts=1 needs bridges=1, haptics=auto, hw-backend-haptics=treble and hw-backend-fallback=0\n");
    return -1;
  }
  if (c->map_defer < 0) c->map_defer = 1;
  if (!c->height) c->height = from_image.height;
  if (!c->hz) c->hz = from_image.hz;
  if (!c->fallback_width) { c->fallback_width = from_image.fallback_width; c->fallback_height = from_image.fallback_height; c->fallback_hz = from_image.fallback_hz; }
  if (!c->fallback_width) { c->fallback_width = device.fallback_width; c->fallback_height = device.fallback_height; c->fallback_hz = device.fallback_hz; }   // else the host phone's
  if (c->display_direct < 0) c->display_direct = from_image.display_direct < 0 ? 1 : from_image.display_direct;
  if (!c->display_format) c->display_format = from_image.display_format ? from_image.display_format : XH_PIXELS_BGRX8;
  // The KMS back end scans out 32-bit pixels: half floats only go to a display helper (shm) or nowhere.
  if (c->display_format == XH_PIXELS_RGBA16F && !strcmp(c->display, "drm")) { fprintf(stderr, "xnuhost: display-format=rgba16f needs --display shm\n"); return -1; }
  if (!c->scale) c->scale = from_image.scale ? from_image.scale : 3;
  if (c->scale_fixup < 0) c->scale_fixup = from_image.scale_fixup < 0 ? 1 : from_image.scale_fixup;
  if (!c->text_fixups_length) { memcpy(c->text_fixups, from_image.text_fixups, sizeof c->text_fixups); c->text_fixups_length = from_image.text_fixups_length; }
  // Neither the command line nor the image set gave any: what the hosted kernel needs to start on this
  // machine (the session's command line has no boot arguments). From Droid-VM's ipados27/board.toml
  // kernel_args, the ones about the machine: SMP and ArmCpu's IPIs, no contiguous-hint mappings (the
  // collection's layout: arm_vm_init.c:1107 asserts without it), no interrupt-masked or preemption watchdogs
  // (a vCPU is a host thread), the source storage, the GPU's 3D context, the console in the log.
  if (!*c->boot_args)
    strcpy(c->boot_args, "debug=0x14e serial=3 -v kt_fixed_zones=120 -no_slto_panic ml-timeout-lock-panic-scale=40 tlto_us=0 "
           "sheng-smp=1 armcpu-self-ipi=1 armcpu-spi-ipi=1 use_contiguous_hint=0 interrupt-masked-debug-mode=0 "
           "sched_preemption_disable_debug_mode=0 wdt=-1 native-source-storage=1 vgpu-3d=1 droidvm-gpu-metal=1");
  if (!*c->boot_args_add) strcpy(c->boot_args_add, from_image.boot_args_add);
  if (add_boot_args(c->boot_args, sizeof c->boot_args, c->boot_args_add)) {
    fprintf(stderr, "xnuhost: boot-args-add does not fit beside the guest boot args (%zu bytes at most)\n", sizeof c->boot_args - 1);
    return -1;
  }
  if (c->haptics_native_impacts == 1 && native_impacts_boot_arg(c->boot_args, sizeof c->boot_args)) {
    fprintf(stderr, "xnuhost: haptics-native-impacts=1: the boot arguments say oneios_native_impacts= otherwise, or are too long\n");
    return -1;
  }
  if (c->ndisk == 0) { memcpy(c->disk, from_image.disk, sizeof c->disk); c->ndisk = from_image.ndisk; }
  if (!*c->kernel && !exists(c->image, "kernel.kc", c->kernel, sizeof c->kernel) &&
      !exists(c->image, "kernel.macho", c->kernel, sizeof c->kernel)) {
    fprintf(stderr, "xnuhost: no kernel.kc in %s (and no --kernel)\n", c->image);
    return -1;
  }
  if (!*c->identity && !exists(c->image, "identity.adt", c->identity, sizeof c->identity)) c->identity[0] = 0;
  if (c->ndisk == 0) {
    // The image set's volumes (scripts/image/build.sh): read-only APFS containers, in this order.
    static const char *const names[] = { "os.img", "systemos.img", "app.img" };
    for (unsigned i = 0; i < 3; i++)
      if (exists(c->image, names[i], path, sizeof path)) {
        xh_disk_config *d = &c->disk[c->ndisk++];
        memset(d, 0, sizeof *d);
        strcpy(d->path, path);
      }
    if (*c->data) {
      xh_disk_config *d = &c->disk[c->ndisk++];
      memset(d, 0, sizeof *d);
      if (snprintf(d->path, sizeof d->path, "%s/data.img", c->data) >= (int)sizeof d->path) return -1;
      d->writable = d->data_store = true;
      if (exists(c->image, "data.seed", path, sizeof path)) strcpy(d->seed, path);
    }
    // The Data seed is a tar (docs/image-format.md): Linux cannot write APFS, so the guest makes the
    // Data volume itself on the first start and unpacks the seed, which it reads as the last
    // read-only disk. (An APFS seed image is copied into the store instead: blk.c.)
    if (exists(c->image, "data.seed", path, sizeof path)) {
      xh_disk_config *d = &c->disk[c->ndisk++];
      memset(d, 0, sizeof *d);
      strcpy(d->path, path);
    }
  }
  // The guest's RAM: the command line's ram=, else the image set's, else auto.
  if (!c->ram_given && from_image.ram_given) c->ram_bytes = from_image.ram_bytes;
  if (!c->ram_view_given) c->ram_view = from_image.ram_view;     // ram-view= (main.c, docs/booter.md 2.2 "The memory view")
  if (c->ram_percent < 0) c->ram_percent = from_image.ram_percent < 0 ? XH_RAM_AUTO_PERCENT : from_image.ram_percent;
  if (c->ram_host_floor < 0) c->ram_host_floor = from_image.ram_host_floor < 0 ? (int64_t)XH_RAM_AUTO_FLOOR : from_image.ram_host_floor;
  if (c->host_mem_log < 0) c->host_mem_log = from_image.host_mem_log < 0 ? 30 : from_image.host_mem_log;
  if (c->usb_log < 0) c->usb_log = from_image.usb_log < 0 ? 3 : from_image.usb_log;
  // Default 0 (today's immediate kill): the graceful path's guest half (iOS actually powering off) is not yet built
  // or tested end to end, so it is off until the Mac/phone proves it. return-graceful-ms=N (or the image's
  // xnuhost.conf) turns it on. The host state machine itself is tested (docs/booter.md "The return").
  if (c->return_graceful_ms < 0) c->return_graceful_ms = from_image.return_graceful_ms < 0 ? 0 : from_image.return_graceful_ms;
  // session/ready at the first lit frame of the guest's own, or this many seconds after its first (black) one (ready.c).
  if (c->ready_content_wait < 0) c->ready_content_wait = from_image.ready_content_wait < 0 ? XH_READY_CONTENT_WAIT : from_image.ready_content_wait;
  if (!*c->ram_meminfo) strcpy(c->ram_meminfo, *from_image.ram_meminfo ? from_image.ram_meminfo : "/proc/meminfo");
  // The RAM held fixed and locked for the session (ramfix.c): the default since fix-ram-locked (2026-11-15; the owner's
  // rule: iOS's RAM is never swapped). The phone's session of main 3233fed showed why: with ram-giveback=1 at a fitted
  // 6656 MiB Linux still swapped iOS's RAM (shmem) with 1.9 GB of other page cache there, 138 -> 604 MiB at up to
  // 37.9 MB/s of swap-outs, and only a lock stops that. ram-giveback=1 is 0058's way again (one setting).
  if (c->ram_giveback < 0) c->ram_giveback = from_image.ram_giveback < 0 ? 0 : from_image.ram_giveback;
  if (c->ram_prefault < 0) c->ram_prefault = from_image.ram_prefault < 0 ? !c->ram_giveback : from_image.ram_prefault;
  if (c->ram_lock < 0) c->ram_lock = from_image.ram_lock < 0 ? !c->ram_giveback : from_image.ram_lock;
  if (c->ram_giveback) c->ram_prefault = c->ram_lock = 0;        // a RAM that gives pages back is neither prefaulted nor locked
  if (!c->ram_prefault) c->ram_lock = 0;
  if (c->ram_unlock_below < 0) c->ram_unlock_below = from_image.ram_unlock_below < 0 ? (int64_t)XH_RAM_UNLOCK_BELOW : from_image.ram_unlock_below;
  // A locked RAM always has its valve: under it the host may swap the guest again rather than reclaim every other
  // process's pages (the fork's watcher's, surfaceflinger's) on the way to its OOM killer. The way back must fit.
  if (c->ram_lock && c->ram_unlock_below < (int64_t)XH_RAM_UNLOCK_MIN) {
    fprintf(stderr, "xnuhost: ram-unlock-below=%lld MiB is under %llu MiB with the RAM locked: %llu MiB is used (the valve stays on)\n",
            (long long)(c->ram_unlock_below >> 20), XH_RAM_UNLOCK_MIN >> 20, XH_RAM_UNLOCK_MIN >> 20);
    c->ram_unlock_below = (int64_t)XH_RAM_UNLOCK_MIN;
  }
  // The headroom: smaller with a lock that can work (no zram pool or working set for the guest's pages), which is asked
  // here, before the size is known (xh_ram_can_lock: CAP_IPC_LOCK, or no RLIMIT_MEMLOCK).
  c->ram_lock_expected = c->ram_lock && xh_ram_can_lock();
  if (c->ram_session_reserve < 0)
    c->ram_session_reserve = from_image.ram_session_reserve >= 0 ? from_image.ram_session_reserve :
                             (int64_t)(c->ram_lock_expected ? XH_RAM_SESSION_RESERVE_LOCKED : XH_RAM_SESSION_RESERVE);
  // With the lock, what fits leaves at least the valve's level and a margin: else the valve would open as soon as the
  // session is up.
  if (c->ram_lock && c->ram_session_reserve < c->ram_unlock_below + (int64_t)XH_RAM_VALVE_MARGIN)
    c->ram_session_reserve = c->ram_unlock_below + (int64_t)XH_RAM_VALVE_MARGIN;
  if (c->ram_fit_on < 0) c->ram_fit_on = from_image.ram_fit_on < 0 ? 1 : from_image.ram_fit_on;
  if (c->ram_huge < 0) c->ram_huge = from_image.ram_huge < 0 ? 1 : from_image.ram_huge;
  if (c->ram_gpu_reserve < 0) c->ram_gpu_reserve = from_image.ram_gpu_reserve < 0 ? (int64_t)XH_RAM_GPU_RESERVE : from_image.ram_gpu_reserve;
  if (c->ram_pagetable_percent < 0) c->ram_pagetable_percent = from_image.ram_pagetable_percent < 0 ? XH_RAM_PAGETABLE_PERCENT : from_image.ram_pagetable_percent;
  // What the host can hold for the guest without swapping it (2026-11-14, fix-ram-fit): MemAvailable now, after the fork
  // stopped One UI's zygote and before the guest takes anything (oneos/interface.md 4), less what the session needs
  // outside the guest: the iOS tasks' page tables (a share of the guest's RAM), KGSL's own memory, and a headroom.
  // With the RAM locked the host can neither swap nor reclaim it, so this is the bound; the page cache MemAvailable
  // counts is taken (the host reclaims cache, never the locked RAM).
  uint64_t total = 0, available = 0;
  bool known = xh_meminfo(c->ram_meminfo, &total, &available) == 0;
  c->ram_fit = known ? xh_ram_fit(available, (uint64_t)c->ram_session_reserve, (uint64_t)c->ram_gpu_reserve, (unsigned)c->ram_pagetable_percent) : 0;
  c->ram_fit_available = known ? available : 0;
  c->ram_lock_fit = c->ram_lock_expected ? c->ram_fit & ~((256ull << 20) - 1) : 0;
  if (!c->ram_bytes) {
    if (!known) {
      // Not Linux, or no /proc: the size of the time before ram=auto.
      fprintf(stderr, "xnuhost: ram=auto: no MemTotal in %s: the guest gets 4 GiB\n", c->ram_meminfo);
      c->ram_bytes = 4ull << 30;
    } else {
      c->ram_bytes = xh_ram_fit_auto(total, (unsigned)c->ram_percent, (uint64_t)c->ram_host_floor, c->ram_fit_on ? c->ram_fit : UINT64_MAX,
                                     &c->ram_auto_bound);
      if (!c->ram_bytes) {
        fprintf(stderr, "xnuhost: ram=auto: this host has %llu MiB, %llu MiB of it available; %d%% of it, all but %llu MiB, or what fits "
                "beside the session's needs (%llu MiB), is less than the guest's 1 GiB: give --ram SIZE\n", (unsigned long long)(total >> 20),
                (unsigned long long)(available >> 20), c->ram_percent, (unsigned long long)((uint64_t)c->ram_host_floor >> 20),
                (unsigned long long)(c->ram_fit >> 20));
        return -1;
      }
      c->ram_auto_total = total;
      c->ram_auto_available = available;
    }
  }
  if (c->ram_bytes & 0x1fffff || c->ram_bytes < (256ull << 20) || c->ram_bytes > XH_RAM_MAX) {
    fprintf(stderr, "xnuhost: --ram must be a multiple of 2 MiB, at least 256 MiB, at most %llu GiB\n", XH_RAM_MAX >> 30);
    return -1;
  }
  return 0;
}

uint64_t xh_ram_auto(uint64_t total, unsigned percent, uint64_t floor)
{
  if (percent > 100) percent = 100;
  uint64_t share = total / 100 * percent + total % 100 * percent / 100, left = total > floor ? total - floor : 0;
  uint64_t ram = (share < left ? share : left) & ~((256ull << 20) - 1);
  if (ram > XH_RAM_MAX) ram = XH_RAM_MAX;
  return ram >= (1ull << 30) ? ram : 0;
}

uint64_t xh_ram_fit(uint64_t available, uint64_t reserve, uint64_t gpu, unsigned pagetable_percent)
{
  // G + G * pt / 100 + gpu + reserve <= available
  if (available <= reserve + gpu) return 0;
  return (available - reserve - gpu) * 100 / (100 + pagetable_percent);        // MemAvailable < 2^57: no overflow
}

uint64_t xh_ram_fit_auto(uint64_t total, unsigned percent, uint64_t floor, uint64_t fit, unsigned *bound)
{
  uint64_t ram = xh_ram_auto(total, percent, floor);
  unsigned why = 0;
  if (ram) {
    unsigned p = percent > 100 ? 100 : percent;
    uint64_t share = total / 100 * p + total % 100 * p / 100, left = total > floor ? total - floor : 0;
    why = share <= left ? XH_RAM_BOUND_SHARE : XH_RAM_BOUND_FLOOR;
  }
  if (ram && fit != UINT64_MAX) {
    // No more than the host can hold beside what the session needs outside the guest (xh_ram_fit): nothing to swap.
    uint64_t fits = fit & ~((256ull << 20) - 1);
    if (fits < ram) { ram = fits; why = XH_RAM_BOUND_AVAILABLE; }
    if (ram < (1ull << 30)) ram = 0;
  }
  if (bound) *bound = ram ? why : 0;
  return ram;
}

bool xh_swap_note(uint64_t swapped, uint64_t *next)
{
  if (!*next) *next = XH_SWAP_NOTE_FIRST;
  if (swapped <= *next) return false;
  // Said once over 64 MiB, then again at each further 256 MiB: the next level is the first one at or above it.
  *next = XH_SWAP_NOTE_FIRST + (swapped - XH_SWAP_NOTE_FIRST + XH_SWAP_NOTE_STEP - 1) / XH_SWAP_NOTE_STEP * XH_SWAP_NOTE_STEP;
  return true;
}

int xh_meminfo(const char *path, uint64_t *total, uint64_t *available)
{
  FILE *f = fopen(path, "r");
  if (!f) return -1;
  char line[128];
  unsigned long long kb;
  int found = 0;
  while (fgets(line, sizeof line, f) && found != 3) {
    if (sscanf(line, "MemTotal: %llu kB", &kb) == 1) { *total = (uint64_t)kb << 10; found |= 1; }
    else if (sscanf(line, "MemAvailable: %llu kB", &kb) == 1) { *available = (uint64_t)kb << 10; found |= 2; }
  }
  fclose(f);
  return found == 3 && *total ? 0 : -1;
}

int xh_meminfo_value(const char *path, const char *key, uint64_t *bytes)
{
  FILE *f = fopen(path, "r");
  if (!f) return -1;
  char line[128];
  size_t n = strlen(key);
  int r = -1;
  while (fgets(line, sizeof line, f)) {
    unsigned long long kb;
    if (!strncmp(line, key, n) && line[n] == ':' && sscanf(line + n + 1, " %llu kB", &kb) == 1) { *bytes = (uint64_t)kb << 10; r = 0; break; }
  }
  fclose(f);
  return r;
}

unsigned xh_proc_values(const char *path, const char *const *keys, unsigned n, uint64_t *out)
{
  FILE *f = fopen(path, "r");
  if (!f) return 0;
  char line[160];
  unsigned found = 0;
  uint64_t seen = 0;                        // which keys were found (n <= 64)
  while (found < n && fgets(line, sizeof line, f))
    for (unsigned k = 0; k < n && k < 64; k++) {
      size_t len = strlen(keys[k]);
      if (seen >> k & 1 || strncmp(line, keys[k], len) || (line[len] != ':' && line[len] != ' ')) continue;
      char *end;
      unsigned long long v = strtoull(line + len + 1, &end, 10);
      while (*end == ' ') end++;
      out[k] = !strncmp(end, "kB", 2) ? (uint64_t)v << 10 : (uint64_t)v;
      seen |= 1ull << k;
      found++;
      break;
    }
  fclose(f);
  return found;
}

// ---- the display's mode ---------------------------------------------------------------------------
bool xh_panel_native(const xh_panel_mode *modes, unsigned count, xh_panel_mode *native)
{
  bool found = false;
  for (unsigned i = 0; i < count; i++) {
    const xh_panel_mode *m = &modes[i];
    if (!m->width || !m->height || m->width > 16384 || m->height > 16384) continue;
    uint64_t area = (uint64_t)m->width * m->height, best = found ? (uint64_t)native->width * native->height : 0;
    if (!found || area > best || (area == best && m->width == native->width && m->hz > native->hz)) { *native = *m; found = true; }
  }
  return found;
}

bool xh_panel_connector(unsigned type)
{
  // DRM_MODE_CONNECTOR_LVDS, eDP, DSI, DPI. A phone's Virtual connectors (the writeback ones) report "connected"
  // with modes up to 5120x2560, and DisplayPort is whatever monitor is plugged in.
  return type == 7 || type == 14 || type == 16 || type == 17;
}

bool xh_panel_report(const void *message, size_t length, xh_panel_mode *panel)
{
  struct { char magic[4]; uint32_t width, height, hz, unused; } m;
  if (length != sizeof m) return false;
  memcpy(&m, message, sizeof m);
  if (memcmp(m.magic, "XHDP", 4) || !m.width || !m.height || m.width > 16384 || m.height > 16384) return false;
  *panel = (xh_panel_mode){ m.width, m.height, m.hz };
  return true;
}

xh_panel_mode xh_panel_fallback(const xh_config *c)
{
  xh_panel_mode mode = { XH_PANEL_WIDTH, XH_PANEL_HEIGHT, XH_PANEL_HZ };
  if (c->fallback_width && c->fallback_height) mode = (xh_panel_mode){ c->fallback_width, c->fallback_height, c->fallback_hz ? c->fallback_hz : XH_PANEL_HZ };
  return mode;
}

xh_panel_mode xh_display_mode(const xh_config *c, const xh_panel_mode *panel)
{
  xh_panel_mode mode = xh_panel_fallback(c);
  if (panel && panel->width && panel->height) { mode = *panel; if (mode.hz < 24 || mode.hz > 240) mode.hz = xh_panel_fallback(c).hz; }
  if (c->width) mode.width = c->width;
  if (c->height) mode.height = c->height;
  if (c->hz) mode.hz = c->hz;
  return mode;
}

static bool has_switch(const char *args, const char *name)
{
  size_t n = strlen(name);
  for (const char *p = args; (p = strstr(p, name)); p += n)
    if ((p == args || p[-1] == ' ') && p[n] == '=') return true;
  return false;
}

bool xh_boot_args_hides_vmm_from(const char *args, const char *name)
{
  static const char key[] = "owner-hide-vmm=";
  size_t n = strlen(name);
  for (const char *p = args; (p = strstr(p, key)); p++) {
    if (p != args && p[-1] != ' ') continue;
    for (const char *e = p + sizeof key - 1; *e && *e != ' ';) {
      size_t len = strcspn(e, ", ");
      if (len == n && !strncmp(e, name, n)) return true;
      e += len + (e[len] == ',');
    }
  }
  return false;
}

int xh_boot_args_hide_vmm_add(char *args, size_t size, const char *name)
{
  static const char key[] = "owner-hide-vmm=";
  size_t used = strlen(args), n = strlen(name);
  const char *word = NULL;
  for (const char *p = args; !word && (p = strstr(p, key)); p++)
    if (p == args || p[-1] == ' ') word = p;           // the kernel's PE_parse_boot_arg_str takes the first: so do we
  if (word) {
    char first[256];
    size_t length = strcspn(word, " ");
    if (length >= sizeof first) return -1;
    memcpy(first, word, length);
    first[length] = 0;
    if (xh_boot_args_hides_vmm_from(first, name)) return 0;
  } else {
    if (used + (used ? 1 : 0) + sizeof key - 1 + n + 1 > size || n > 127) return -1;
    snprintf(args + used, size - used, "%s%s%s", used ? " " : "", key, name);
    return 1;
  }
  char *end = (char *)word + sizeof key - 1;
  size_t list = strcspn(end, " ");
  if (used + 1 + n + 1 > size || list + 1 + n > 127) return -1;
  memmove(end + list + 1 + n, end + list, used - (size_t)(end + list - args) + 1);
  end[list] = ',';
  memcpy(end + list + 1, name, n);
  if (!list) memmove(end, end + 1, strlen(end + 1) + 1);   // "owner-hide-vmm=" with an empty list: no leading comma
  return 1;
}

// bluetoothd's own reading of its boot argument (24A446 /usr/sbin/bluetoothd at 0x100309098): every space-separated
// word that contains "bt-transport-override" sets the transport to strtol of what follows its '=', when under 13; the
// last such word wins.
int xh_boot_args_bt_transport(const char *args)
{
  static const char key[] = "bt-transport-override";
  int found = -1;
  for (const char *p = args; *p;) {
    size_t len = strcspn(p, " ");
    bool k = false;
    for (size_t i = 0; !k && i + sizeof key - 1 <= len; i++) k = !strncmp(p + i, key, sizeof key - 1);
    const char *eq = memchr(p, '=', len);
    if (k && eq) {
      long v = strtol(eq + 1, NULL, 10);
      if (v >= 0 && v < 13) found = (int)v;
    }
    p += len + (p[len] == ' ');
  }
  return found;
}

// The display's switches the guest needs, unless the command line has them: its UI scale, the panel's rate, and
// with a half-float frame (display-format=rgba16f) oneios-display-wide=1, which tells the guest's display to hand
// half-float surfaces to the scanout as they are (XH_VIRTIO_GPU_FORMAT_RGBA16F) instead of narrowing them to 8 bits.
int xh_display_boot_args(char *args, size_t size, unsigned scale, unsigned hz, unsigned format)
{
  char add[80];
  if (format == XH_PIXELS_RGBA16F && !has_switch(args, "oneios-display-wide")) {
    snprintf(add, sizeof add, "%soneios-display-wide=1", *args ? " " : "");
    if (strlen(args) + strlen(add) >= size) return -1;
    strcat(args, add);
  }
  if (!has_switch(args, "offline-legacy-unit-scale")) {
    snprintf(add, sizeof add, "%soffline-legacy-unit-scale=%u", *args ? " " : "", scale);
    if (strlen(args) + strlen(add) >= size) return -1;
    strcat(args, add);
  }
  if (!has_switch(args, "sheng-vsync")) {
    snprintf(add, sizeof add, "%ssheng-vsync=%u", *args ? " " : "", hz);
    if (strlen(args) + strlen(add) >= size) return -1;
    strcat(args, add);
  }
  return 0;
}

// ram-view= (docs/booter.md 2.2, "The memory view"): the compressor's room for a view of `view` bytes over `window`
// bytes of RAM, unless the command line sets it. XNU caps what the compressor holds at its pool's pages times
// kern.compressor_pool_multiplier, and at least max_mem's pages (vm_compressor_set_size; 1 on an iOS of more than 2 GiB):
// so at most the RAM's own size of original pages. The multiplier is the view over the RAM, rounded up (2 for 12 GiB
// over 7 GiB). Returns the multiplier (0: no view, nothing added), or -1 with no room in the arguments.
int xh_view_boot_args(char *args, size_t size, uint64_t view, uint64_t window)
{
  if (!view || !window || view <= window) return 0;
  unsigned multiplier = (unsigned)((view + window - 1) / window);
  if (has_switch(args, "kern.compressor_pool_multiplier")) return (int)multiplier;
  char add[64];
  snprintf(add, sizeof add, "%skern.compressor_pool_multiplier=%u", *args ? " " : "", multiplier);
  if (strlen(args) + strlen(add) >= size) return -1;
  strcat(args, add);
  return (int)multiplier;
}

// -[CADisplayMode preferredScale] of QuartzCore, read in 24A446's cache (docs/userland.md, "The UI scale"): a
// mode with no preferred scale of its own is @2x in a virtual device (or what the mode's own field says, when
// more), else @2x when taller than 1080 pixels and @1x otherwise. Nothing there answers 3, so for a scale of 3
// both answers are changed: the virtual device's floor, and the height test's result. A scale of 2 is what the
// code gives unchanged. The kernel applies a fixup only where the original bytes match.
unsigned xh_scale_fixups(const char *build, unsigned scale, char *list, unsigned length, unsigned size)
{
  static const char *const fixups_24A446_3x[] = {
    "dyld_shared_cache_arm64e.01,40b17a8,49008052,69008052",                    // mov w9, #2 -> mov w9, #3
    "dyld_shared_cache_arm64e.01,40b17bc,280080520895889a,680080521f2003d5",    // mov w8, #1; cinc x8, x8, hi -> mov w8, #3; nop
  };
  if (scale != 3 || !build || strcmp(build, "24A446")) return length;
  for (unsigned i = 0; i < sizeof fixups_24A446_3x / sizeof *fixups_24A446_3x; i++) {
    unsigned n = (unsigned)strlen(fixups_24A446_3x[i]) + 1;
    if (length + n > size) break;
    memcpy(list + length, fixups_24A446_3x[i], n);
    length += n;
  }
  return length;
}

// --host-info (main.c): the host table's row for this phone, one line on `out`, for the OneOS switch's preflight
// (oneos/interface.md 4a): "host-info: device=galaxy-s24-ultra takeover=yes (matched by ro.product.model=SM-S928B)", or
// "host-info: device=none takeover=no (unknown phone; read: ...)". It reads what xh_config_finish's host layer reads (the
// properties, three sysfs and proc files; host-device=, host-props=, host-root= from the command line or a --config
// file) and needs no image. -1 for a host-device= that names no row.
int xh_config_host_info(const xh_config *c, FILE *out)
{
  br_host_facts f;
  memset(&f, 0, sizeof f);
  const br_host_device *e = NULL;
  char why[260], read[400];
  if (br_host_choose(*c->host_device ? c->host_device : "auto", c->host_props, c->host_root, &f, &e, why, sizeof why)) {
    fprintf(stderr, "xnuhost: %s\n", why);
    return -1;
  }
  if (e) fprintf(out, "host-info: device=%s takeover=%s (%s)\n", e->name, e->takeover ? "yes" : "no", why);
  else {
    br_host_describe(&f, read, sizeof read);
    fprintf(out, "host-info: device=none takeover=no (%s; read: %s)\n", why, read);
  }
  return 0;
}
