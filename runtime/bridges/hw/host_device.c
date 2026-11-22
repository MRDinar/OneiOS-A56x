// SPDX-License-Identifier: MIT
// The host phone and its defaults (bridges/host_device.h): the table of known phones, the detection, the log lines.
// Read only: a few properties and three small sysfs/proc files, once.
#define _GNU_SOURCE
#include "bridges/host_device.h"
#include <ctype.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <strings.h>
#include <unistd.h>

#if defined(__ANDROID__)
#include <sys/system_properties.h>
#endif

// ---- the table ------------------------------------------------------------------------------------------------------
// The one place that says what differs per host phone. Rules (docs/hardware.md, "The host device"):
//   - an entry holds only settings that already exist as options: an xnuhost setting that config.c merges as a device
//     default (xh_config_finish: gpu, panel-fallback, hw-backend*), or an environment variable a bridge reads;
//   - the home device (the S24 Ultra) has none: its behaviour is today's, byte for byte;
//   - .takeover = true allows session-mode=takeover there (the opt-in Android takeover, docs/booter.md 2.9d): only on a
//     phone where that mode ran (the S26 Ultra, YMGPwcca's experiment) or that is the owner's (the S24 Ultra);
//   - a value the docs mark unverified on that phone is not set (today's default is the conservative one) and is said
//     here. Marks as in docs/s25.md: [F] read from the source named, [I] inferred, [U] unknown until a phone shows it.
// Match strings: the model prefixes and codenames are the phone gates' (scripts/port/verify-host.sh, docs/a56-oneos-fork.md);
// a SoC or platform match alone means "this SoC's entry" (the S24 and S24+ on SM8650 get the S24 Ultra's: no change).
static const br_host_device table[] = {
  // The home device. [F] SM-S928*, e3q, SM8650 (verify-host.sh, the OneOS fork's gate); platform pineapple [F] (the
  // stock device tree, qcom/pineapple-v2.dts, docs/hardware.md). No defaults: everything is today's.
  // The model prefix takes every S24 Ultra model number (docs/hardware.md 7h, "The S24 Ultra's models"; the record
  // profiles/host-galaxy-s24-ultra-sm8650.toml): SM-S928B (the owner's phone [F]), SM-S928U and SM-S928U1 (US),
  // SM-S928W (Canada, CSC XAC), SM-S928N (Korea), SM-S9280 (China).
  // The SM-S928W runs the US carrier model's bootloader and vendor partition under its own firmware (S928USQU6EZIF,
  // ro.product.vendor.model=SM-S928U [F per the OneOS fork's public issue 128]): nothing here reads either, so it is
  // matched by its own ro.product.model. e3q and SM8650 on the U, U1, W, N and 0 [I] (one hardware family; only the B
  // has booted iOS). The Japanese ones (SC-52E, SCG26) are not SM-S928 and their codename is [U]: they
  // get this entry by their SoC (SM8650, the SoC's entry), which has no defaults either.
  // takeover: session-mode=takeover is allowed (the owner's phone, 2026-11-23: the opt-in Android takeover, ported from
  // YMGPwcca/OneOS's S26 experiment; docs/booter.md 2.9d). Not run on the phone yet.
  { .name = "galaxy-s24-ultra", .label = "Galaxy S24 Ultra (SM-S928, SM8650)",
    .models = { "SM-S928" }, .devices = { "e3q" }, .socs = { "SM8650" }, .platforms = { "pineapple" }, .takeover = true },
  // docs/s25.md "Galaxy S25 Ultra", profiles/host-galaxy-s25-ultra-sm8750.toml. [F] SM-S938, pa3q, SM8750, platform sun
  // (YumikoHayamato/OneiOS s25-ultra-sm8750: the fork's SM-S938B, One UI 9.0, sessions 1-7 on 2026-11-19).
  // ONEIOS_THERMAL_SOC_TRIPS=1, ONEIOS_THERMAL_SOC_SAMPLES=3 [F] (the same sessions): the kernel's first passive trip on
  // the SoC zones is 105 C and the hot one 125 C, so the S24's SoC rule (HEAVY 95 C, TRAPPING 105 C, one reading) put
  // iOS in TRAPPING (its "too hot" screen) at every throttle: 20 level changes in 8 minutes, 5 into TRAPPING, on single
  // junction spikes (68 -> 104 -> 68 C between polls). With the trips (115/120 C) and the lowest of 3 readings (15 s)
  // the SoC rule stayed under them in sessions 3-7; the levels came from the case, as on the S24. The SoC rule takes no
  // step (only the case and the cell do): this changes what iOS is told, not what xnuhost runs.
  // Not set: the backlight (panel0-backlight max 12750 [F], One UI's manual top not measured on this phone: the S26's
  // 2300 is not assumed), the panel (1440x3120 command mode [F]: the display helper's report gives it).
  { .name = "galaxy-s25-ultra", .label = "Galaxy S25 Ultra (SM-S938, SM8750)",
    .models = { "SM-S938" }, .devices = { "pa3q" }, .socs = { "SM8750" }, .platforms = { "sun" },
    .env = { "ONEIOS_THERMAL_SOC_TRIPS=1", "ONEIOS_THERMAL_SOC_SAMPLES=3" } },
  // docs/s25.md: [F] SM-S931B, pa1q, SM8750. panel-fallback=1080x2340@120 [F] (xaxaapple/s25ver's device profile and
  // xnuhost.h: a 1080x2340 panel): only the mode assumed where the panel cannot be read (the display helper's report
  // still wins); without it such a session drew 1440x3120, 1.78 times the pixels, scaled down at every flush. Its
  // Bluetooth HAL (HIDL or AIDL) and bridge node names are [U], as for the S25 Ultra. The SoC rule as the S25 Ultra's
  // [I] (same SM8750): the thresholds come from this phone's own trips, read at the bridge's start and logged
  // ("thermal: the SoC rule: ... the kernel's trips: ..."), never under 95/105 C.
  { .name = "galaxy-s25", .label = "Galaxy S25 (SM-S931, SM8750)",
    .models = { "SM-S931" }, .devices = { "pa1q" }, .socs = { "SM8750" }, .platforms = { "sun" },
    .config = { "panel-fallback=1080x2340@120" }, .env = { "ONEIOS_THERMAL_SOC_TRIPS=1", "ONEIOS_THERMAL_SOC_SAMPLES=3" } },
  // docs/s26.md, profiles/host-galaxy-s26-ultra-sm8850.toml, the port-sm8850 commits. [F] SM-S948B, m3q, SM8850, platform
  // canoe (YMGPwcca/OneiOS: the fork's SM-S948B, S948BXXS4BZIG, measured 2026-11-16; port-sm8850-s26, 2026-11-23).
  // ONEIOS_BRIGHT_MAX=2300 [F]: the fork's phone with One UI in control, screen_brightness 255 (One UI's manual top)
  // wrote 2300 to panel0-backlight, whose max_brightness is 12750 (brightness/test_brightness.c,
  // test_sm8850_manual_brightness_cap). Without it the iOS slider's top writes 12750, 5.5 times One UI's manual top
  // (HBM territory: heat and current). 2300 is the conservative value, not a claim about the panel's maximum.
  // takeover: session-mode=takeover is allowed [F: the phone YMGPwcca's root-only Framework-takeover experiment ran on,
  // 2026-11-20..22, with the native touchscreen grabbed]. Not through this tree's switch yet. touch-exclusive itself is
  // not a default here: session-mode=takeover turns it on (config.c), session-mode=hosted keeps it off on every phone.
  // The native-touchscreen priority (xh_input_accept_touch) and the cpu-hw-trip-* exclusion (thermal.c) already apply
  // on every phone.
  { .name = "galaxy-s26-ultra", .label = "Galaxy S26 Ultra (SM-S948, SM8850)",
    .models = { "SM-S948" }, .devices = { "m3q" }, .socs = { "SM8850" }, .platforms = { "canoe" },
    .env = { "ONEIOS_BRIGHT_MAX=2300" }, .takeover = true },
  // docs/a56.md, profiles/ios-*-a56.toml, docs/a56-oneos-fork.md. [F] SM-A566E, a56x, s5e8855 (the fork's gate:
  // ro.soc.model); platform s5e8855 [I]. gpu=2d [F]: no KGSL (Xclipse 540 on amdgpu, /dev/dri/renderD128) and xnuhost's
  // 3D half is drm2kgsl only, so gpu=auto would only fail to open KGSL and keep an 8 MiB GPU pool nothing uses; the
  // fork's A56 session passes --gpu 2d. panel-fallback=1080x2340@120 [F] (the fork's docs/a56-guide.md), as for the S25. Touch, haptics, battery and thermal node names on the A56 are [U]: samsung stays.
  { .name = "galaxy-a56", .label = "Galaxy A56 (SM-A566, Exynos 1580)",
    .models = { "SM-A566" }, .devices = { "a56x" }, .socs = { "s5e8855" }, .platforms = { "s5e8855" },
    .config = { "gpu=2d", "panel-fallback=1080x2340@120" } },
};

const br_host_device *br_host_table(unsigned *count)
{
  if (count) *count = sizeof table / sizeof *table;
  return table;
}

const br_host_device *br_host_find(const char *name)
{
  for (unsigned i = 0; name && i < sizeof table / sizeof *table; i++)
    if (!strcasecmp(name, table[i].name)) return &table[i];
  return NULL;
}

void br_host_names(char *out, size_t size)
{
  size_t used = 0;
  if (!size) return;
  out[0] = 0;
  for (unsigned i = 0; i < sizeof table / sizeof *table && used < size; i++)
    used += (size_t)snprintf(out + used, size - used, "%s%s", i ? ", " : "", table[i].name);
}

// ---- reading --------------------------------------------------------------------------------------------------------
static const char *const prop_names[] = { "ro.product.model", "ro.product.device", "ro.board.platform", "ro.soc.model", "ro.soc.manufacturer" };
enum { P_MODEL, P_DEVICE, P_PLATFORM, P_SOC, P_MANUFACTURER, P_COUNT };

static char *prop_slot(br_host_facts *f, unsigned i)
{
  switch (i) {
  case P_MODEL: return f->model;
  case P_DEVICE: return f->device;
  case P_PLATFORM: return f->platform;
  case P_SOC: return f->soc_model;
  default: return f->soc_manufacturer;
  }
}

static void trim_copy(char *to, size_t size, const char *from, size_t n)
{
  while (n && isspace((unsigned char)*from)) { from++; n--; }
  while (n && isspace((unsigned char)from[n - 1])) n--;
  if (n >= size) n = size - 1;
  memcpy(to, from, n);
  to[n] = 0;
}

// A getprop dump: "[ro.product.model]: [SM-S928B]" (adb shell getprop) or "ro.product.model=SM-S928B" (build.prop).
static int props_from_file(br_host_facts *f, const char *path)
{
  FILE *in = fopen(path, "r");
  if (!in) return -1;
  char line[512];
  while (fgets(line, sizeof line, in)) {
    const char *key = line, *value;
    size_t klen, vlen;
    if (line[0] == '[') {
      const char *end = strchr(line, ']');
      if (!end) continue;
      key = line + 1; klen = (size_t)(end - key);
      const char *open = strchr(end + 1, '[');
      const char *close = open ? strrchr(open, ']') : NULL;
      if (!open || !close) continue;
      value = open + 1; vlen = (size_t)(close - value);
    } else {
      const char *eq = strchr(line, '=');
      if (!eq || line[0] == '#') continue;
      klen = (size_t)(eq - line); value = eq + 1; vlen = strcspn(value, "\r\n");
    }
    for (unsigned i = 0; i < P_COUNT; i++)
      if (strlen(prop_names[i]) == klen && !strncmp(key, prop_names[i], klen)) trim_copy(prop_slot(f, i), BR_HOST_VALUE, value, vlen);
  }
  fclose(in);
  f->props = true;
  return 0;
}

#if defined(__ANDROID__)
struct read_into { char *out; size_t size; };
static void got_value(void *cookie, const char *name, const char *value, uint32_t serial)
{
  (void)name; (void)serial;
  struct read_into *r = cookie;
  snprintf(r->out, r->size, "%s", value);
}
// Bionic's property area, as usb.c reads it (a property that is not there stays "").
static void props_from_bionic(br_host_facts *f)
{
  f->props = true;
  for (unsigned i = 0; i < P_COUNT; i++) {
    const prop_info *pi = __system_property_find(prop_names[i]);
    if (!pi) continue;
    struct read_into r = { prop_slot(f, i), BR_HOST_VALUE };
    __system_property_read_callback(pi, got_value, &r);
  }
}
#endif

// A small file under root: its bytes, NULs (a device tree's string list) as ',', the line break dropped.
static void read_small(const char *root, const char *rel, char *out, size_t size, bool list)
{
  char path[512];
  out[0] = 0;
  if (snprintf(path, sizeof path, "%s%s", root, rel) >= (int)sizeof path) return;
  int fd = open(path, O_RDONLY | O_CLOEXEC);
  if (fd < 0) return;
  ssize_t n = read(fd, out, size - 1);
  close(fd);
  if (n <= 0) { out[0] = 0; return; }
  while (n && (out[n - 1] == 0 || out[n - 1] == '\n' || out[n - 1] == '\r')) n--;
  out[n] = 0;
  if (list) for (ssize_t i = 0; i < n; i++) if (!out[i]) out[i] = ',';
  out[strcspn(out, "\r\n")] = 0;
}

int br_host_read(br_host_facts *f, const char *props_file, const char *root)
{
  memset(f, 0, sizeof *f);
  if (!root) root = "";
  if (props_file && *props_file) { if (props_from_file(f, props_file)) return -1; }
#if defined(__ANDROID__)
  else props_from_bionic(f);
#endif
  read_small(root, "/sys/devices/soc0/machine", f->machine, sizeof f->machine, false);
  read_small(root, "/sys/devices/soc0/soc_id", f->soc_id, sizeof f->soc_id, false);
  read_small(root, "/proc/device-tree/compatible", f->compatible, sizeof f->compatible, true);
  return 0;
}

// ---- matching -------------------------------------------------------------------------------------------------------
// A compatible entry "vendor,NAME" whose NAME is the SoC or the platform (any case) [I]: the Linux convention
// ("qcom,sm8650", "qcom,pineapple"); a board's own entries ("qcom,pineapple-mtp") do not match.
static bool compatible_has(const char *list, const char *name)
{
  size_t n = strlen(name);
  for (const char *p = list; *p;) {
    size_t len = strcspn(p, ",");                 // vendor
    const char *v = p + len;
    if (!*v) break;
    v++;
    size_t vlen = strcspn(v, ",");
    if (vlen == n && !strncasecmp(v, name, n)) return true;
    p = v + vlen + (v[vlen] == ',');
  }
  return false;
}

const br_host_device *br_host_match(const br_host_facts *f, char *why, size_t size)
{
  const unsigned count = sizeof table / sizeof *table;
  const br_host_device *found = NULL;
  char text[200] = "";
  for (unsigned i = 0; !found && *f->model && i < count; i++)
    for (unsigned k = 0; k < BR_HOST_MATCH && table[i].models[k]; k++)
      if (!strncmp(f->model, table[i].models[k], strlen(table[i].models[k]))) { found = &table[i]; snprintf(text, sizeof text, "ro.product.model=%s", f->model); break; }
  for (unsigned i = 0; !found && *f->device && i < count; i++)
    for (unsigned k = 0; k < BR_HOST_MATCH && table[i].devices[k]; k++)
      if (!strcasecmp(f->device, table[i].devices[k])) { found = &table[i]; snprintf(text, sizeof text, "ro.product.device=%s", f->device); break; }
  for (unsigned i = 0; !found && (*f->soc_model || *f->machine) && i < count; i++)
    for (unsigned k = 0; k < BR_HOST_MATCH && table[i].socs[k]; k++) {
      if (*f->soc_model && !strcasecmp(f->soc_model, table[i].socs[k])) { found = &table[i]; snprintf(text, sizeof text, "ro.soc.model=%s (the SoC's entry)", f->soc_model); break; }
      if (*f->machine && !strcasecmp(f->machine, table[i].socs[k])) { found = &table[i]; snprintf(text, sizeof text, "soc0.machine=%s (the SoC's entry)", f->machine); break; }
    }
  for (unsigned i = 0; !found && *f->platform && i < count; i++)
    for (unsigned k = 0; k < BR_HOST_MATCH && table[i].platforms[k]; k++)
      if (!strcasecmp(f->platform, table[i].platforms[k])) { found = &table[i]; snprintf(text, sizeof text, "ro.board.platform=%s (the SoC's entry)", f->platform); break; }
  for (unsigned i = 0; !found && *f->compatible && i < count; i++) {
    for (unsigned k = 0; !found && k < BR_HOST_MATCH && table[i].socs[k]; k++)
      if (compatible_has(f->compatible, table[i].socs[k])) found = &table[i];
    for (unsigned k = 0; !found && k < BR_HOST_MATCH && table[i].platforms[k]; k++)
      if (compatible_has(f->compatible, table[i].platforms[k])) found = &table[i];
    if (found) snprintf(text, sizeof text, "the device tree's compatible %.120s (the SoC's entry)", f->compatible);
  }
  if (why && size) snprintf(why, size, "%s", text);
  return found;
}

void br_host_describe(const br_host_facts *f, char *out, size_t size)
{
  if (!size) return;
  size_t used = 0;
  out[0] = 0;
  if (!f->props) used += (size_t)snprintf(out, size, "no property area");
  else
    for (unsigned i = 0; i < P_COUNT && used < size; i++) {
      const char *v = prop_slot((br_host_facts *)f, i);
      used += (size_t)snprintf(out + used, size - used, "%s%s=%s", used ? " " : "", prop_names[i], *v ? v : "(none)");
    }
  if (*f->machine && used < size) used += (size_t)snprintf(out + used, size - used, " soc0.machine=%s", f->machine);
  if (*f->soc_id && used < size) used += (size_t)snprintf(out + used, size - used, " soc0.soc_id=%s", f->soc_id);
  if (*f->compatible && used < size) snprintf(out + used, size - used, " dt=%.120s", f->compatible);
}

int br_host_choose(const char *setting, const char *props_file, const char *root, br_host_facts *f,
                   const br_host_device **entry, char *why, size_t size)
{
  *entry = NULL;
  memset(f, 0, sizeof *f);
  if (setting && !strcasecmp(setting, "none")) { if (why && size) snprintf(why, size, "host-device=none: detection off"); return 0; }
  bool forced = setting && *setting && strcasecmp(setting, "auto");
  const br_host_device *named = forced ? br_host_find(setting) : NULL;
  if (forced && !named) { if (why && size) snprintf(why, size, "host-device=%s is not a known phone", setting); return -1; }
  if (br_host_read(f, props_file, root)) { if (why && size) snprintf(why, size, "cannot read the property file %s", props_file); return -1; }
  if (named) { *entry = named; if (why && size) snprintf(why, size, "host-device=%s (forced)", named->name); return 0; }
  char by[220];
  *entry = br_host_match(f, by, sizeof by);
  if (why && size) {
    if (*entry) snprintf(why, size, "matched by %s", by);
    else snprintf(why, size, "unknown phone");
  }
  return 0;
}

// ---- the environment's defaults -------------------------------------------------------------------------------------
static bool split(const char *pair, char *name, size_t size, const char **value)
{
  const char *eq = strchr(pair, '=');
  if (!eq || (size_t)(eq - pair) >= size) return false;
  memcpy(name, pair, (size_t)(eq - pair));
  name[eq - pair] = 0;
  *value = eq + 1;
  return true;
}

void br_host_env_describe(const br_host_device *entry, char *out, size_t size)
{
  if (!size) return;
  size_t used = 0;
  out[0] = 0;
  for (unsigned i = 0; entry && i < BR_HOST_SETTINGS && entry->env[i] && used < size; i++) {
    char name[64];
    const char *value, *now;
    if (!split(entry->env[i], name, sizeof name, &value)) continue;
    if ((now = getenv(name)) && *now) used += (size_t)snprintf(out + used, size - used, "%s%s not taken (the environment's %.40s)", used ? ", " : "", entry->env[i], now);
    else used += (size_t)snprintf(out + used, size - used, "%s%s", used ? ", " : "", entry->env[i]);
  }
}

void br_host_env_apply(const br_host_device *entry)
{
  for (unsigned i = 0; entry && i < BR_HOST_SETTINGS && entry->env[i]; i++) {
    char name[64];
    const char *value, *now;
    if (!split(entry->env[i], name, sizeof name, &value)) continue;
    if (!(now = getenv(name)) || !*now) setenv(name, value, 1);
  }
}

// ---- the ctl tools --------------------------------------------------------------------------------------------------
int br_host_tool(br_hw_settings *hw, const br_host_device **entry, char *line, size_t size)
{
  const br_host_device *e = NULL;
  br_host_facts f;
  char why[260], read[400], env[200], hwtext[200] = "";
  if (entry) *entry = NULL;
  if (br_host_choose(getenv("ONEIOS_HOST_DEVICE"), getenv("ONEIOS_HOST_PROPS"), getenv("ONEIOS_HOST_ROOT"), &f, &e, why, sizeof why)) {
    char names[200];
    br_host_names(names, sizeof names);
    snprintf(line, size, "host: %s (ONEIOS_HOST_DEVICE: auto, none or one of %s)", why, names);
    return -1;
  }
  if (entry) *entry = e;
  if (!e) {
    if (!strncmp(why, "host-device=none", 16)) snprintf(line, size, "host: detection off (ONEIOS_HOST_DEVICE=none): today's defaults");
    else { br_host_describe(&f, read, sizeof read); snprintf(line, size, "host: unknown phone (%s): today's defaults", read); }
    return 0;
  }
  br_host_env_describe(e, env, sizeof env);
  br_host_env_apply(e);
  // Its hw-backend-* defaults, under the environment's.
  br_hw_settings dev;
  br_hw_settings_init(&dev);
  size_t used = 0;
  for (unsigned i = 0; i < BR_HOST_SETTINGS && e->config[i]; i++) {
    char key[64];
    const char *value;
    if (strncmp(e->config[i], "hw-backend", 10) || !split(e->config[i], key, sizeof key, &value) || br_hw_setting(&dev, key, value)) continue;
    used += (size_t)snprintf(hwtext + used, sizeof hwtext - used, "%s%s", used ? ", " : "", e->config[i]);
  }
  if (hw) br_hw_settings_merge(hw, &dev);
  br_host_describe(&f, read, sizeof read);
  snprintf(line, size, "host: %s, %s; read: %s; defaults: %s%s%s", e->name, why, read, *env || *hwtext ? env : "none (today's)",
           *env && *hwtext ? ", " : "", hwtext);
  return 0;
}
