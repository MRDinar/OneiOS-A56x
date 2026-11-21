// SPDX-License-Identifier: MIT
// Host unit tests of the hardware backend layer (bridges/hw_backend.h): the settings, the merge of the command line
// over the image set's xnuhost.conf, the environment of the ctl tools, the choice and its log lines; and the host
// device table and its detection (bridges/host_device.h) on stand-in property dumps and sysfs folders.
#define _GNU_SOURCE
#include "bridges/bridges.h"
#include "bridges/hw_backend.h"
#include "bridges/host_device.h"
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <strings.h>
#include <sys/stat.h>
#include <unistd.h>

static int checks, failures;
#define CHECK(cond) do { checks++; if (!(cond)) { failures++; printf("FAIL %s:%d: %s\n", __FILE__, __LINE__, #cond); } } while (0)

static char last[256];
static int last_level = -1;
static void sink(int level, const char *line) { snprintf(last, sizeof last, "%s", line); last_level = level; }

static void put(const char *path, const char *text)
{
  FILE *f = fopen(path, "w");
  if (f) { fputs(text, f); fclose(f); }
}

// A stand-in phone's properties as `adb shell getprop` prints them, and its root (soc0, the device tree).
static void phone(const char *dir, const char *name, const char *model, const char *device, const char *platform, const char *soc,
                  const char *manufacturer, char *props, size_t size)
{
  snprintf(props, size, "%s/%s.props", dir, name);
  char text[600];
  snprintf(text, sizeof text, "[ro.build.version.release]: [16]\n[ro.product.model]: [%s]\n[ro.product.device]: [%s]\n"
           "[ro.board.platform]: [%s]\n[ro.soc.model]: [%s]\n[ro.soc.manufacturer]: [%s]\n[ro.product.name]: [x]\n",
           model, device, platform, soc, manufacturer);
  put(props, text);
}

static const char *detect(const char *props, const char *root, char *why, size_t size)
{
  br_host_facts f;
  const br_host_device *e = NULL;
  if (br_host_choose("auto", props, root, &f, &e, why, size)) return "error";
  return e ? e->name : "unknown";
}

static void test_host_device(void)
{
  char tmp[] = "/tmp/oneios-host-XXXXXX", props[300], why[300], empty[300], line[1024];
  CHECK(mkdtemp(tmp) != NULL);
  snprintf(empty, sizeof empty, "%s/empty-root", tmp);
  mkdir(empty, 0755);

  // The table: the home device first; names unique; every entry matches by a model; its xnuhost settings are only the
  // keys config.c merges as device defaults (gpu, hw-backend*: test_xnuhost checks each is taken), its environment
  // ones only variables a bridge reads. The S24 Ultra's entry holds nothing: its behaviour is today's.
  unsigned n = 0;
  const br_host_device *t = br_host_table(&n);
  CHECK(n == 5 && !strcmp(t[0].name, "galaxy-s24-ultra"));
  for (unsigned i = 0; i < n; i++) {
    CHECK(t[i].name && t[i].label && t[i].models[0] && br_host_find(t[i].name) == &t[i]);
    for (unsigned j = 0; j < i; j++) CHECK(strcasecmp(t[i].name, t[j].name) != 0);
    for (unsigned k = 0; k < BR_HOST_SETTINGS && t[i].config[k]; k++)
      CHECK(!strncmp(t[i].config[k], "gpu=", 4) || !strncmp(t[i].config[k], "hw-backend", 10));
    for (unsigned k = 0; k < BR_HOST_SETTINGS && t[i].env[k]; k++) CHECK(!strncmp(t[i].env[k], "ONEIOS_BRIGHT_MAX=", 18));
  }
  CHECK(!t[0].config[0] && !t[0].env[0]);
  CHECK(br_host_find("GALAXY-A56") == &t[4] && !br_host_find("pixel") && !br_host_find(NULL));
  br_host_names(line, sizeof line);
  CHECK(!strcmp(line, "galaxy-s24-ultra, galaxy-s25-ultra, galaxy-s25, galaxy-s26-ultra, galaxy-a56"));

  // Each known phone by its model (the phone gates' properties).
  phone(tmp, "s24u", "SM-S928B", "e3q", "pineapple", "SM8650", "QTI", props, sizeof props);
  CHECK(!strcmp(detect(props, empty, why, sizeof why), "galaxy-s24-ultra") && !strcmp(why, "matched by ro.product.model=SM-S928B"));
  phone(tmp, "s24u-us", "SM-S928U1", "e3q", "pineapple", "SM8650", "QTI", props, sizeof props);
  CHECK(!strcmp(detect(props, empty, why, sizeof why), "galaxy-s24-ultra"));
  phone(tmp, "s25u", "SM-S938B", "pa3q", "sun", "SM8750", "QTI", props, sizeof props);
  CHECK(!strcmp(detect(props, empty, why, sizeof why), "galaxy-s25-ultra") && strstr(why, "SM-S938B"));
  phone(tmp, "s25", "SM-S931B", "pa1q", "sun", "SM8750", "QTI", props, sizeof props);
  CHECK(!strcmp(detect(props, empty, why, sizeof why), "galaxy-s25") && strstr(why, "SM-S931B"));
  phone(tmp, "s26u", "SM-S948B", "", "", "SM8850", "QTI", props, sizeof props);
  CHECK(!strcmp(detect(props, empty, why, sizeof why), "galaxy-s26-ultra") && strstr(why, "SM-S948B"));
  phone(tmp, "a56", "SM-A566E", "a56x", "s5e8855", "s5e8855", "Samsung", props, sizeof props);
  CHECK(!strcmp(detect(props, empty, why, sizeof why), "galaxy-a56") && strstr(why, "SM-A566E"));
  // Then the codename, then the SoC (a phone of the same SoC gets its entry: the S24 on SM8650 the home device's).
  phone(tmp, "codename", "", "pa1q", "", "", "", props, sizeof props);
  CHECK(!strcmp(detect(props, empty, why, sizeof why), "galaxy-s25") && !strcmp(why, "matched by ro.product.device=pa1q"));
  phone(tmp, "s24", "SM-S921B", "e1q", "pineapple", "SM8650", "QTI", props, sizeof props);
  CHECK(!strcmp(detect(props, empty, why, sizeof why), "galaxy-s24-ultra") && !strcmp(why, "matched by ro.soc.model=SM8650 (the SoC's entry)"));
  phone(tmp, "platform", "", "", "SUN", "", "", props, sizeof props);
  CHECK(!strcmp(detect(props, empty, why, sizeof why), "galaxy-s25-ultra") && strstr(why, "ro.board.platform=SUN"));
  // An unknown phone: none, and what was read is said.
  phone(tmp, "pixel", "Pixel 11 Pro", "frankel", "zuma", "Tensor G5", "Google", props, sizeof props);
  CHECK(!strcmp(detect(props, empty, why, sizeof why), "unknown") && !strcmp(why, "unknown phone"));
  br_host_facts f;
  CHECK(br_host_read(&f, props, empty) == 0 && f.props && !strcmp(f.model, "Pixel 11 Pro") && !strcmp(f.soc_manufacturer, "Google"));
  br_host_describe(&f, line, sizeof line);
  CHECK(!strcmp(line, "ro.product.model=Pixel 11 Pro ro.product.device=frankel ro.board.platform=zuma ro.soc.model=Tensor G5 ro.soc.manufacturer=Google"));
  // The fallbacks: soc0's machine, the device tree's compatible list (a board's own entry does not match).
  char root[300], path[400];
  snprintf(root, sizeof root, "%s/soc0-root", tmp); mkdir(root, 0755);
  snprintf(path, sizeof path, "%s/sys", root); mkdir(path, 0755);
  snprintf(path, sizeof path, "%s/sys/devices", root); mkdir(path, 0755);
  snprintf(path, sizeof path, "%s/sys/devices/soc0", root); mkdir(path, 0755);
  snprintf(path, sizeof path, "%s/sys/devices/soc0/machine", root); put(path, "SM8850\n");
  snprintf(path, sizeof path, "%s/sys/devices/soc0/soc_id", root); put(path, "1234\n");
  snprintf(props, sizeof props, "%s/none.props", tmp); put(props, "# nothing\n");
  CHECK(!strcmp(detect(props, root, why, sizeof why), "galaxy-s26-ultra") && !strcmp(why, "matched by soc0.machine=SM8850 (the SoC's entry)"));
  CHECK(br_host_read(&f, props, root) == 0 && !strcmp(f.soc_id, "1234"));
  br_host_describe(&f, line, sizeof line);
  CHECK(strstr(line, "ro.product.model=(none)") && strstr(line, " soc0.machine=SM8850 soc0.soc_id=1234"));
  snprintf(root, sizeof root, "%s/dt-root", tmp); mkdir(root, 0755);
  snprintf(path, sizeof path, "%s/proc", root); mkdir(path, 0755);
  snprintf(path, sizeof path, "%s/proc/device-tree", root); mkdir(path, 0755);
  snprintf(path, sizeof path, "%s/proc/device-tree/compatible", root);
  FILE *dt = fopen(path, "w");
  if (dt) { fwrite("qcom,pineapple-mtp\0qcom,pineapple\0", 1, 35, dt); fclose(dt); }
  CHECK(!strcmp(detect(props, root, why, sizeof why), "galaxy-s24-ultra") && strstr(why, "compatible qcom,pineapple-mtp,qcom,pineapple"));
  dt = fopen(path, "w");
  if (dt) { fwrite("qcom,pineapple-mtp\0", 1, 19, dt); fclose(dt); }
  CHECK(!strcmp(detect(props, root, why, sizeof why), "unknown"));
  // build.prop's form too; a file that cannot be read is an error, not an unknown phone.
  snprintf(props, sizeof props, "%s/build.prop", tmp);
  put(props, "# build\nro.product.model=SM-A566B\nro.soc.model=s5e8855\n");
  CHECK(!strcmp(detect(props, empty, why, sizeof why), "galaxy-a56"));
  snprintf(path, sizeof path, "%s/missing.props", tmp);
  const br_host_device *e = &t[0];
  CHECK(br_host_choose("auto", path, empty, &f, &e, why, sizeof why) == -1 && !e && strstr(why, "cannot read"));

  // Forced and off: a name wins over what the phone says; none reads nothing; a bad name is refused.
  phone(tmp, "s24u", "SM-S928B", "e3q", "pineapple", "SM8650", "QTI", props, sizeof props);
  CHECK(br_host_choose("galaxy-a56", props, empty, &f, &e, why, sizeof why) == 0 && e == &t[4] && !strcmp(why, "host-device=galaxy-a56 (forced)") &&
        !strcmp(f.model, "SM-S928B"));
  CHECK(br_host_choose("none", props, empty, &f, &e, why, sizeof why) == 0 && !e && !strcmp(why, "host-device=none: detection off") && !f.props);
  CHECK(br_host_choose("pixel", props, empty, &f, &e, why, sizeof why) == -1 && !e && strstr(why, "host-device=pixel is not a known phone"));
  CHECK(br_host_choose(NULL, props, empty, &f, &e, why, sizeof why) == 0 && e == &t[0]);
  CHECK(br_host_choose("", props, empty, &f, &e, why, sizeof why) == 0 && e == &t[0]);

  // The environment's defaults: taken when unset, a variable already set wins (the S26 Ultra's brightness cap).
  unsetenv("ONEIOS_BRIGHT_MAX");
  const br_host_device *s26 = br_host_find("galaxy-s26-ultra");
  br_host_env_describe(s26, line, sizeof line);
  CHECK(!strcmp(line, "ONEIOS_BRIGHT_MAX=2300"));
  br_host_env_apply(s26);
  CHECK(getenv("ONEIOS_BRIGHT_MAX") && !strcmp(getenv("ONEIOS_BRIGHT_MAX"), "2300"));
  setenv("ONEIOS_BRIGHT_MAX", "1000", 1);
  br_host_env_describe(s26, line, sizeof line);
  CHECK(!strcmp(line, "ONEIOS_BRIGHT_MAX=2300 not taken (the environment's 1000)"));
  br_host_env_apply(s26);
  CHECK(!strcmp(getenv("ONEIOS_BRIGHT_MAX"), "1000"));
  unsetenv("ONEIOS_BRIGHT_MAX");
  br_host_env_apply(&t[0]);
  br_host_env_apply(NULL);
  CHECK(!getenv("ONEIOS_BRIGHT_MAX"));                    // the S24 Ultra's entry and none set nothing
  br_host_env_describe(&t[0], line, sizeof line);
  CHECK(!strcmp(line, ""));

  // The ctl tools: ONEIOS_HOST_DEVICE, ONEIOS_HOST_PROPS, ONEIOS_HOST_ROOT; the line; the environment set; hw untouched
  // (no entry has hw-backend defaults today) and what the environment gave kept.
  br_hw_settings hw;
  br_hw_settings_init(&hw);
  CHECK(br_hw_setting(&hw, "hw-backend-backlight", "treble") == 0);
  phone(tmp, "s26u", "SM-S948B", "", "", "SM8850", "QTI", props, sizeof props);
  setenv("ONEIOS_HOST_PROPS", props, 1);
  setenv("ONEIOS_HOST_ROOT", empty, 1);
  CHECK(br_host_tool(&hw, &e, line, sizeof line) == 0 && e == s26 && !strcmp(getenv("ONEIOS_BRIGHT_MAX"), "2300") &&
        !strcmp(line, "host: galaxy-s26-ultra, matched by ro.product.model=SM-S948B; read: ro.product.model=SM-S948B ro.product.device=(none) "
                      "ro.board.platform=(none) ro.soc.model=SM8850 ro.soc.manufacturer=QTI; defaults: ONEIOS_BRIGHT_MAX=2300"));
  CHECK(hw.one[BR_HW_BACKLIGHT] == BR_HW_TREBLE && !hw.all);
  unsetenv("ONEIOS_BRIGHT_MAX");
  phone(tmp, "s24u", "SM-S928B", "e3q", "pineapple", "SM8650", "QTI", props, sizeof props);
  setenv("ONEIOS_HOST_PROPS", props, 1);
  CHECK(br_host_tool(&hw, &e, line, sizeof line) == 0 && e == &t[0] && strstr(line, "host: galaxy-s24-ultra, matched by ro.product.model=SM-S928B;") &&
        strstr(line, "; defaults: none (today's)") && !getenv("ONEIOS_BRIGHT_MAX"));
  phone(tmp, "pixel", "Pixel 11 Pro", "frankel", "zuma", "Tensor G5", "Google", props, sizeof props);
  setenv("ONEIOS_HOST_PROPS", props, 1);
  CHECK(br_host_tool(&hw, &e, line, sizeof line) == 0 && !e && strstr(line, "host: unknown phone (ro.product.model=Pixel 11 Pro ") &&
        strstr(line, "): today's defaults"));
  setenv("ONEIOS_HOST_DEVICE", "none", 1);
  CHECK(br_host_tool(&hw, &e, line, sizeof line) == 0 && !e && !strcmp(line, "host: detection off (ONEIOS_HOST_DEVICE=none): today's defaults"));
  setenv("ONEIOS_HOST_DEVICE", "galaxy-s26-ultra", 1);
  CHECK(br_host_tool(&hw, &e, line, sizeof line) == 0 && e == s26 && strstr(line, "host-device=galaxy-s26-ultra (forced)") && !strcmp(getenv("ONEIOS_BRIGHT_MAX"), "2300"));
  setenv("ONEIOS_HOST_DEVICE", "toaster", 1);
  CHECK(br_host_tool(&hw, &e, line, sizeof line) == -1 && !e && strstr(line, "toaster is not a known phone") && strstr(line, "galaxy-a56"));
  unsetenv("ONEIOS_HOST_DEVICE"); unsetenv("ONEIOS_HOST_PROPS"); unsetenv("ONEIOS_HOST_ROOT"); unsetenv("ONEIOS_BRIGHT_MAX");

  char command[400];
  snprintf(command, sizeof command, "rm -rf %s", tmp);
  CHECK(system(command) == 0);
}

int main(void)
{
  br_hw_settings s, under;
  char why[64];
  br_log_sink = sink;

  // Nothing set: every bridge samsung, "the default", no fallback. A zeroed struct counts as nothing set too.
  br_hw_settings_init(&s);
  for (unsigned i = 0; i < BR_HW_NBRIDGE; i++) CHECK(br_hw_choose(&s, i, BR_HW_UNSET, why, sizeof why) == BR_HW_SAMSUNG && !strcmp(why, "the default"));
  CHECK(!br_hw_fallback_allowed(&s));
  br_hw_settings zero = { 0 };
  CHECK(br_hw_choose(&zero, BR_HW_BATTERY, BR_HW_UNSET, NULL, 0) == BR_HW_SAMSUNG && !br_hw_fallback_allowed(&zero));
  CHECK(br_hw_choose(NULL, BR_HW_BATTERY, BR_HW_UNSET, why, sizeof why) == BR_HW_SAMSUNG);

  // The keys: hw-backend, hw-backend-<bridge>, hw-backend-fallback; other keys are not ours; bad values refused.
  CHECK(br_hw_setting(&s, "hw-backend", "treble") == 0 && s.all == BR_HW_TREBLE);
  CHECK(br_hw_setting(&s, "hw-backend-touch", "samsung") == 0 && s.one[BR_HW_TOUCH] == BR_HW_SAMSUNG);
  CHECK(br_hw_setting(&s, "hw-backend-backlight", "treble") == 0 && s.one[BR_HW_BACKLIGHT] == BR_HW_TREBLE);
  CHECK(br_hw_setting(&s, "hw-backend-fallback", "1") == 0 && s.fallback == 1 && br_hw_fallback_allowed(&s));
  CHECK(br_hw_setting(&s, "hw-backend-fallback", "no") == 0 && s.fallback == 0 && !br_hw_fallback_allowed(&s));
  CHECK(br_hw_setting(&s, "hw-backend", "pixel") == -1 && s.all == BR_HW_TREBLE);
  CHECK(br_hw_setting(&s, "hw-backend-fallback", "maybe") == -1 && s.fallback == 0);
  CHECK(br_hw_setting(&s, "hw-backend-toaster", "treble") == -1);
  CHECK(br_hw_setting(&s, "hw-backend-battery", "") == -1 && s.one[BR_HW_BATTERY] == BR_HW_UNSET);
  CHECK(br_hw_setting(&s, "haptics", "sim") == 1 && br_hw_setting(&s, "hw-backendx", "treble") == 1);
  const char *names[] = { "backlight", "battery", "thermal", "haptics", "touch", "keys", "audio", "camera", "wifi", "bluetooth", "display", "encoder" };
  for (unsigned i = 0; i < BR_HW_NBRIDGE; i++) {
    char key[40];
    snprintf(key, sizeof key, "hw-backend-%s", names[i]);
    br_hw_settings t;
    br_hw_settings_init(&t);
    CHECK(!strcmp(br_hw_bridge_name(i), names[i]) && br_hw_setting(&t, key, "treble") == 0 && t.one[i] == BR_HW_TREBLE);
  }
  CHECK(!strcmp(br_hw_bridge_name(BR_HW_NBRIDGE), "?"));

  // The choice: per bridge over hw-backend over the default; a forced one (a test, a ctl tool) over all.
  CHECK(br_hw_choose(&s, BR_HW_TOUCH, BR_HW_UNSET, why, sizeof why) == BR_HW_SAMSUNG && !strcmp(why, "hw-backend-touch=samsung"));
  CHECK(br_hw_choose(&s, BR_HW_BACKLIGHT, BR_HW_UNSET, why, sizeof why) == BR_HW_TREBLE && !strcmp(why, "hw-backend-backlight=treble"));
  CHECK(br_hw_choose(&s, BR_HW_THERMAL, BR_HW_UNSET, why, sizeof why) == BR_HW_TREBLE && !strcmp(why, "hw-backend=treble"));
  CHECK(br_hw_choose(&s, BR_HW_THERMAL, BR_HW_SAMSUNG, why, sizeof why) == BR_HW_SAMSUNG && !strcmp(why, "the caller's choice"));

  // The command line over the image set's xnuhost.conf: what the first leaves unset comes from the second.
  br_hw_settings_init(&s);
  br_hw_settings_init(&under);
  CHECK(br_hw_setting(&s, "hw-backend-battery", "samsung") == 0);
  CHECK(br_hw_setting(&under, "hw-backend", "treble") == 0 && br_hw_setting(&under, "hw-backend-battery", "treble") == 0);
  CHECK(br_hw_setting(&under, "hw-backend-haptics", "treble") == 0 && br_hw_setting(&under, "hw-backend-fallback", "1") == 0);
  br_hw_settings_merge(&s, &under);
  CHECK(s.all == BR_HW_TREBLE && s.one[BR_HW_BATTERY] == BR_HW_SAMSUNG && s.one[BR_HW_HAPTICS] == BR_HW_TREBLE && s.fallback == 1);
  br_hw_settings_init(&under);
  CHECK(br_hw_setting(&s, "hw-backend-fallback", "0") == 0);
  br_hw_settings_merge(&s, &under);
  CHECK(s.fallback == 0);

  // The environment (the ctl tools).
  br_hw_settings_init(&s);
  const char *bad = NULL;
  setenv("ONEIOS_HW_BACKEND", "treble", 1);
  setenv("ONEIOS_HW_BACKEND_KEYS", "samsung", 1);
  setenv("ONEIOS_HW_FALLBACK", "1", 1);
  CHECK(br_hw_settings_env(&s, &bad) == 0 && s.all == BR_HW_TREBLE && s.one[BR_HW_KEYS] == BR_HW_SAMSUNG && s.fallback == 1);
  setenv("ONEIOS_HW_BACKEND_BATTERY", "lg", 1);
  CHECK(br_hw_settings_env(&s, &bad) == -1 && bad && !strcmp(bad, "ONEIOS_HW_BACKEND_BATTERY"));
  unsetenv("ONEIOS_HW_BACKEND"); unsetenv("ONEIOS_HW_BACKEND_KEYS"); unsetenv("ONEIOS_HW_FALLBACK"); unsetenv("ONEIOS_HW_BACKEND_BATTERY");

  // The process's settings and the log lines. Not configured: samsung, the default.
  CHECK(br_hw_select(BR_HW_BATTERY, BR_HW_UNSET) == BR_HW_SAMSUNG && !strcmp(last, "hw: battery: backend samsung (the default)") && last_level == 1);
  br_hw_settings_init(&s);
  CHECK(br_hw_setting(&s, "hw-backend", "treble") == 0);
  br_hw_configure(&s);
  CHECK(br_hw_select(BR_HW_THERMAL, BR_HW_UNSET) == BR_HW_TREBLE && !strcmp(last, "hw: thermal: backend treble (hw-backend=treble)"));
  CHECK(!br_hw_missing(BR_HW_THERMAL, "android.hardware.thermal.IThermal/default is not registered") && last_level == 0 &&
        !strcmp(last, "hw: thermal: treble: missing: android.hardware.thermal.IThermal/default is not registered; the bridge stays off (hw-backend-fallback=0)"));
  CHECK(br_hw_setting(&s, "hw-backend-fallback", "1") == 0);
  br_hw_configure(&s);
  CHECK(br_hw_missing(BR_HW_HAPTICS, "x") && last_level == 1 && !strcmp(last, "hw: haptics: treble: missing: x; falling back to samsung (hw-backend-fallback=1)"));
  br_hw_configure(NULL);
  CHECK(br_hw_select(BR_HW_HAPTICS, BR_HW_UNSET) == BR_HW_SAMSUNG && !br_hw_fallback_allowed(br_hw_current()));

  test_host_device();
  printf("hw_backend: %d checks, %d failures\n", checks, failures);
  return failures != 0;
}
