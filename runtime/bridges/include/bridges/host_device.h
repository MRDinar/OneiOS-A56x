// SPDX-License-Identifier: MIT
// The host phone: which known phone xnuhost (and each bridge's ctl tool) runs on, and that phone's defaults
// (docs/hardware.md, "The host device"; docs/architecture.md, "The host device table"). One tree, many phones: the
// table in hw/host_device.c is the one place that says what differs per phone, and it holds only settings that
// already exist as options (an xnuhost setting KEY=VALUE, or an environment variable a bridge reads).
//
// Detection reads, once at the start and read only:
//   the properties ro.product.model, ro.product.device, ro.board.platform, ro.soc.model, ro.soc.manufacturer
//     (bionic's property area, as usb.c reads it; or a getprop dump given as a file: tests, a PC replaying a phone)
//   and as fallbacks /sys/devices/soc0/machine, /sys/devices/soc0/soc_id (logged only) and /proc/device-tree/compatible
//     (under a root folder that tests replace)
// and matches, in this order: a model prefix (ro.product.model), a codename (ro.product.device), the SoC (ro.soc.model,
// then soc0's machine), the platform (ro.board.platform), the device tree's compatible list. The first entry that
// matches wins. No match: no entry (today's defaults) and a line that says the phone is unknown and what was read.
//
// Precedence (xnuhost's config.c; the ctl tools likewise): an explicit setting (the command line, the image set's
// xnuhost.conf, an environment variable that is set) > the entry's default > the built-in default.
// Settings: xnuhost `host-device=auto|none|NAME` (auto: detect; none: no entry, today's defaults; NAME: that entry,
// whatever the phone says), `host-props=FILE`, `host-root=DIR`; the ctl tools ONEIOS_HOST_DEVICE, ONEIOS_HOST_PROPS,
// ONEIOS_HOST_ROOT.
#ifndef ONEIOS_BRIDGES_HOST_DEVICE_H
#define ONEIOS_BRIDGES_HOST_DEVICE_H

#include <stdbool.h>
#include <stddef.h>
#include "bridges/hw_backend.h"

#define BR_HOST_VALUE 92                  // PROP_VALUE_MAX
#define BR_HOST_MATCH 4                   // match strings of one kind per entry
#define BR_HOST_SETTINGS 6                // defaults of one kind per entry

typedef struct {
  const char *name;                       // the host-device= name: "galaxy-s24-ultra"
  const char *label;                      // "Galaxy S24 Ultra (SM-S928, SM8650)"
  const char *models[BR_HOST_MATCH];      // ro.product.model prefixes ("SM-S928": SM-S928B, SM-S928U, ...)
  const char *devices[BR_HOST_MATCH];     // ro.product.device codenames, whole ("e3q")
  const char *socs[BR_HOST_MATCH];        // ro.soc.model / soc0's machine, whole, any case ("SM8650")
  const char *platforms[BR_HOST_MATCH];   // ro.board.platform, whole, any case ("pineapple")
  const char *config[BR_HOST_SETTINGS];   // xnuhost settings KEY=VALUE (only keys config.c merges as device defaults)
  const char *env[BR_HOST_SETTINGS];      // environment defaults NAME=VALUE for the bridges (a variable already set wins)
} br_host_device;

typedef struct {
  char model[BR_HOST_VALUE], device[BR_HOST_VALUE], platform[BR_HOST_VALUE];
  char soc_model[BR_HOST_VALUE], soc_manufacturer[BR_HOST_VALUE];
  char machine[64], soc_id[32];           // /sys/devices/soc0/{machine,soc_id}, "" when absent
  char compatible[160];                   // /proc/device-tree/compatible, its entries joined by ',' ... first ones only
  bool props;                             // a property source was there (bionic's area or the file)
} br_host_facts;

// The table, in match order (the home device first).
const br_host_device *br_host_table(unsigned *count);
// An entry by its name (any case); NULL when there is none.
const br_host_device *br_host_find(const char *name);
// The names, comma-separated: "galaxy-s24-ultra, galaxy-s25-ultra, ...".
void br_host_names(char *out, size_t size);

// Reads the facts. props_file: a getprop dump ("[key]: [value]" lines, or key=value) instead of the property area
// (NULL or "": the property area on Android, none elsewhere). root: a folder that stands for "/" for the sysfs and proc
// fallbacks (NULL or "": "/"). 0, or -1 when props_file was given and cannot be read.
int  br_host_read(br_host_facts *f, const char *props_file, const char *root);
// The entry the facts match (NULL: none) and `why` the fact that matched it: "ro.product.model=SM-S928B".
const br_host_device *br_host_match(const br_host_facts *f, char *why, size_t size);
// What was read, for the log: "ro.product.model=SM-S928B ro.product.device=e3q ro.board.platform=pineapple
// ro.soc.model=SM8650 ro.soc.manufacturer=QTI" and, when present, "soc0.machine=... soc0.soc_id=... dt=...".
void br_host_describe(const br_host_facts *f, char *out, size_t size);

// The choice for a setting (auto, "" or NULL: detect; none; a name): 0 and *entry (NULL: none or unknown), with `why`:
// "matched by ro.product.model=SM-S928B", "host-device=galaxy-a56 (forced)", "host-device=none: detection off",
// "unknown phone". -1: a name that is not in the table. `f` is read here unless the setting is none.
int  br_host_choose(const char *setting, const char *props_file, const char *root, br_host_facts *f,
                    const br_host_device **entry, char *why, size_t size);

// The entry's environment defaults: what each would do, for the log ("ONEIOS_BRIGHT_MAX=2300" taken, or
// "ONEIOS_BRIGHT_MAX=2300 not taken (the environment's 1000)"), joined by ", "; and setting them (setenv without
// overwrite: a variable already set wins). Call apply while the process has one thread.
void br_host_env_describe(const br_host_device *entry, char *out, size_t size);
void br_host_env_apply(const br_host_device *entry);

// The ctl tools (brightctl, batteryctl, thermalctl, bridgectl): ONEIOS_HOST_DEVICE, ONEIOS_HOST_PROPS, ONEIOS_HOST_ROOT,
// detect, apply the entry's environment defaults, merge its hw-backend-* defaults under `hw` (which holds the
// environment's own), and write the one line ("host: ..."). 0, or -1 (a bad ONEIOS_HOST_DEVICE or ONEIOS_HOST_PROPS,
// the reason in `line`).
int  br_host_tool(br_hw_settings *hw, const br_host_device **entry, char *line, size_t size);

#endif
