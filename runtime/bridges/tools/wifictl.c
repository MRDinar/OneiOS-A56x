// SPDX-License-Identifier: MIT
// wifictl: the Wi-Fi bridge on the phone without xnuhost or a guest (docs/hardware.md section 3). Run as root (su):
//   wifictl status [IFACE]           read only: the interface's address, the supplicant's AIDL service, its station
//                                    interface, its networks' ids, the signal poll. Safe beside One UI.
//   wifictl scan [SECONDS [IFACE]]   one nl80211 scan (default 6 s): each network's name, access point, channel
//                                    frequency, signal and security. Safe beside One UI (its own scans do the same).
//   wifictl join SSID [PASSPHRASE [SECONDS]]
//                                    the whole host path with this tool as the guest: takes wlan0 as a session does
//                                    (the supplicant's networks removed, IPv4 addresses removed), scans, joins,
//                                    asks for an address by DHCP and prints the offer, stays SECONDS (default 5),
//                                    leaves and gives wlan0 back. It takes the Wi-Fi from One UI while it runs: One
//                                    UI joins its network again by itself after it. Not during a session.
//   wifictl restore FILE             wlan0's settings back from a session's wifi.state (after a killed xnuhost)
//   wifictl p2p [IFACE]              read only: what every radio offers for a peer-to-peer link (AirDrop's AWDL:
//                                    docs/hardware.md 4a): each wiphy's interface types and combinations, action
//                                    frames, off-channel sends, NAN, Wi-Fi Direct, vendor commands; every interface;
//                                    Google's Wonder radio (its subcommands under 00:1a:11, who holds it); the
//                                    verdicts for IFACE's radio (default wlan0); Wonder's module, driver and
//                                    files, mosey_server; the network devices, modules and processes whose names hint
//                                    at AWDL or NAN. Safe beside One UI and in a session.
//   wifictl wonder [--seconds N] [--freq MHZ] [--country CC] [--station IFACE]
//                                    a session on Wonder's raw 802.11 path beside wlan0 (wifi/wonder.c): a monitor
//                                    interface on Wonder's radio, its country (the driver's own), frequency (2437,
//                                    5220 or 5745 MHz; default 2437), TX rate and AWDL's BSSID set, up (the driver's
//                                    PASSTHRU vdev wondertap0 opens), N s (default 30) of frames counted (AWDL's
//                                    action and data frames, by peer), then all of it closed in reverse and wlan0's
//                                    link checked against its link before. Refused when Wonder is missing or in use.
//                                    Root, One UI running; Ctrl-C ends the count and still closes everything.
// Each ends with "ok" or "failed".
#define _GNU_SOURCE
#include "bridges/bridges.h"
#include "../wifi/supplicant.h"
#include "../wifi/wifi.h"
#include <dirent.h>
#include <errno.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

static void say(int level, const char *line) { printf("  %s%s\n", level == 0 ? "error: " : "", line); }

static const char *security_text(const uint8_t *ies, size_t n, uint16_t capability)
{
  static char text[64];
  br_wifi_security s;
  br_wifi_security_parse(ies, n, &s);
  if (!s.rsn && !s.wpa) return capability & 0x10 ? "WEP" : "open";
  snprintf(text, sizeof text, "%s%s%s%s%s%s", s.sae ? "WPA3 " : "", s.psk || s.psk_sha256 ? (s.rsn ? "WPA2 " : "WPA ") : "", s.eap ? "802.1X " : "",
           s.owe ? "OWE " : "", s.pmf_required ? "PMF required " : "", s.tkip && !s.ccmp ? "TKIP only" : "");
  return text;
}

static void print_bss(void *ctx, const struct ow_bss *b, const uint8_t *ies, size_t n)
{
  (void)ctx;
  char name[33] = "";
  for (unsigned i = 0; i < b->ssid_length && i < 32; i++) name[i] = b->ssid[i] >= 0x20 && b->ssid[i] < 0x7f ? (char)b->ssid[i] : '?';
  printf("  %02x:%02x:%02x:%02x:%02x:%02x %4u MHz %4d dBm  %-22s \"%s\"%s\n", b->bssid[0], b->bssid[1], b->bssid[2], b->bssid[3], b->bssid[4], b->bssid[5],
         b->freq_mhz, b->signal_dbm, security_text(ies, n, b->capability), name, b->ssid_length ? "" : " (hidden)");
}

static int scan(int argc, char **argv)
{
  unsigned seconds = argc > 2 ? (unsigned)atoi(argv[2]) : 6;
  int n = br_wlan_scan_once(argc > 3 ? argv[3] : "wlan0", seconds * 1000, print_bss, NULL);
  if (n < 0) { printf("scan: %s\n", strerror(-n)); return 1; }
  printf("scan: %d networks\n", n);
  return 0;
}

static int status(int argc, char **argv)
{
  const char *ifname = argc > 2 ? argv[2] : "wlan0";
  char path[128], text[64] = "";
  snprintf(path, sizeof path, "/sys/class/net/%s/address", ifname);
  FILE *f = fopen(path, "r");
  if (f) { if (!fgets(text, sizeof text, f)) text[0] = 0; fclose(f); }
  text[strcspn(text, "\n")] = 0;
  printf("%s: %s\n", ifname, *text ? text : "no such interface");
  br_supplicant *s = br_supplicant_open(ifname, 0, NULL, NULL);       // a look: nothing registered, added or removed
  if (!s) return 1;
  uint8_t mac[6];
  int32_t ids[32], poll[5];
  unsigned n = 32;
  int rc = br_supplicant_status(s, mac, ids, &n, poll);
  if (!rc) {
    printf("supplicant: service %s, station interface %s, address %02x:%02x:%02x:%02x:%02x:%02x, %u network(s):", SUP_SERVICE, ifname, mac[0], mac[1],
           mac[2], mac[3], mac[4], mac[5], n);
    for (unsigned i = 0; i < n; i++) printf(" %d", ids[i]);
    printf("\nsignal poll: %s", poll[4] ? "" : "none (not joined)\n");
    if (poll[4]) printf("%d dBm, tx %d Mb/s, rx %d Mb/s, %d MHz\n", poll[1], poll[2], poll[3], poll[4]);
  }
  br_supplicant_close(s);
  return rc ? 1 : 0;
}

// ---- join: this tool as the guest's driver ----
static br_net *wlan;
static uint8_t our_mac[6];
static volatile uint32_t link_state, radio_state, scan_count, join_status = ~0u, join_code;
static volatile bool scan_done, offered;
static volatile unsigned frames;
static uint8_t offer_ip[4], offer_server[4];

static void from_host(void *ctx, const uint8_t *frame, size_t length)
{
  (void)ctx;
  if (br_wifi_ethertype(frame, length) != OW_ETHERTYPE) {
    frames++;
    // A DHCP reply for us: IPv4, UDP 67 -> 68, BOOTREPLY with our transaction.
    if (length >= 14 + 20 + 8 + 240 && frame[12] == 0x08 && frame[13] == 0x00 && frame[23] == 17) {
      const uint8_t *udp = frame + 14 + (frame[14] & 15) * 4;
      if (udp + 8 + 240 <= frame + length && udp[0] == 0 && udp[1] == 67 && udp[2] == 0 && udp[3] == 68 && udp[8] == 2 &&
          !memcmp(udp + 12, "\x4f\x57\x49\x46", 4) && !offered) {
        memcpy(offer_ip, udp + 8 + 16, 4);
        memcpy(offer_server, frame + 26, 4);
        offered = true;
      }
    }
    return;
  }
  struct ow_header h;
  if (length < 14 + sizeof h) return;
  memcpy(&h, frame + 14, sizeof h);
  const uint8_t *p = frame + 14 + sizeof h;
  if (h.type == OW_STATE && h.length >= sizeof(struct ow_state)) {
    struct ow_state s;
    memcpy(&s, p, sizeof s);
    if (s.link != link_state || s.radio != radio_state)
      printf("  state: radio %u, link %u%s, %d dBm, %u Mb/s, %u MHz, reason %u\n", s.radio, s.link, s.link == OW_LINK_UP ? " (up)" : "", s.rssi_dbm,
             s.tx_rate_mbps, s.bss.freq_mhz, s.reason);
    link_state = s.link;
    radio_state = s.radio;
  } else if (h.type == OW_BSS && h.length >= sizeof(struct ow_bss)) {
    struct ow_bss b;
    memcpy(&b, p, sizeof b);
    print_bss(NULL, &b, p + sizeof b, h.length - sizeof b < b.ie_length ? h.length - sizeof b : b.ie_length);
  } else if (h.type == OW_SCAN_DONE && h.length >= sizeof(struct ow_scan_done)) {
    struct ow_scan_done d;
    memcpy(&d, p, sizeof d);
    printf("  scan done: %u networks (error %d)\n", d.count, d.error);
    scan_count = d.count;
    scan_done = true;
  } else if (h.type == OW_JOIN_RESULT && h.length >= sizeof(struct ow_join_result)) {
    struct ow_join_result r;
    memcpy(&r, p, sizeof r);
    join_code = r.code;
    join_status = r.status;
  }
}

static void to_host(uint16_t type, const void *payload, uint16_t n)
{
  uint8_t frame[256];
  struct ow_header h = { OW_MAGIC, type, n };
  memcpy(frame, ow_address, 6);
  memcpy(frame + 6, our_mac, 6);
  frame[12] = (uint8_t)(OW_ETHERTYPE >> 8);
  frame[13] = (uint8_t)OW_ETHERTYPE;
  memcpy(frame + 14, &h, sizeof h);
  if (n) memcpy(frame + 14 + sizeof h, payload, n);
  (void)br_wlan_send(wlan, frame, 14 + sizeof h + n);
}

static uint16_t ip_sum(const uint8_t *p, size_t n)
{
  uint32_t sum = 0;
  for (size_t i = 0; i + 1 < n; i += 2) sum += (uint32_t)(p[i] << 8 | p[i + 1]);
  while (sum >> 16) sum = (sum & 0xffff) + (sum >> 16);
  return (uint16_t)~sum;
}

static void dhcp_discover(void)
{
  uint8_t f[14 + 20 + 8 + 300];
  memset(f, 0, sizeof f);
  memset(f, 0xff, 6);
  memcpy(f + 6, our_mac, 6);
  f[12] = 0x08;
  uint8_t *ip = f + 14, *udp = ip + 20, *b = udp + 8;
  ip[0] = 0x45; ip[2] = (uint8_t)((sizeof f - 14) >> 8); ip[3] = (uint8_t)(sizeof f - 14); ip[8] = 64; ip[9] = 17;
  memset(ip + 16, 0xff, 4);
  uint16_t sum = ip_sum(ip, 20);
  ip[10] = (uint8_t)(sum >> 8); ip[11] = (uint8_t)sum;
  udp[1] = 68; udp[3] = 67; udp[4] = (uint8_t)((8 + 300) >> 8); udp[5] = (uint8_t)(8 + 300);
  b[0] = 1; b[1] = 1; b[2] = 6;
  memcpy(b + 4, "\x4f\x57\x49\x46", 4);                // the transaction
  b[10] = 0x80;                                        // broadcast the reply
  memcpy(b + 28, our_mac, 6);
  memcpy(b + 236, "\x63\x82\x53\x63", 4);
  memcpy(b + 240, "\x35\x01\x01\x37\x03\x01\x03\x06\xff", 9);    // DISCOVER; ask for the mask, the router, DNS
  int rc = br_wlan_send(wlan, f, sizeof f);
  if (rc) printf("  DHCP discover not sent: %s\n", strerror(-rc));
}

static int join(int argc, char **argv)
{
  const char *ssid = argv[2], *pass = argc > 3 ? argv[3] : "";
  unsigned stay = argc > 4 ? (unsigned)atoi(argv[4]) : 5;
  if (strlen(ssid) > 32 || (*pass && (strlen(pass) < 8 || strlen(pass) > 63))) { printf("a name of at most 32 bytes, a passphrase of 8 to 63\n"); return 1; }
  br_net_open_fn open_fn = br_wlan_prepare("wlan0", "/data/local/tmp", our_mac);     // /data/local/tmp/wifi.state
  if (!open_fn || !(wlan = open_fn(NULL, from_host, NULL))) return 1;
  int rc = 1;
  struct ow_hello hello = { OW_VERSION };
  to_host(OW_HELLO, &hello, sizeof hello);
  for (unsigned i = 0; i < 100 && !radio_state; i++) usleep(100000);
  if (!radio_state) { printf("the bridge did not answer\n"); goto out; }
  sleep(2);                                            // the supplicant: found, or started by the lookup
  to_host(OW_SCAN, NULL, 0);
  for (unsigned i = 0; i < 120 && !scan_done; i++) usleep(100000);
  struct ow_join j = { .security = *pass ? OW_SECURITY_AUTO : OW_SECURITY_OPEN };
  j.ssid_length = (uint8_t)strlen(ssid);
  memcpy(j.ssid, ssid, j.ssid_length);
  j.key_length = (uint8_t)strlen(pass);
  memcpy(j.key, pass, j.key_length);
  to_host(OW_JOIN, &j, sizeof j);
  memset(&j, 0, sizeof j);
  for (unsigned i = 0; i < 320 && join_status == ~0u; i++) usleep(100000);
  if (join_status != OW_JOIN_OK) { printf("join: %s (status %u, 802.11 code %u)\n", join_status == ~0u ? "no result in 32 s" : "failed", join_status, join_code); goto out; }
  printf("join: joined\n");
  for (unsigned i = 0; i < 4 && !offered; i++) { dhcp_discover(); sleep(2); }
  if (offered) printf("DHCP: %u.%u.%u.%u offered by %u.%u.%u.%u\n", offer_ip[0], offer_ip[1], offer_ip[2], offer_ip[3], offer_server[0], offer_server[1],
                      offer_server[2], offer_server[3]);
  else printf("DHCP: no offer in 8 s (%u other frames came from the network)\n", frames);
  sleep(stay);
  rc = offered ? 0 : 1;
out:
  to_host(OW_LEAVE, NULL, 0);
  usleep(500000);
  br_wlan_close();
  return rc;
}

// ---- p2p: what the radio offers for AirDrop's AWDL (docs/hardware.md "AirDrop") ----
// A name that hints at a peer-to-peer link of the vendor's or Google's: AWDL, NAN / Wi-Fi Aware, Pixel's "wonder"
// interface and its mosey_server, Quick Share. [I] hints only: the names are guesses, the nl80211 report is the fact.
static bool p2p_hint(const char *name)
{
  static const char *const words[] = { "awdl", "aware", "nan", "wonder", "mosey", "p2p", "quickshare", "nearby", "llw", NULL };
  char lower[256];
  size_t i = 0;
  for (; name[i] && i + 1 < sizeof lower; i++) lower[i] = (char)(name[i] >= 'A' && name[i] <= 'Z' ? name[i] + 32 : name[i]);
  lower[i] = 0;
  for (const char *const *w = words; *w; w++) if (strstr(lower, *w)) return true;
  return false;
}

static int p2p(int argc, char **argv)
{
  const char *ifname = argc > 2 ? argv[2] : "wlan0";
  static br_wifi_radios radios;
  int rc = br_wlan_radios(&radios);     // every wiphy: Wonder's radio is not wlan0's
  if (rc) { printf("p2p: nl80211: %s\n", strerror(-rc)); return 1; }
  static char text[65536];
  size_t n = br_wifi_radios_report(&radios, ifname, text, sizeof text);
  fputs(text, stdout);
  if (n >= sizeof text) printf("(report cut at %zu of %zu bytes)\n", sizeof text, n);
  // Wonder's pieces on disk and in the kernel (Samsung's binding: a platform driver "wonder" on google,wonder-drv-v1;
  // Google's mosey_server in system_ext from One UI 9). Named paths only.
  static const char *const wonder_paths[] = { "/sys/module/wonder", "/sys/module/wonder/parameters/physical_name", "/sys/bus/platform/drivers/wonder",
                                              "/vendor_dlkm/lib/modules/wonder.ko", "/vendor/lib/modules/wonder.ko", "/system_ext/bin/mosey_server",
                                              "/vendor/bin/mosey_server", NULL };
  for (const char *const *p = wonder_paths; *p; p++) {
    if (access(*p, F_OK)) { printf("wonder: %s: none\n", *p); continue; }
    char value[64] = "";
    FILE *v = strstr(*p, "/parameters/") ? fopen(*p, "r") : NULL;
    if (v) { if (!fgets(value, sizeof value, v)) value[0] = 0; fclose(v); value[strcspn(value, "\n")] = 0; }
    printf("wonder: %s: present%s%s\n", *p, *value ? ": " : "", value);
  }
  // Network devices: one with no wiphy but a hinting name is a virtual one (a tun of a user-space AWDL, Pixel's
  // "wonder" virtual radio).
  DIR *d = opendir("/sys/class/net");
  for (struct dirent *e; d && (e = readdir(d));) {
    if (e->d_name[0] == '.') continue;
    char path[320];
    snprintf(path, sizeof path, "/sys/class/net/%s/phy80211", e->d_name);
    const bool radio = access(path, F_OK) == 0;
    if (radio || p2p_hint(e->d_name)) printf("netdev: %s%s\n", e->d_name, radio ? " (a Wi-Fi radio's)" : " (no Wi-Fi radio: virtual)");
  }
  if (d) closedir(d);
  FILE *f = fopen("/proc/modules", "r");
  char line[512];
  while (f && fgets(line, sizeof line, f)) {
    line[strcspn(line, " ")] = 0;
    if (p2p_hint(line) || strstr(line, "qca") || strstr(line, "cnss")) printf("module: %s\n", line);
  }
  if (f) fclose(f);
  d = opendir("/proc");
  for (struct dirent *e; d && (e = readdir(d));) {
    if (e->d_name[0] < '1' || e->d_name[0] > '9') continue;
    char path[300], cmd[256] = "";
    snprintf(path, sizeof path, "/proc/%s/cmdline", e->d_name);
    FILE *c = fopen(path, "r");
    if (!c) continue;
    const size_t got = fread(cmd, 1, sizeof cmd - 1, c);
    fclose(c);
    cmd[got] = 0;   // the program's path, up to its first NUL
    if (*cmd && p2p_hint(cmd)) printf("process: %s %s\n", e->d_name, cmd);
  }
  if (d) closedir(d);
  return 0;
}

// ---- wonder: a session on Wonder's raw 802.11 path (docs/hardware.md 4a, wifi/wonder.c) ----
static volatile int wonder_stop;
static void wonder_signal(int sig) { (void)sig; wonder_stop = 1; }

static int wonder(int argc, char **argv)
{
  br_wonder_params p = { .station = "wlan0", .name = BR_WONDER_IFNAME, .freq_mhz = 2437, .seconds = 30, .stop = &wonder_stop };
  for (int i = 2; i < argc; i++) {
    const char *v = i + 1 < argc ? argv[i + 1] : NULL;
    if (!strcmp(argv[i], "--seconds") && v) { p.seconds = (unsigned)atoi(v); i++; }
    else if (!strcmp(argv[i], "--freq") && v) { p.freq_mhz = (uint32_t)atoi(v); i++; }
    else if (!strcmp(argv[i], "--country") && v && strlen(v) == 2) { p.country[0] = (char)(v[0] & ~0x20); p.country[1] = (char)(v[1] & ~0x20); i++; }
    else if (!strcmp(argv[i], "--station") && v) { p.station = v; i++; }
    else { printf("usage: wifictl wonder [--seconds 1..600] [--freq 2437|5220|5745] [--country CC] [--station IFACE]\n"); return 2; }
  }
  if (!p.seconds || p.seconds > 600) { printf("wonder: --seconds 1 to 600\n"); return 2; }
  if (geteuid() != 0) { printf("wonder: needs root (su)\n"); return 1; }
  struct sigaction sa;
  memset(&sa, 0, sizeof sa);
  sa.sa_handler = wonder_signal;        // no SA_RESTART: the receive's poll ends at once
  sigaction(SIGINT, &sa, NULL);
  sigaction(SIGTERM, &sa, NULL);
  sigaction(SIGHUP, &sa, NULL);
  signal(SIGPIPE, SIG_IGN);             // adb gone: the lines are lost, the teardown still runs
  void *ctx = br_wlan_wonder_open();
  if (!ctx) { printf("wonder: nl80211 not opened\n"); return 1; }
  static br_wonder_result r;
  const int rc = br_wonder_run(&br_wlan_wonder_ops, ctx, &p, &r);
  br_wlan_wonder_close(ctx);
  printf("wonder: %s: %llu frames, %llu AWDL action frames from %u peer(s); %s %s\n", rc ? strerror(-rc) : "done", (unsigned long long)r.count.frames,
         (unsigned long long)r.count.awdl_action, r.count.peer_count, p.station,
         !r.link_before ? "was not associated before" : r.link_after ? "associated as before" : "LOST ITS LINK");
  return rc ? 1 : 0;
}

int main(int argc, char **argv)
{
  int rc = 2;
  setvbuf(stdout, NULL, _IOLBF, 0);
  br_log_sink = say;
  if (argc < 2) { fprintf(stderr, "usage: wifictl status [IFACE] | scan [SECONDS [IFACE]] | join SSID [PASSPHRASE [SECONDS]] | restore FILE | p2p [IFACE] | wonder [--seconds N] [--freq MHZ] [--country CC] [--station IFACE]\n"); return 2; }
  if (!strcmp(argv[1], "status")) rc = status(argc, argv);
  else if (!strcmp(argv[1], "scan")) rc = scan(argc, argv);
  else if (!strcmp(argv[1], "p2p")) rc = p2p(argc, argv);
  else if (!strcmp(argv[1], "wonder")) rc = wonder(argc, argv);
  else if (!strcmp(argv[1], "join") && argc > 2) rc = join(argc, argv);
  else if (!strcmp(argv[1], "restore") && argc > 2) { rc = br_wlan_restore(argv[2]); if (rc == -ENOENT) { printf("nothing to give back (no %s)\n", argv[2]); rc = 0; } else rc = rc ? 1 : 0; }
  printf("%s\n", rc == 0 ? "ok" : rc == 2 ? "unknown command" : "failed");
  return rc;
}
