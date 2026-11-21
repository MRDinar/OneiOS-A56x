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
//     default (xh_config_finish: gpu, hw-backend*), or an environment variable a bridge reads;
//   - the home device (the S24 Ultra) has none: its behaviour is today's, byte for byte;
//   - a value the docs mark unverified on that phone is not set (today's default is the conservative one) and is said
//     here. Marks as in docs/s25.md: [F] read from the source named, [I] inferred, [U] unknown until a phone shows it.
// Match strings: the model prefixes and codenames are the phone gates' (scripts/port/verify-host.sh, docs/a56-oneos-fork.md);
// a SoC or platform match alone means "this SoC's entry" (the S24 and S24+ on SM8650 get the S24 Ultra's: no change).
static const br_host_device table[] = {
  // The home device. [F] SM-S928*, e3q, SM8650 (verify-host.sh, the OneOS fork's gate); platform pineapple [F] (the
  // stock device tree, qcom/pineapple-v2.dts, docs/hardware.md). No defaults: everything is today's.
  { .name = "galaxy-s24-ultra", .label = "Galaxy S24 Ultra (SM-S928, SM8650)",
    .models = { "SM-S928" }, .devices = { "e3q" }, .socs = { "SM8650" }, .platforms = { "pineapple" } },
  // docs/s25.md "Galaxy S25 Ultra", profiles/host-galaxy-s25-ultra-sm8750.toml. [F] SM-S938, pa3q, SM8750; platform sun [I]
  // (the profile: "verify on device via ro.board.platform"). No defaults: the Adreno 830 fence path is the default
  // everywhere (drm2kgsl 0007), the CPU plan's Oryon rule is automatic (topology.c), the panel's mode comes from the
  // display helper's report (1080x2340 is not forced: width=/height= would override that report); every bridge's
  // node names on One UI 8.5 are [U] (tools/probe/s25-probe.sh answers them), so the backends stay samsung.
  { .name = "galaxy-s25-ultra", .label = "Galaxy S25 Ultra (SM-S938, SM8750)",
    .models = { "SM-S938" }, .devices = { "pa3q" }, .socs = { "SM8750" }, .platforms = { "sun" } },
  // docs/s25.md: [F] SM-S931B, pa1q, SM8750. No defaults, as for the S25 Ultra (its Bluetooth HAL, HIDL or AIDL, is [U]).
  { .name = "galaxy-s25", .label = "Galaxy S25 (SM-S931, SM8750)",
    .models = { "SM-S931" }, .devices = { "pa1q" }, .socs = { "SM8750" }, .platforms = { "sun" } },
  // docs/forks.md (the S26 row), the port-sm8850 commits. [F] SM-S948B, SM8850; codename and platform [U] (not matched).
  // ONEIOS_BRIGHT_MAX=2300 [F]: the fork's phone with One UI in control, screen_brightness 255 (One UI's manual top)
  // wrote 2300 to panel0-backlight, whose max_brightness is 12750 (brightness/test_brightness.c,
  // test_sm8850_manual_brightness_cap). Without it the iOS slider's top writes 12750, 5.5 times One UI's manual top
  // (HBM territory: heat and current). 2300 is the conservative value, not a claim about the panel's maximum.
  // Not set: touch-exclusive (bench-only, config.c refuses it in a takeover); the native-touchscreen priority
  // (xh_input_accept_touch) and the cpu-hw-trip-* exclusion (thermal.c) already apply on every phone.
  { .name = "galaxy-s26-ultra", .label = "Galaxy S26 Ultra (SM-S948, SM8850)",
    .models = { "SM-S948" }, .socs = { "SM8850" },
    .env = { "ONEIOS_BRIGHT_MAX=2300" } },
  // docs/a56.md, profiles/ios-*-a56.toml, docs/a56-oneos-fork.md. [F] SM-A566E, a56x, s5e8855 (the fork's gate:
  // ro.soc.model); platform s5e8855 [I]. gpu=2d [F]: no KGSL (Xclipse 540 on amdgpu, /dev/dri/renderD128) and xnuhost's
  // 3D half is drm2kgsl only, so gpu=auto would only fail to open KGSL and keep an 8 MiB GPU pool nothing uses; the
  // fork's A56 session passes --gpu 2d. Touch, haptics, battery and thermal node names on the A56 are [U]: samsung stays.
  { .name = "galaxy-a56", .label = "Galaxy A56 (SM-A566, Exynos 1580)",
    .models = { "SM-A566" }, .devices = { "a56x" }, .socs = { "s5e8855" }, .platforms = { "s5e8855" },
    .config = { "gpu=2d" } },
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
