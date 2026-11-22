// SPDX-License-Identifier: BSD-2-Clause-Patent
// Ported from UniXNU-droidvm ipados27/scripts/make-droidvm-adt.py (commit f9121cd). See
// include/xnuloader/machine.h for the list of changes. The comments on single properties are the
// reasons that script records for them.
#include "xnuloader/machine.h"
#include <string.h>

#define FAIL(text) do { *error = (text); xl_adt_free(root); return NULL; } while (0)

static uint32_t adler32(const uint8_t *p, size_t n)
{
  uint32_t a = 1, b = 0;
  for (size_t i = 0; i < n; i++) { a = (a + p[i]) % 65521; b = (b + a) % 65521; }
  return b << 16 | a;
}

static void chrp_header(uint8_t *at, const char *name, unsigned size)
{
  memset(at, 0, 16);
  at[0] = 0x70;
  at[2] = (uint8_t)(size / 16);
  at[3] = (uint8_t)(size / 16 >> 8);
  memcpy(at + 4, name, strlen(name));
  unsigned sum = 0;
  for (int i = 0; i < 16; i++) sum += at[i];
  while (sum > 255) sum = (sum & 255) + (sum >> 8);
  at[1] = (uint8_t)sum;
}

void xl_nvram_proxy(uint8_t image[8192])
{
  memset(image, 0, 8192);
  chrp_header(image, "nvram", 32);
  chrp_header(image + 32, "common", 16 + 2048);
  unsigned free_at = 32 + 16 + 2048;
  chrp_header(image + free_at, "free", 8192 - free_at);
  uint32_t sum = adler32(image + 20, 8192 - 20);
  memcpy(image + 16, &sum, 4);
}

void xl_machine_mac(const xl_machine *m, unsigned which, uint8_t mac[6])
{
  static const uint8_t salt[3] = { 0, 0x5a, 0x69 };
  uint64_t seed = m->have_chip_id && m->unique_chip_id ? m->unique_chip_id : 0x4f6e6569;
  mac[0] = 0x02; mac[1] = 0xd7;                    // locally administered
  mac[2] = (uint8_t)(seed >> 24) ^ salt[which % 3];
  mac[3] = (uint8_t)(seed >> 16); mac[4] = (uint8_t)(seed >> 8); mac[5] = (uint8_t)seed;
}

void xl_machine_board(xl_machine *m, unsigned ncpu, const xl_device_kind *kinds, unsigned ndev)
{
  m->ncpu = ncpu;
  for (unsigned i = 0; i < ncpu && i < XL_MAX_CPUS; i++) {
    m->cpu_reg[i] = i;                    // Affinity0, as crosvm numbers its vCPUs
    if (!m->cpu_cluster[i]) m->cpu_cluster[i] = 'P';
  }
  m->armio_base = XL_BOARD_ARMIO_BASE;
  m->armio_size = XL_BOARD_ARMIO_SIZE;
  m->gic_dist = XL_BOARD_GIC_DIST;
  m->gic_dist_size = XL_BOARD_GIC_DIST_SIZE;
  m->gic_redist = XL_BOARD_GIC_REDIST;
  m->gic_redist_stride = XL_BOARD_GIC_STRIDE;
  m->gic_redist_size = XL_BOARD_GIC_STRIDE * (ncpu ? ncpu : 1);
  m->uart_base = XL_BOARD_UART;
  m->uart_size = XL_BOARD_UART_SIZE;
  m->psci_method = "hvc";
  m->ndev = ndev < XL_MAX_DEVICES ? ndev : XL_MAX_DEVICES;
  for (unsigned i = 0; i < m->ndev; i++) {
    m->dev[i].kind = kinds[i];
    m->dev[i].base = XL_BOARD_VIRTIO + i * XL_BOARD_VIRTIO_SIZE;
    m->dev[i].size = XL_BOARD_VIRTIO_SIZE;
  }
  m->host_io_base = XL_BOARD_IO_BASE;
  m->host_io_size = XL_BOARD_IO_SIZE;
}

// The /product properties of the identity tree a hosted machine does not have: the exclaves of
// Apple's SoCs, and the panel's gamut, which display_gamut sets.
static bool product_dropped(const char *name)
{
  static const char *const dropped[] = { "name", "AAPL,phandle", "exclaves-enabled", "has-exclaves", "artwork-display-gamut" };
  for (unsigned i = 0; i < sizeof dropped / sizeof *dropped; i++)
    if (!strcmp(name, dropped[i])) return true;
  return false;
}

// graphics-featureset-fallbacks without APPLE2 and MTL2,2 (a NUL-terminated, colon-separated list):
// with them MobileGestalt answers DeviceSupportsASTC and SplashBoard writes app snapshots in the AGX
// GPU's own ASTC layout, which no other GPU decodes.
static void set_featuresets(xl_adt_node *product, const xl_adt_prop *p)
{
  char out[512];
  size_t used = 0, at = 0, length = p->length;
  while (length && p->value[length - 1] == 0) length--;
  while (at <= length && length) {
    size_t end = at;
    while (end < length && p->value[end] != ':') end++;
    size_t n = end - at;
    bool drop = (n == 6 && !memcmp(p->value + at, "APPLE2", 6)) || (n == 6 && !memcmp(p->value + at, "MTL2,2", 6));
    if (!drop && used + n + 2 < sizeof out) {
      if (used) out[used++] = ':';
      memcpy(out + used, p->value + at, n);
      used += n;
    }
    at = end + 1;
  }
  out[used++] = 0;
  xl_adt_set(product, p->name, out, used);
}

static void reg(xl_adt_node *n, uint64_t armio_base, uint64_t base, uint64_t size)
{
  uint64_t v[2] = { base - armio_base, size };
  xl_adt_set(n, "reg", v, sizeof v);
}

xl_adt_node *xl_machine_tree(const xl_machine *m, const char **error)
{
  const char *ignored;
  xl_adt_node *root = NULL;
  if (!error) error = &ignored;
  *error = NULL;
  if (m->ncpu == 0 || m->ncpu > XL_MAX_CPUS) FAIL("1 to 8 CPUs");
  if (m->timebase_hz == 0) FAIL("the timebase frequency is missing");
  if ((m->dram_base | m->dram_size) & 0x3fff) FAIL("the RAM base and size must be 16 KiB aligned");
  if (m->armio_base == 0) FAIL("the arm-io base must not be 0");
  if (m->ncpu > 1 && (!m->psci_method || (strcmp(m->psci_method, "hvc") && strcmp(m->psci_method, "smc"))))
    FAIL("more than one CPU needs a PSCI conduit");
  if (m->gic_redist_size < m->gic_redist_stride * m->ncpu) FAIL("the redistributor window is smaller than one frame per CPU");
  const uint64_t windows[][2] = { { m->gic_dist, m->gic_dist_size }, { m->gic_redist, m->gic_redist_size } };
  for (unsigned i = 0; i < 2; i++)
    if (windows[i][0] < m->armio_base || windows[i][0] + windows[i][1] > m->armio_base + m->armio_size)
      FAIL("a GIC window lies outside the arm-io window");
  for (unsigned i = 0; i < m->ndev; i++)
    if (m->dev[i].base < m->armio_base || m->dev[i].base + m->dev[i].size > m->armio_base + m->armio_size)
      FAIL("a device window lies outside the arm-io window");
  if (m->panic_size && ((m->panic_base | m->panic_size) & 0x3fff)) FAIL("the panic region must be 16 KiB aligned");

  const xl_adt_node *id = m->identity, *id_product = id ? xl_adt_child(id, "product") : NULL;
  if (id && (!id_product || !xl_adt_get(id, "model") || !xl_adt_get(id, "target-type")))
    { *error = "the identity tree has no model, target-type or /product"; return NULL; }

  root = xl_adt_new("device-tree");
  char compatible[160];
  size_t used = 0;
  if (id) {
    const xl_adt_prop *p = xl_adt_get(id, "model");
    xl_adt_set(root, "model", p->value, p->length);
    p = xl_adt_get(id, "target-type");
    xl_adt_set(root, "target-type", p->value, p->length);
    // The device's board name first: libMobileGestalt reads the first entry as the hardware model,
    // and CoreMotion, the audio server and MediaExperience key their per-board tables on it.
    p = xl_adt_get(id, "compatible");
    if (p && p->length) {
      size_t n = strnlen((const char *)p->value, p->length);
      if (n && n + 1 < sizeof compatible - 32) { memcpy(compatible, p->value, n); compatible[n] = 0; used = n + 1; }
    }
  } else {
    xl_adt_set_str(root, "model", m->model ? m->model : "oneios-xnuhost");
    // The board (24A446's DeviceTree: target-type "V54" for the iPhone18,2). iOS names its jetsam properties by it:
    // without it runningboardd logs "Can't find jetsamproperties ('/System/Library/LaunchDaemons/com.apple.jetsamproperties'
    // | '' | 'plist')" (a PC boot, 2026-11-15) and has none of com.apple.jetsamproperties.V54.plist's bands and limits.
    if (m->target_type) xl_adt_set_str(root, "target-type", m->target_type);
  }
  memcpy(compatible + used, "oneios,xnuhost", sizeof "oneios,xnuhost");
  used += sizeof "oneios,xnuhost";
  xl_adt_set(root, "compatible", compatible, used);
  xl_adt_set_u32(root, "#address-cells", 2);
  xl_adt_set_u32(root, "#size-cells", 2);

  xl_adt_node *chosen = xl_adt_add(root, "chosen");
  xl_adt_set(chosen, "random-seed", m->random_seed, sizeof m->random_seed);
  if (m->have_chip_id) {
    xl_adt_set_u64(chosen, "unique-chip-id", m->unique_chip_id);
    xl_adt_set_u32(chosen, "chip-id", m->chip_id);
  }
  xl_adt_set_str(chosen, "osenvironment", "main");
  xl_adt_set_u32(chosen, "certificate-production-status", 0);
  xl_adt_set_u32(chosen, "effective-production-status-ap", 0);
  xl_adt_set_u32(chosen, "certificate-security-mode", 1);
  xl_adt_set_u32(chosen, "effective-security-mode-sep", 1);
  if (m->ramdisk || m->packed_ramdisk) xl_adt_set_u32(chosen, "ephemeral-storage", 1);
  xl_adt_set_u64(chosen, "dram-base", m->dram_base);
  xl_adt_set_u64(chosen, "dram-size", m->dram_size);
  xl_adt_set_str(chosen, "crypto-hash-method", "sha2-384");
  if (m->root_volume_uuid) {
    char match[256], uuid[40];
    size_t u = 0;
    for (; m->root_volume_uuid[u] && u < sizeof uuid - 1; u++) {
      char c = m->root_volume_uuid[u];
      uuid[u] = c >= 'a' && c <= 'f' ? (char)(c - 32) : c;
    }
    uuid[u] = 0;
    if (m->root_volume_uuid[u]) FAIL("root volume UUID too long");
    int n = snprintf(match, sizeof match,
                     "<dict><key>IOProviderClass</key><string>IOMedia</string><key>IOPropertyMatch</key>"
                     "<dict><key>UUID</key><string>%s</string></dict></dict>", uuid);
    xl_adt_set(chosen, "root-matching", match, (size_t)n + 1);
  }
  xl_adt_set_u32(chosen, "amfi-only-platform-code", 1);
  // What iBoot writes: 0 = nothing locks the kernel's text (no CTRR under a host or a hypervisor). xnu-12377 asserts
  // that it was given before DTrace's SDT probes ask ml_unsafe_kernel_text (bsd_init -> sdt_init).
  xl_adt_set_u32(chosen, "kernel-ctrr-to-be-enabled", 0);
  if (m->panic_size) xl_adt_set_u32(chosen, "embedded-panic-log-size", (uint32_t)m->panic_size);
  if (m->text_fixups && m->text_fixups_length) xl_adt_set(chosen, "text-fixups", m->text_fixups, m->text_fixups_length);
  {
    uint8_t image[8192];
    xl_nvram_proxy(image);
    xl_adt_set_u32(chosen, "nvram-bank-size", sizeof image);
    xl_adt_set_u32(chosen, "nvram-bank-count", 1);
    xl_adt_set_u32(chosen, "nvram-current-bank", 0);
    xl_adt_set(chosen, "nvram-proxy-data", image, sizeof image);
  }
  static const uint8_t empty[16];
  xl_adt_node *map = xl_adt_add(chosen, "memory-map");
  if (m->ramdisk || m->packed_ramdisk) xl_adt_set(map, m->packed_ramdisk ? "PackedRAMDisk" : "RAMDisk", empty, 16);
  // iOS 16's kernel reads the ramdisk's address from RAMDisk1 (port-a56, docs/a56.md); 27's never sees this node.
  if (m->ramdisk1 && (m->ramdisk || m->packed_ramdisk)) xl_adt_set(map, "RAMDisk1", empty, 16);
  if (m->trust_cache) xl_adt_set(map, "TrustCache", empty, 16);
  if (m->gpu_addon) xl_adt_set(map, "GpuAddon", empty, 16);
  if (m->root_policies) xl_adt_set(map, "RootPolicies", empty, 16);
  xl_adt_add(chosen, "manifest-properties");
  if (m->display_scale) {
    // The UI scale: MobileGestalt's main-screen-scale. The display driver publishes the same two words and
    // refuses to start its scale policy on a value that differs (LegacyDisplayProbe.cpp publishOrMatchWord).
    xl_adt_set_u32(chosen, "display-scale", m->display_scale);
    xl_adt_set_u32(chosen, "display-rotation", 0);
  }
  if (m->host_vector) {
    xl_adt_node *host = xl_adt_add(chosen, "xnuhost");
    xl_adt_set_u32(host, "abi", 1);
    xl_adt_set_u64(host, "vector", m->host_vector);
    uint64_t io[2] = { m->host_io_base, m->host_io_size };
    xl_adt_set(host, "io-aperture", io, sizeof io);
    xl_adt_set_u64(host, "user-va-limit", m->user_va_limit);
    xl_adt_set_u32(host, "host-page-size", m->host_page_size);
    if (m->display_width && m->display_height) {
      xl_adt_set_u32(host, "display-width", m->display_width);
      xl_adt_set_u32(host, "display-height", m->display_height);
      xl_adt_set_u32(host, "display-scale", m->display_scale);
      xl_adt_set_u32(host, "display-hz", m->display_hz);
    }
  }

  xl_adt_node *cpus = xl_adt_add(root, "cpus");
  for (unsigned i = 0; i < m->ncpu; i++) {
    char name[16];
    snprintf(name, sizeof name, "cpu%u", i);
    xl_adt_node *cpu = xl_adt_add(cpus, name);
    xl_adt_set_str(cpu, "state", i == 0 ? "running" : "waiting");
    xl_adt_set_u32(cpu, "reg", m->cpu_reg[i]);
    xl_adt_set_str(cpu, "device_type", "cpu");
    xl_adt_set_str(cpu, "compatible", "arm,armv8");
    xl_adt_set_u32(cpu, "timebase-frequency", m->timebase_hz);
    // An AMP kernel panics on a CPU of no cluster type, so every cpu node carries one.
    char cluster[2] = { m->cpu_cluster[i] == 'E' ? 'E' : 'P', 0 };
    xl_adt_set_str(cpu, "cluster-type", cluster);
  }
  if (m->ncpu > 1) {
    xl_adt_node *psci = xl_adt_add(root, "psci");
    xl_adt_set_str(psci, "compatible", "arm,psci-1.0");
    xl_adt_set_str(psci, "method", m->psci_method);
  }
  if (m->panic_size) {
    uint64_t v[2] = { m->panic_base, m->panic_size };
    xl_adt_set(xl_adt_add(root, "pram"), "reg", v, sizeof v);
  }
  xl_adt_node *defaults = xl_adt_add(root, "defaults");
  xl_adt_set_u32(defaults, "sleep-disabled", 1);
  // XNU publishes this as kern.hv_vmm_present; 24A446's bluetoothd then skips chip initialisation, and its power init
  // succeeds only with transport 10, its own simulated controller (docs/hardware.md 4, "The virtual transport").
  xl_adt_set_u32(defaults, "vmm-present", 1);
  // The vnode limit XNU sizes from memory, (bytes / 64 KiB) + 1024: without it the BCM2837
  // configuration clips it to 1024 and the vnode jetsam kills every idle process.
  xl_adt_set_u32(defaults, "kern.maxvnodes", (uint32_t)(m->dram_size / 0x10000 + 1024));
  // The I/O throttle an iPhone18,2 has (XNU throttle_init_throttle_window/_period read these before any disk opens).
  // The hosted disks answer DKIOCISSOLIDSTATE 0 (`spec_open : ... isssd is (0)`), so XNU took its rotating-disk
  // table: a utility (tier 2) I/O slept 85 ms and a background (tier 3) I/O 200 ms, within 500 ms of any foreground
  // I/O on the same disk, while the iPhone's NAND gets the SSD table (5, 15, 25 ms) and its DeviceTree /defaults
  // (24A446) sets kern.io_throttle_period_tier3 20 and kern.io_throttle_window_tier3 100. These are the iPhone's
  // effective values, whatever the disk answers; the tier 1 and tier 2 windows (25, 100 ms) are XNU's on both.
  xl_adt_set_u32(defaults, "kern.io_throttle_period_tier1", 5);
  xl_adt_set_u32(defaults, "kern.io_throttle_period_tier2", 15);
  xl_adt_set_u32(defaults, "kern.io_throttle_period_tier3", 20);
  xl_adt_set_u32(defaults, "kern.io_throttle_window_tier3", 100);
  // ram-view= (xnuhost): the per-process memory limits an iPhone18,2 has, from its own device tree (24A446's
  // DeviceTree /defaults: kern.max_task_pmem 3376, kern.entitled_max_task_pmem 6144, kern.entitled_dev_max_task_pmem
  // 6656 MB, for its 12 GiB), scaled by the view. XNU reads them in task_init (max_task_footprint) and
  // memorystatus_init (the entitled raise of apps with com.apple.developer.kernel.increased-memory-limit). A footprint
  // counts the task's compressed pages too (phys_footprint), so these are limits on what the compressor holds as well.
  // Without the view: the iPhone18,2's own default task limit (kern.max_task_pmem 3376 MB; before 2026-11-15 the tree had
  // none, so a task with no limit of its own from launchd or RunningBoard had none at all) and no entitled raise, as before.
  if (!m->view_size) xl_adt_set_u32(defaults, "kern.max_task_pmem", 3376);
  else {
    static const struct { const char *name; uint32_t mb; } limits[] = {
      { "kern.max_task_pmem", 3376 }, { "kern.entitled_max_task_pmem", 6144 }, { "kern.entitled_dev_max_task_pmem", 6656 },
    };
    for (unsigned i = 0; i < sizeof limits / sizeof *limits; i++)
      xl_adt_set_u32(defaults, limits[i].name, (uint32_t)((uint64_t)limits[i].mb * (m->view_size >> 20) / (12u << 10)));
  }
  xl_adt_add(root, "options");

  // bluetoothd finds its controller by name; transport-encoding 10 is the virtual transport.
  xl_adt_node *bt = xl_adt_add(root, "bluetooth");
  xl_adt_set_str(bt, "device_type", "bluetooth");
  xl_adt_set_u32(bt, "transport-encoding", 10);
  uint8_t mac[6];
  if (m->bt_transport) {
    xl_adt_set_u32(bt, "transport-encoding", m->bt_transport);
    if (m->bt_speed) xl_adt_set_u32(bt, "transport-speed", m->bt_speed);
    memcpy(mac, m->bt_mac, 6);
  } else xl_machine_mac(m, 0, mac);
  xl_adt_set(bt, "local-mac-address", mac, 6);
  // The three addresses the userland's boot contract requires, each stable for one unique chip ID.
  xl_adt_set(chosen, "mac-address-bluetooth0", mac, 6);
  xl_machine_mac(m, 1, mac);
  xl_adt_set(chosen, "mac-address-wifi0", mac, 6);
  xl_machine_mac(m, 2, mac);
  xl_adt_set(chosen, "mac-address-ethernet0", mac, 6);

  xl_adt_node *buttons = xl_adt_add(root, "buttons");
  xl_adt_set_str(buttons, "compatible", "buttons");
  xl_adt_set_str(buttons, "device_type", "buttons");
  xl_adt_set(buttons, "button-names", "hold\0voldown\0volup", sizeof "hold\0voldown\0volup");
  xl_adt_set_u32(buttons, "home-button-type", 2);

  xl_adt_node *product = xl_adt_add(root, "product");
  if (id_product) {
    for (unsigned i = 0; i < id_product->nprops; i++) {
      const xl_adt_prop *p = &id_product->props[i];
      if (product_dropped(p->name)) continue;
      if (!strcmp(p->name, "graphics-featureset-fallbacks")) set_featuresets(product, p);
      else xl_adt_set(product, p->name, p->value, p->length);
    }
    xl_adt_set_str(product, "artwork-display-gamut", m->display_gamut ? m->display_gamut : "P3");
    xl_adt_node *audio = xl_adt_add(product, "audio");
    xl_adt_set_u32(audio, "wireless-splitter", 1);
    const xl_adt_node *id_audio = xl_adt_child(id_product, "audio");
    const xl_adt_prop *acoustic = id_audio ? xl_adt_get(id_audio, "acoustic-id") : NULL;
    if (acoustic) xl_adt_set(audio, "acoustic-id", acoustic->value, acoustic->length);
  } else {
    xl_adt_set_str(product, "product-name", "OneiOS guest");
    // No identity tree (the phone's image set has none): the iPhone18,2 answers that CoreUI asks MobileGestalt for
    // (kMGArtworkDeviceIdiom, kMGArtworkDisplayGamut, kMGArtworkGraphicsFeatureSetClass, the subtype). Without them
    // every app logged `CUICatalog: couldn't get 'kMGArtworkDeviceIdiom' from MGCopyAnswer got '{ArtworkDeviceScaleFactor
    // = 3}'`, its asset catalogs chose no image, and UIKit probed loose files for each image on every redraw
    // (_UIImageCollectImagePathsForPath: about 6000 faccessat a second in Spotlight, Notes and WebSheet, 105% of a core,
    // keys waiting for the main thread; the phone, 2026-11-22). The values are 24A446's V54 device tree's /product,
    // with this host's choices: the gamut is the display's (sRGB unless display_gamut says so: the helper tags the
    // frames sRGB), and the GPU fallbacks lose APPLE2 and MTL2,2 as with an identity (set_featuresets).
    xl_adt_set_str(product, "artwork-device-idiom", "phone");
    xl_adt_set_u32(product, "artwork-device-subtype", 2868);
    xl_adt_set_str(product, "artwork-display-gamut", m->display_gamut ? m->display_gamut : "sRGB");
    xl_adt_set_str(product, "artwork-dynamic-displaymode", "0");
    xl_adt_set_str(product, "graphics-featureset-class", "APPLE10");
    static char fallbacks[] = "APPLE9:APPLE8:APPLE7:APPLE6:APPLE5:APPLE4:APPLE3:APPLE3v1:APPLE2:APPLE1:GLES2,0";
    xl_adt_prop listed = { .name = "graphics-featureset-fallbacks", .length = sizeof fallbacks, .value = (uint8_t *)fallbacks };
    set_featuresets(product, &listed);
  }
  // The artwork scale follows the UI scale (the identity tree's is the iPhone panel's 3). The artwork subtype
  // stays the identity's (2868 for iPhone18,2): it names a set of assets, and no set exists for another height.
  if (m->display_scale) xl_adt_set_u32(product, "artwork-scale-factor", m->display_scale);
  xl_adt_set_str(product, "model", "oneios,xnuhost");
  // The cameras (with the camera device only, so a host without one keeps the tree it had). 24A446's libMobileGestalt
  // answers video-camera from /product/camera being there (no property of it is needed; iOSPlus10 adds it empty), and
  // HWModelDescriptionForCamera (= ...ForAnalytics, ...ForAudio, ...ForPowerPerf, ...ForAutomatedTesting) from /product
  // unique-model. CMCapture's FigCaptureGetModelSpecificName takes that answer less "AP" as the folder of its per-model
  // tables (CMCapture.framework/V54/AVCaptureSession.plist, CameraSetup.plist, ...); without it the name was NULL and
  // every built-in source failed (iOS's log, 2026-11-22: Fig assert "modelSpecificAVCaptureSessionPlist" at
  // FigCaptureSourceBackingsProvider.m:2316, err -12780, "Created 0 in-memory built-in backings").
  // The name is not the iPhone18,2's own "V54AP" by default: AVFCapture's AVGestalt (in audiomxd too) finds V54 in its
  // table of 41 iPhone models and answers its webcam key true, VirtualAudio's RoutingSettings_N71 then builds a webcam
  // subport whose DSP list exists only with input processing on (here it is off), its precondition fails, and
  // VirtualAudio never starts: no speaker, no volume, no haptics (iOS's log, v0.1.16, 2026-11-23: "Need valid DSPConfig
  // for webcam recording", VA Init Status: 1). XL_CAMERA_MODEL ("OneiOSAP") is in no such table, so AVGestalt answers
  // as with no name, and the hosted kernel (0088) shows CMCapture.framework/OneiOS as a link to the sealed V54 folder.
  // m->unique_model (xnuhost's camera-model=) wins; without it an identity tree's /product keeps its own.
  bool camera_device = false;
  for (unsigned i = 0; i < m->ndev; i++) camera_device |= m->dev[i].kind == XL_DEV_CAMERA;
  const uint32_t cameras = camera_device ? m->cameras & (XL_CAMERA_REAR | XL_CAMERA_FRONT) : 0;
  if (cameras) {
    xl_adt_add(product, "camera");
    if (m->unique_model || !xl_adt_get(product, "unique-model"))
      xl_adt_set_str(product, "unique-model", m->unique_model ? m->unique_model : XL_CAMERA_MODEL);
  }

  xl_adt_node *io = xl_adt_add(root, "arm-io");
  xl_adt_set_str(io, "device_type", "arm-io");
  xl_adt_set_u32(io, "#address-cells", 2);
  xl_adt_set_u32(io, "#size-cells", 2);
  uint64_t ranges[3] = { 0, m->armio_base, m->armio_size };
  xl_adt_set(io, "ranges", ranges, sizeof ranges);
  xl_adt_node *intc = xl_adt_add(io, "intc");
  xl_adt_set_str(intc, "compatible", "arm,gic-v3");
  uint64_t gic[4] = { m->gic_dist - m->armio_base, m->gic_dist_size, m->gic_redist - m->armio_base, m->gic_redist_size };
  xl_adt_set(intc, "reg", gic, sizeof gic);
  xl_adt_set_u64(intc, "redistributor-stride", m->gic_redist_stride);
  // launchd loads wifid only where the tree has a node named wlan; no driver matches it.
  xl_adt_set_str(xl_adt_add(io, "wlan"), "device_type", "wlan");
  // MobileGestalt answers rear-facing-camera from IODeviceTree:/arm-io/isp camera-rear, front-facing-camera from its
  // camera-front, and still-camera (Camera.app's SBAppTags: SpringBoard shows the app only with it) from either. The
  // iPhone18,2's values are 1 (u32). No compatible and no device_type: no driver matches the node (the camera's
  // driver is drivers/camera on /arm-io/camera, and CMCapture finds it by the AppleH9CamIn class, not by this node).
  if (cameras) {
    xl_adt_node *isp = xl_adt_add(io, "isp");
    if (cameras & XL_CAMERA_REAR) xl_adt_set_u32(isp, "camera-rear", 1);
    if (cameras & XL_CAMERA_FRONT) xl_adt_set_u32(isp, "camera-front", 1);
  }

  unsigned blocks = 0, inputs = 0;
  for (unsigned i = 0; i < m->ndev; i++) {
    const xl_device *d = &m->dev[i];
    char name[32];
    const char *compat;
    switch (d->kind) {
    case XL_DEV_BLOCK:      snprintf(name, sizeof name, "virtio-mmio-%u", blocks++); compat = "virtio,mmio"; break;
    case XL_DEV_DATA_STORE: snprintf(name, sizeof name, "data-store"); compat = "droidvm,data-store"; break;
    case XL_DEV_INPUT:      snprintf(name, sizeof name, "input-%u", inputs++); compat = "droidvm,virtio-input"; break;
    case XL_DEV_NET:        snprintf(name, sizeof name, "net"); compat = "droidvm,net"; break;
    case XL_DEV_GPU:        snprintf(name, sizeof name, "virtio-gpu"); compat = "droidvm,virtio-gpu"; break;
    case XL_DEV_SND:        snprintf(name, sizeof name, "virtio-snd"); compat = "droidvm,virtio-snd"; break;
    case XL_DEV_BRIDGE:     snprintf(name, sizeof name, "bridge"); compat = "oneios,bridge"; break;
    case XL_DEV_CAMERA:     snprintf(name, sizeof name, "camera"); compat = "oneios,camera"; break;
    case XL_DEV_MEDIA:      snprintf(name, sizeof name, "virtio-media-0"); compat = "droidvm,virtio-media"; break;
    default: FAIL("unknown device kind");
    }
    if (xl_adt_child(io, name)) FAIL("two devices of a kind that has one node");
    xl_adt_node *n = xl_adt_add(io, name);
    xl_adt_set_str(n, "compatible", compat);
    reg(n, m->armio_base, d->base, d->size);
    xl_adt_set(n, "dma-coherent", "", 0);
    // The line of the window on the board's GIC: SPI INTID 32 + its index, under the name UniXNU-droidvm's
    // transports read. Not "interrupts": the tree has no interrupt-controller node (ArmCpu owns the GIC), and
    // IOKit's IODTMapOneInterrupt walks the parents of a node with "interrupts" forever looking for one.
    xl_adt_set_u32(n, "droidvm,intid", XL_BOARD_VIRTIO_SPI + i);
    if (d->kind == XL_DEV_GPU && m->gpu_pool_size) {
      uint64_t pool[2] = { m->gpu_pool_base, m->gpu_pool_size };
      xl_adt_set(n, "drm-host-pool", pool, sizeof pool);
    }
    if (d->kind == XL_DEV_MEDIA && m->media_pool_size) {
      uint64_t pool[2] = { m->media_pool_base, m->media_pool_size };
      xl_adt_set(n, "media-host-pool", pool, sizeof pool);
    }
  }
  if (m->cpufreq_size) {
    xl_adt_node *freq = xl_adt_add(io, "cpufreq");
    xl_adt_set_str(freq, "compatible", "oneios,cpufreq");
    reg(freq, m->armio_base, m->cpufreq_base, m->cpufreq_size);
  }
  // XNU's PL011 console finds the node named "uart0" at soc_base + reg.
  if (m->uart_size) {
    xl_adt_node *uart = xl_adt_add(io, "uart0");
    xl_adt_set_str(uart, "compatible", "arm,pl011");
    reg(uart, m->armio_base, m->uart_base, m->uart_size);
  }
  return root;
}

static uint64_t fnv(uint64_t h, const void *data, size_t length)
{
  const uint8_t *p = data;
  for (size_t i = 0; i < length; i++) { h ^= p[i]; h *= 0x100000001b3ull; }
  return h;
}

// What changes from boot to boot, or with the RAM xnuhost got this time: never part of the identity.
static bool boot_to_boot(const char *name)
{
  static const char *const names[] = { "random-seed", "dram-base", "dram-size", "drm-host-pool", "media-host-pool", "oneios-identity" };
  for (unsigned i = 0; i < sizeof names / sizeof *names; i++)
    if (!strncmp(name, names[i], XL_ADT_NAME_MAX)) return true;
  return false;
}

// A node's properties (less boot_to_boot and, with `prefix`, only those that start with it) and, with `deep`, its
// children's, in the tree's order.
static uint64_t hash_node(uint64_t h, const xl_adt_node *n, bool deep, const char *prefix)
{
  for (unsigned i = 0; i < n->nprops; i++) {
    const xl_adt_prop *p = &n->props[i];
    if (boot_to_boot(p->name) || (prefix && strncmp(p->name, prefix, strlen(prefix)))) continue;
    uint32_t length = p->length;
    h = fnv(h, p->name, strnlen(p->name, XL_ADT_NAME_MAX));
    h = fnv(h, &length, sizeof length);
    h = fnv(h, p->value, p->length);
  }
  if (deep) {
    uint32_t count = n->nchildren;
    h = fnv(h, &count, sizeof count);
    for (unsigned i = 0; i < n->nchildren; i++) h = hash_node(h, n->children[i], true, NULL);
  }
  return h;
}

uint64_t xl_machine_identity_hash(const xl_adt_node *tree)
{
  uint64_t h = hash_node(0xcbf29ce484222325ull, tree, false, NULL);   // the root's own properties
  // Each part with a marker byte first, so that a missing node and an empty one differ.
  static const char *const parts[] = { "product", "arm-io", "buttons", "bluetooth" };
  for (unsigned i = 0; i < sizeof parts / sizeof *parts; i++) {
    const xl_adt_node *n = xl_adt_child(tree, parts[i]);
    h = fnv(h, n ? "+" : "-", 1);
    if (n) h = hash_node(h, n, true, NULL);
  }
  // /chosen's own properties (the chip ID, the MAC addresses, display-scale, the root's volume, ...) and the display
  // mode of /chosen/xnuhost; not its memory map (addresses of this boot) nor the host vector (xnuhost's own ASLR).
  const xl_adt_node *chosen = xl_adt_child(tree, "chosen"), *host = chosen ? xl_adt_child(chosen, "xnuhost") : NULL;
  h = fnv(h, chosen ? "+" : "-", 1);
  if (chosen) h = hash_node(h, chosen, false, NULL);
  h = fnv(h, host ? "+" : "-", 1);
  if (host) h = hash_node(h, host, false, "display-");
  return h;
}

uint64_t xl_machine_identity(xl_adt_node *tree)
{
  uint64_t h = xl_machine_identity_hash(tree);
  xl_adt_set_u64(xl_adt_add(tree, "chosen"), "oneios-identity", h);
  return h;
}
