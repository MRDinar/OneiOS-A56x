// SPDX-License-Identifier: MIT
// The treble backend's HAL bridges against fake HALs on a real binder driver (`make test-binder`: a binderfs in a user
// namespace; skipped without one). A child plays servicemanager and the HALs (tests/fake_hal.c, values from files of
// a folder, read at every call); the parent opens the bridges with hw-backend treble and checks what they read and
// call: the lookup by name, the descriptor check, the parcels, a HAL that is not registered (the one missing line,
// then off, or the samsung form with hw-backend-fallback=1), and a HAL whose process dies under the bridge.
#define _GNU_SOURCE
#include "bridges/bridges.h"
#include "bridges/hw_backend.h"
#include "../battery/battery.h"
#include "../hal/hal.h"
#include <errno.h>
#include <fcntl.h>
#include <linux/android/binderfs.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ioctl.h>
#include <sys/stat.h>
#include <sys/wait.h>
#include <unistd.h>

int fake_hal_serve(const char *values, int ready_fd);

static int failures, checks;
#define CHECK(cond) do { checks++; if (!(cond)) { failures++; printf("FAIL %s:%d: %s\n", __FILE__, __LINE__, #cond); fflush(stdout); } } while (0)
static char lines[8192];
static void log_line(int level, const char *line)
{
  printf("  [%s] %s\n", level == 0 ? "E" : "I", line);
  size_t n = strlen(lines);
  snprintf(lines + n, sizeof lines - n, "%s\n", line);
}
static bool logged(const char *text) { return strstr(lines, text) != NULL; }

static char values[] = "/tmp/oneios-hal-XXXXXX";
static void put(const char *name, const char *text)
{
  char path[300];
  snprintf(path, sizeof path, "%s/%s", values, name);
  FILE *f = fopen(path, "w");
  if (f) { fputs(text, f); fclose(f); }
}

static pid_t start_fake(void)
{
  int fds[2];
  char line[8] = "";
  if (pipe(fds)) return -1;
  pid_t pid = fork();
  if (pid == 0) { close(fds[0]); _exit(fake_hal_serve(values, fds[1])); }
  close(fds[1]);
  CHECK(read(fds[0], line, 6) == 6 && !memcmp(line, "ready\n", 6));
  close(fds[0]);
  return pid;
}
static void stop_fake(pid_t pid) { kill(pid, SIGKILL); waitpid(pid, NULL, 0); }

static void binderfs_add(const char *path)
{
  const char *slash = strrchr(path, '/');
  if (!slash || access(path, F_OK) == 0) return;
  char control[512];
  snprintf(control, sizeof control, "%.*s/binder-control", (int)(slash - path), path);
  int fd = open(control, O_RDWR | O_CLOEXEC);
  if (fd < 0) return;
  struct binderfs_device d = { 0 };
  snprintf(d.name, sizeof d.name, "%s", slash + 1);
  (void)ioctl(fd, BINDER_CTL_ADD, &d);
  close(fd);
}

static void test_battery(void)
{
  put("health", "level=73\nstatus=2\npresent=1\ntemp=312\nvoltage_mv=4105\ncurrent_ua=1250000\ncycles=41\nfull_uah=4850000\ndesign_uah=5000000\nusb=1\n");
  pid_t pid = start_fake();
  br_battery_options o = { .backend = BR_HW_TREBLE, .dir = "/nonexistent", .poll_ms = 50 };
  lines[0] = 0;
  br_battery *b = br_battery_open(&o);
  CHECK(b != NULL);
  CHECK(logged("hw: battery: backend treble (the caller's choice)") && logged("battery: on: the health HAL android.hardware.health.IHealth/default (version 2), status \"Charging\", 73%, plugged in"));
  if (b) {
    br_battery_sample s;
    CHECK(br_battery_query(b, &s) == 0);
    CHECK(s.percent == 73 && s.present && s.plugged && s.charging && !s.full && s.temp_deci_c == 312 && s.voltage_mv == 4105);
    CHECK(s.current_ma == 1250 && s.full_mah == 4850 && s.design_mah == 5000 && s.cycles == 41 && s.minutes == (4850 - 4850 * 73 / 100) * 60 / 1250);
    // The HAL is asked again at every read: a change shows at the next query.
    put("health", "level=72\nstatus=3\npresent=1\ntemp=300\nvoltage_mv=4000\ncurrent_ua=-400000\ncycles=41\nfull_uah=4850000\n");
    CHECK(br_battery_query(b, &s) == 0 && s.percent == 72 && !s.plugged && !s.charging && s.current_ma == -400 && s.design_mah == 4850);
    // The HAL's process dies: the read fails, iOS keeps the last level, one error line.
    stop_fake(pid);
    pid = -1;
    lines[0] = 0;
    CHECK(br_battery_query(b, &s) == -1 && s.percent == 72);
    CHECK(logged("battery: the health HAL's getHealthInfo failed"));
    br_battery_close(b);
  }
  if (pid > 0) stop_fake(pid);

  // Not registered: the one missing line, then off; with hw-backend-fallback=1 the samsung nodes.
  char path[300];
  snprintf(path, sizeof path, "%s/health", values);
  unlink(path);
  pid = start_fake();
  lines[0] = 0;
  CHECK(br_battery_open(&o) == NULL);
  CHECK(logged("hw: battery: treble: missing: android.hardware.health.IHealth/default (no service: not registered with servicemanager, or SELinux denies the lookup); the bridge stays off (hw-backend-fallback=0)"));
  char sys[200];
  snprintf(sys, sizeof sys, "%s/power_supply", values);
  mkdir(sys, 0755);
  snprintf(path, sizeof path, "%s/battery", sys);
  mkdir(path, 0755);
  put("power_supply/battery/capacity", "55\n");
  put("power_supply/battery/status", "Discharging\n");
  br_hw_settings hw;
  br_hw_settings_init(&hw);
  CHECK(br_hw_setting(&hw, "hw-backend-fallback", "1") == 0);
  br_hw_configure(&hw);
  o.dir = sys;
  lines[0] = 0;
  br_battery *f = br_battery_open(&o);
  CHECK(f != NULL && logged("falling back to samsung (hw-backend-fallback=1)") && logged("battery: on: ") && logged("55%"));
  if (f) br_battery_close(f);
  br_hw_configure(NULL);
  stop_fake(pid);
}

int main(void)
{
  setvbuf(stdout, NULL, _IOLBF, 0);
  br_log_sink = log_line;
  const char *device = getenv("ONEIOS_BINDER");
  if (device) binderfs_add(device);
  if (!device || access(device, R_OK | W_OK)) { printf("test_hal_binder: set ONEIOS_BINDER to a binderfs binder device (make test-binder does)\n"); return 77; }
  if (!mkdtemp(values)) return 1;
  test_battery();
  char command[300];
  snprintf(command, sizeof command, "rm -rf %s", values);
  CHECK(system(command) == 0);
  printf("hal binder: %d checks, %d failed\n", checks, failures);
  return failures != 0;
}
