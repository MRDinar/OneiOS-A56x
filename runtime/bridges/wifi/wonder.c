// SPDX-License-Identifier: MIT
// `wifictl wonder` (docs/hardware.md 4a): a session on Google's Wonder radio, the raw 802.11 path One UI's Wi-Fi
// driver offers beside wlan0, with AWDL's frames counted. The order is Samsung's Wonder 1.5 (S928BXXU5DZDP,
// vendor/qcom/opensource/wlan/wonder, GPL-2.0): Wonder caches the frequency, the TX rate, the BSSID and the country
// and needs all four before its interface comes up; the interface's up (mac80211's start) calls the provider's init
// in qcacld (wlan_hdd_wondertap.c), which opens the PASSTHRU vdev wondertap0 beside wlan0 and sets the driver's
// country to the one given; its down calls deinit, which closes the vdev.
//   1. wlan0's link before; every radio; refused without Wonder, with Wonder in use (another interface on it, a
//      wondertap vdev up), without wlan0, without a known country, on a frequency that is no AWDL channel
//   2. a monitor interface on Wonder's wiphy (NL80211_CMD_NEW_INTERFACE)
//   3. SET_REGULATORY (the station radio's own country), SET_FREQUENCY (20 MHz), SET_FIXED_TX_RATE (HT MCS 0),
//      SET_FILTER (AWDL's BSSID)
//   4. the interface up; GET_IF_MAC_ADDR (Wonder answers it only when up)
//   5. AF_PACKET on it: frames counted for the seconds asked, AWDL's by kind and by peer
//   6. in reverse, also after any failed step: frames closed, the interface down (deinit), the interface removed
//   7. wlan0's link after, against the link before (-ENOLINK when it was associated and is not again within 10 s)
// The steps are pure over br_wonder_ops (wlan.c has the phone's); tests/test_wifi.c runs them on a stand-in.
#include "wifi.h"
#include "bridges/bridges.h"
#include <errno.h>
#include <stdarg.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>

static const uint8_t apple_oui[3] = { 0x00, 0x17, 0xf2 };
static const uint8_t awdl_bssid[6] = { 0x00, 0x25, 0x00, 0xff, 0x94, 0x73 };   // every AWDL data frame's addr3 (OWL)

static uint32_t le32(const uint8_t *p) { return (uint32_t)p[0] | (uint32_t)p[1] << 8 | (uint32_t)p[2] << 16 | (uint32_t)p[3] << 24; }

// ---- the frames ---------------------------------------------------------------------------------
bool br_wifi_radiotap_frame(const uint8_t *f, size_t n, br_awdl_frame *out)
{
  memset(out, 0, sizeof *out);
  if (n < 8 || f[0] != 0) return false;                 // radiotap version 0
  const size_t rt = (size_t)(f[2] | f[3] << 8);
  if (rt < 8 || rt > n) return false;
  const uint32_t present = le32(f + 4);
  size_t at = 8;
  for (uint32_t p = present; p & 0x80000000u; at += 4) {  // the fields come after every present word
    if (at + 4 > rt) return false;
    p = le32(f + at);
  }
  // The first word's fields 0 to 5, in bit order, each aligned to its size from the header's start: TSFT (u64),
  // FLAGS, RATE, CHANNEL (u16 frequency, u16 flags), FHSS, DBM_ANTSIGNAL (s8).
  static const uint8_t align[6] = { 8, 1, 1, 2, 1, 1 }, size[6] = { 8, 1, 1, 4, 2, 1 };
  uint8_t flags = 0;
  for (unsigned b = 0; b < 6; b++) {
    if (!(present >> b & 1)) continue;
    at = (at + align[b] - 1) & ~(size_t)(align[b] - 1);
    if (at + size[b] > rt) return false;
    if (b == 0) out->tsft = true;
    else if (b == 1) flags = f[at];
    else if (b == 5) { out->signal_dbm = (int8_t)f[at]; out->has_signal = true; }
    at += size[b];
  }
  out->bad_fcs = flags & 0x40;
  const uint8_t *h = f + rt;
  size_t len = n - rt;
  if ((flags & 0x10) && len >= 4) len -= 4;              // the FCS at the end
  if (len < 10) return false;                            // frame control, duration, addr1: the shortest (an ACK)
  out->type = (uint8_t)(h[0] >> 2 & 3);
  if (out->type == 1) return true;                       // control: no source we count
  if (len < 24) return false;
  memcpy(out->source, h + 10, 6);
  const unsigned subtype = h[0] >> 4;
  if (out->type == 0 && (subtype == 13 || subtype == 14)) {   // action, action no-ack
    const uint8_t *body = h + 24;
    const size_t blen = len - 24;
    // AWDL's action header (OWL's struct awdl_action): category 127, Apple's OUI, type 8, version, subtype
    if (blen >= 7 && body[0] == 127 && !memcmp(body + 1, apple_oui, 3) && body[4] == 8) { out->kind = BR_AWDL_ACTION; out->subtype = body[6]; }
    else out->kind = BR_AWDL_OTHER_ACTION;
  } else if (out->type == 2 && !(h[1] & 3) && !memcmp(h + 16, awdl_bssid, 6)) out->kind = BR_AWDL_DATA;
  return true;
}

static void peer(br_awdl_count *c, const br_awdl_frame *f)
{
  unsigned i = 0;
  while (i < c->peer_count && memcmp(c->peers[i].mac, f->source, 6)) i++;
  if (i == c->peer_count) {
    if (i == BR_AWDL_PEERS) { c->peers_dropped++; return; }
    memset(&c->peers[i], 0, sizeof c->peers[i]);
    memcpy(c->peers[i].mac, f->source, 6);
    c->peer_count++;
  }
  if (f->kind == BR_AWDL_ACTION) c->peers[i].action++;
  else c->peers[i].data++;
  if (f->has_signal && (!c->peers[i].has_signal || f->signal_dbm > c->peers[i].best_dbm)) { c->peers[i].best_dbm = f->signal_dbm; c->peers[i].has_signal = true; }
}

void br_awdl_count_add(br_awdl_count *c, const uint8_t *frame, size_t length)
{
  br_awdl_frame f;
  c->frames++;
  if (!br_wifi_radiotap_frame(frame, length, &f)) { c->unparsed++; return; }
  if (f.bad_fcs) { c->bad_fcs++; return; }
  if (f.tsft) c->with_tsft++;
  if (f.type == 0) c->management++;
  else if (f.type == 1) c->control++;
  else if (f.type == 2) c->data++;
  if (f.kind == BR_AWDL_ACTION || f.kind == BR_AWDL_OTHER_ACTION) c->action++;
  if (f.kind == BR_AWDL_ACTION) {
    c->awdl_action++;
    if (f.subtype == 0) c->awdl_psf++;
    else if (f.subtype == 3) c->awdl_mif++;
    else c->awdl_other++;
    peer(c, &f);
  } else if (f.kind == BR_AWDL_DATA) {
    c->awdl_data++;
    peer(c, &f);
  }
}

struct out { char *p; size_t size, used; };
static void put(struct out *o, const char *fmt, ...) __attribute__((format(printf, 2, 3)));
static void put(struct out *o, const char *fmt, ...)
{
  va_list ap;
  va_start(ap, fmt);
  const int n = vsnprintf(o->used < o->size ? o->p + o->used : NULL, o->used < o->size ? o->size - o->used : 0, fmt, ap);
  va_end(ap);
  if (n > 0) o->used += (size_t)n;
}

size_t br_awdl_count_report(const br_awdl_count *c, unsigned seconds, char *text, size_t size)
{
  struct out o = { text, size, 0 };
  if (size) text[0] = 0;
  put(&o, "frames: %llu in %u s: %llu management (%llu action), %llu control, %llu data; %llu not parsed, %llu with a bad FCS; "
          "%llu carry the radiotap TSFT\n",
      (unsigned long long)c->frames, seconds, (unsigned long long)c->management, (unsigned long long)c->action, (unsigned long long)c->control,
      (unsigned long long)c->data, (unsigned long long)c->unparsed, (unsigned long long)c->bad_fcs, (unsigned long long)c->with_tsft);
  put(&o, "AWDL: %llu action frames (%llu PSF, %llu MIF, %llu other subtypes), %llu data frames to AWDL's BSSID, from %u peer(s)%s\n",
      (unsigned long long)c->awdl_action, (unsigned long long)c->awdl_psf, (unsigned long long)c->awdl_mif, (unsigned long long)c->awdl_other,
      (unsigned long long)c->awdl_data, c->peer_count, c->peers_dropped ? " (more were not kept)" : "");
  for (unsigned i = 0; i < c->peer_count; i++) {
    put(&o, "AWDL peer: %02x:%02x:%02x:%02x:%02x:%02x: %u action, %u data", c->peers[i].mac[0], c->peers[i].mac[1], c->peers[i].mac[2],
        c->peers[i].mac[3], c->peers[i].mac[4], c->peers[i].mac[5], c->peers[i].action, c->peers[i].data);
    if (c->peers[i].has_signal) put(&o, ", best %d dBm", c->peers[i].best_dbm);
    put(&o, "\n");
  }
  if (!c->frames) put(&o, "AWDL: no frame at all: Wonder's receive path gave nothing (the vdev's channel, the BSSID filter, the firmware)\n");
  else if (!c->awdl_action) put(&o, "AWDL: no AWDL frame: no Apple device sends AWDL on this channel now (an iPhone in AirDrop \"Everyone for 10 minutes\", its share sheet open)\n");
  const uint64_t read = c->frames - c->unparsed - c->bad_fcs;   // the frames whose radiotap was read and kept
  if (c->with_tsft < read) put(&o, "radiotap: %llu frames without the TSFT (an AWDL engine needs it for the sync)\n", (unsigned long long)(read - c->with_tsft));
  return o.used;
}

// ---- the vendor data ----------------------------------------------------------------------------
static size_t attr(uint8_t *out, size_t size, size_t at, uint16_t type, const void *data, uint16_t len)
{
  const size_t total = 4u + len, step = (total + 3u) & ~(size_t)3u;
  if (at == SIZE_MAX || at + step > size) return SIZE_MAX;
  out[at] = (uint8_t)total; out[at + 1] = (uint8_t)(total >> 8); out[at + 2] = (uint8_t)type; out[at + 3] = (uint8_t)(type >> 8);
  if (len) memcpy(out + at + 4, data, len);
  memset(out + at + total, 0, step - total);
  return at + step;
}
static size_t done(size_t at) { return at == SIZE_MAX ? 0 : at; }

size_t br_wonder_set_frequency(uint8_t *out, size_t size, uint32_t freq_mhz)
{
  const uint16_t bw20 = 0;                               // WONDER_VEN_ATTR_CHANNEL_BW_20
  size_t at = attr(out, size, 0, 1, &freq_mhz, 4);       // CHANNEL_FREQ (u32, MHz)
  return done(attr(out, size, at, 2, &bw20, 2));         // CHANNEL_BANDWIDTH (u16)
}

size_t br_wonder_set_regulatory(uint8_t *out, size_t size, const char country[2])
{
  const char cc[3] = { country[0], country[1], 0 };      // REG_COUNTRY_CODE: NLA_NUL_STRING of at most 2
  return done(attr(out, size, 0, 1, cc, 3));
}

size_t br_wonder_set_fixed_tx_rate(uint8_t *out, size_t size)
{
  const uint32_t preamble = 1, gi = 2;                   // HT; 0.8 us
  const uint16_t bw = 0;                                 // 20 MHz (policy u16; Wonder reads it as u32 over the zero pad)
  const uint8_t nss = 1, mcs = 0;
  size_t at = attr(out, size, 0, 1, &preamble, 4);
  at = attr(out, size, at, 2, &bw, 2);
  at = attr(out, size, at, 3, &gi, 4);
  at = attr(out, size, at, 4, &nss, 1);
  return done(attr(out, size, at, 5, &mcs, 1));
}

size_t br_wonder_set_bssid_filter(uint8_t *out, size_t size, const uint8_t bssid[6])
{
  uint8_t inner[24];
  const uint8_t on = 1;
  size_t n = attr(inner, sizeof inner, 0, 1, &on, 1);    // FILTER_BSSID_ENABLED
  n = attr(inner, sizeof inner, n, 2, bssid, 6);         // FILTER_BSSID_ADDR
  if (n == SIZE_MAX) return 0;
  const uint32_t type = 0;                               // FILTER_TYPE_BSSID
  size_t at = attr(out, size, 0, 1, &type, 4);           // TOP_FILTER_TYPE
  return done(attr(out, size, at, 2 | 0x8000, inner, (uint16_t)n));   // TOP_FILTER_PARAMS, nested
}

bool br_wonder_mac_reply(const uint8_t *p, size_t length, uint8_t mac[6])
{
  while (length >= 4) {
    const size_t len = (size_t)(p[0] | p[1] << 8), type = (size_t)((p[2] | p[3] << 8) & 0x3fff);
    if (len < 4 || len > length) return false;
    if (type == 1 && len == 10) { memcpy(mac, p + 4, 6); return true; }   // IF_ADDR_MAC_ADDR
    const size_t step = (len + 3u) & ~(size_t)3u;
    if (step >= length) return false;
    p += step;
    length -= step;
  }
  return false;
}

// ---- the session --------------------------------------------------------------------------------
bool br_wonder_freq_ok(uint32_t f) { return f == 2437 || f == 5220 || f == 5745; }

static const char *err(int rc) { return rc ? strerror(-rc) : "ok"; }

static void say_link(const char *when, const char *station, int rc, const br_wifi_link *l)
{
  if (rc) br_log(0, "wonder: %s %s: its link not read: %s", station, when, err(rc));
  else if (!l->up) br_log(1, "wonder: %s %s: not associated", station, when);
  else br_log(1, "wonder: %s %s: associated to %02x:%02x:%02x:%02x:%02x:%02x on %u MHz, %d dBm, %u Mb/s", station, when, l->bssid[0], l->bssid[1],
              l->bssid[2], l->bssid[3], l->bssid[4], l->bssid[5], l->freq_mhz, l->rssi_dbm, l->tx_rate_mbps);
}

static const br_wifi_iface *iface_named(const br_wifi_radios *r, const char *name)
{
  for (unsigned i = 0; i < r->iface_count; i++) if (!strcmp(r->ifaces[i].name, name)) return &r->ifaces[i];
  return NULL;
}

static int vendor(const br_wonder_ops *ops, void *ctx, uint32_t ifindex, uint32_t subcmd, const uint8_t *data, size_t length, const char *what)
{
  uint8_t reply[64];
  size_t reply_length = sizeof reply;
  const int rc = ops->vendor(ctx, ifindex, subcmd, data, length, reply, &reply_length);
  br_log(rc ? 0 : 1, "wonder: %s (vendor 00:1a:11 subcommand %u %s): %s", what, subcmd, br_wifi_wonder_subcmd_name(subcmd), err(rc));
  return rc;
}

// The interface down (when it is up) and removed; a failed removal is the session's error.
static void remove_iface(const br_wonder_ops *ops, void *ctx, const char *name, uint32_t ifindex, bool down, int *rc)
{
  int d;
  if (down) {
    d = ops->set_up(ctx, name, false);
    br_log(d ? 0 : 1, "wonder: %s down (Wonder's stop: the provider's deinit closes the PASSTHRU vdev): %s", name, err(d));
  }
  d = ops->del_iface(ctx, ifindex);
  br_log(d ? 0 : 1, "wonder: %s removed (NL80211_CMD_DEL_INTERFACE): %s%s", name, err(d), d ? " (the next `wifictl wonder` removes it)" : "");
  if (d && !*rc) *rc = d;
}

int br_wonder_run(const br_wonder_ops *ops, void *ctx, const br_wonder_params *p, br_wonder_result *r)
{
  static br_wifi_radios radios;                          // about 25 KB: one session per process
  memset(r, 0, sizeof *r);
  const char *station = p->station && *p->station ? p->station : "wlan0", *name = p->name && *p->name ? p->name : BR_WONDER_IFNAME;
  if (!br_wonder_freq_ok(p->freq_mhz)) {
    br_log(0, "wonder: refused: %u MHz is no AWDL channel (2437: channel 6, 5220: 44, 5745: 149)", p->freq_mhz);
    return -EINVAL;
  }
  int rc = ops->link(ctx, station, &r->before);
  say_link("before", station, rc, &r->before);
  r->link_before = !rc && r->before.up;

  // ---- every radio, and the refusals
  memset(&radios, 0, sizeof radios);
  if ((rc = ops->radios(ctx, &radios))) { br_log(0, "wonder: refused: the radios not read (nl80211 GET_WIPHY, GET_INTERFACE): %s", err(rc)); return rc; }
  const br_wifi_caps *w = br_wifi_wonder(&radios);
  if (!w) {
    br_log(0, "wonder: refused: no Wonder radio (no wiphy named wonder, none with vendor commands under 00:1a:11): `wifictl p2p` lists the radios");
    return -ENODEV;
  }
  const uint32_t wiphy = w->wiphy;
  const bool has_filter = br_wifi_caps_has_vendor(w, BR_WIFI_OUI_GOOGLE, 2), has_rate = br_wifi_caps_has_vendor(w, BR_WIFI_OUI_GOOGLE, 3);
  br_log(1, "wonder: radio: wiphy %u (%s)%s%s", wiphy, w->name[0] ? w->name : "?", has_filter ? "" : ", no SET_FILTER", has_rate ? "" : ", no SET_FIXED_TX_RATE");
  if (!br_wifi_caps_has_vendor(w, BR_WIFI_OUI_GOOGLE, 1) || !br_wifi_caps_has_vendor(w, BR_WIFI_OUI_GOOGLE, 4) || !br_wifi_caps_has_vendor(w, BR_WIFI_OUI_GOOGLE, 5)) {
    br_log(0, "wonder: refused: wiphy %u lacks SET_FREQUENCY, SET_REGULATORY or GET_IF_MAC_ADDR: not a Wonder this tool knows", wiphy);
    return -EOPNOTSUPP;
  }
  const br_wifi_iface *old = iface_named(&radios, name);
  if (old && old->wiphy == wiphy) {                      // a session of ours that did not end (a kill): ours to remove
    br_log(1, "wonder: %s (index %u) is left from a session that did not end: removed first", name, old->ifindex);
    int left = 0;
    remove_iface(ops, ctx, name, old->ifindex, true, &left);
    if (left) return left;
    memset(&radios, 0, sizeof radios);
    if ((rc = ops->radios(ctx, &radios))) { br_log(0, "wonder: refused: the radios not read again: %s", err(rc)); return rc; }
    if (!(w = br_wifi_wonder(&radios))) { br_log(0, "wonder: refused: the Wonder radio is gone"); return -ENODEV; }
  }
  for (unsigned i = 0; i < radios.iface_count; i++) {
    const br_wifi_iface *f = &radios.ifaces[i];
    if (f->wiphy == wiphy || !strncmp(f->name, "wondertap", 9)) {
      br_log(0, "wonder: refused: Wonder is in use: %s (%s, wiphy %u) is up (Quick Share's \"Share with Apple devices\"? try again after it)",
             f->name[0] ? f->name : "(no netdev)", br_wifi_iftype_name(f->iftype), f->wiphy);
      return -EBUSY;
    }
  }
  const br_wifi_iface *sta = iface_named(&radios, station);
  if (!sta) { br_log(0, "wonder: refused: no %s (One UI's Wi-Fi is off?): Wonder's provider runs in its driver", station); return -ENODEV; }
  char cc[3] = "";
  rc = ops->country(ctx, sta->wiphy, cc);
  const bool known = !rc && cc[0] >= 'A' && cc[0] <= 'Z' && cc[1] >= 'A' && cc[1] <= 'Z';
  if (p->country[0]) {
    if (known && (cc[0] != p->country[0] || cc[1] != p->country[1])) {
      br_log(0, "wonder: refused: the driver's country is %s, --country %s would change it (Wonder's init sets the driver's country)", cc, p->country);
      return -EINVAL;
    }
    memcpy(cc, p->country, 2);
    cc[2] = 0;
  } else if (!known) {
    br_log(0, "wonder: refused: the driver's country is not known (%s%s%s): give --country XX, the phone's own (Wonder's init sets the driver's to it)",
           rc ? err(rc) : "", rc ? "" : "\"", rc ? "" : cc);
    return -EINVAL;
  }
  br_log(1, "wonder: country %s (the driver's own)%s", cc, p->country[0] ? " (as --country)" : "");
  if (r->link_before && r->before.freq_mhz && r->before.freq_mhz != p->freq_mhz)
    br_log(1, "wonder: %s is on %u MHz, Wonder on %u MHz: the radio shares its time between them (MCC)%s", station, r->before.freq_mhz, p->freq_mhz,
           (r->before.freq_mhz >= 5000) == (p->freq_mhz >= 5000) ? ", on one band" : "");

  // ---- the session
  uint32_t ifindex = 0;
  int handle = -1;
  bool up = false;
  rc = ops->new_monitor(ctx, wiphy, name, &ifindex);
  br_log(rc ? 0 : 1, "wonder: %s: a monitor interface on wiphy %u (NL80211_CMD_NEW_INTERFACE): %s", name, wiphy, err(rc));
  if (rc) goto after;
  br_log(1, "wonder: %s is index %u", name, ifindex);
  uint8_t data[64];
  size_t n = br_wonder_set_regulatory(data, sizeof data, cc);
  if ((rc = vendor(ops, ctx, ifindex, 4, data, n, "country"))) goto teardown;
  n = br_wonder_set_frequency(data, sizeof data, p->freq_mhz);
  char what[48];
  snprintf(what, sizeof what, "%u MHz, 20 MHz wide", p->freq_mhz);
  if ((rc = vendor(ops, ctx, ifindex, 1, data, n, what))) goto teardown;
  if (has_rate) {
    n = br_wonder_set_fixed_tx_rate(data, sizeof data);
    if ((rc = vendor(ops, ctx, ifindex, 3, data, n, "TX rate HT MCS 0, 1 stream, 20 MHz, 0.8 us"))) goto teardown;
  }
  if (has_filter) {
    n = br_wonder_set_bssid_filter(data, sizeof data, awdl_bssid);
    if ((rc = vendor(ops, ctx, ifindex, 2, data, n, "BSSID filter 00:25:00:ff:94:73 (AWDL's)"))) goto teardown;
  }
  rc = ops->set_up(ctx, name, true);
  br_log(rc ? 0 : 1, "wonder: %s up (Wonder's start: the provider's init opens the PASSTHRU vdev): %s%s", name, err(rc),
         rc == -EBUSY ? " (a connection or a roam of wlan0 in progress, or Wonder already up)" : "");
  if (rc) goto teardown;
  up = true;
  uint8_t reply[64];
  size_t reply_length = sizeof reply;
  int m = ops->vendor(ctx, ifindex, 5, NULL, 0, reply, &reply_length);
  r->have_mac = !m && br_wonder_mac_reply(reply, reply_length, r->mac);
  if (r->have_mac) br_log(1, "wonder: the PASSTHRU vdev's address %02x:%02x:%02x:%02x:%02x:%02x (GET_IF_MAC_ADDR)", r->mac[0], r->mac[1], r->mac[2], r->mac[3], r->mac[4], r->mac[5]);
  else br_log(0, "wonder: GET_IF_MAC_ADDR: %s", m ? err(m) : "no address in the reply");
  memset(&radios, 0, sizeof radios);
  if (!ops->radios(ctx, &radios))
    for (unsigned i = 0; i < radios.iface_count; i++) {
      const br_wifi_iface *f = &radios.ifaces[i];
      if (!strncmp(f->name, "wondertap", 9) || f->wiphy == wiphy)
        br_log(1, "wonder: up: %s (index %u) %s, wiphy %u, %u MHz", f->name[0] ? f->name : "(no netdev)", f->ifindex, br_wifi_iftype_name(f->iftype), f->wiphy, f->freq_mhz);
    }
  handle = ops->open_frames(ctx, ifindex);
  if (handle < 0) { rc = handle; br_log(0, "wonder: AF_PACKET on %s: %s", name, err(rc)); goto teardown; }
  r->opened = true;
  br_log(1, "wonder: counting %s's frames for %u s", name, p->seconds);
  static uint8_t frame[8192];
  const uint64_t start = ops->now_ms(ctx), end = start + (uint64_t)p->seconds * 1000u;
  for (uint64_t now = start; now < end && !(p->stop && *p->stop); now = ops->now_ms(ctx)) {
    const uint64_t left = end - now;
    const int got = ops->receive(ctx, handle, frame, sizeof frame, left < 500 ? (unsigned)left : 500u);
    if (got == -EINTR || got == -EAGAIN) continue;
    if (got < 0) { rc = got; br_log(0, "wonder: receive on %s: %s", name, err(rc)); break; }
    if (got > 0) br_awdl_count_add(&r->count, frame, (size_t)got);
  }
  if (p->stop && *p->stop) br_log(1, "wonder: stopped by a signal after %llu ms", (unsigned long long)(ops->now_ms(ctx) - start));
  static char text[4096];
  const unsigned seconds = (unsigned)((ops->now_ms(ctx) - start + 500) / 1000);
  br_awdl_count_report(&r->count, seconds, text, sizeof text);
  for (char *line = text, *nl; *line; line = nl + 1) {
    if (!(nl = strchr(line, '\n'))) break;
    *nl = 0;
    br_log(1, "wonder: %s", line);
  }

teardown:
  if (handle >= 0) { ops->close_frames(ctx, handle); br_log(1, "wonder: AF_PACKET on %s closed", name); }
  remove_iface(ops, ctx, name, ifindex, up, &rc);

after:;
  // ---- wlan0 as it was
  int l = ops->link(ctx, station, &r->after);
  for (unsigned i = 0; r->link_before && (l || !r->after.up) && i < 10; i++) {
    ops->pause_ms(ctx, 1000);
    l = ops->link(ctx, station, &r->after);
  }
  say_link("after", station, l, &r->after);
  r->link_after = !l && r->after.up;
  if (r->link_before && !r->link_after) {
    br_log(0, "wonder: %s lost its link (associated before, not 10 s after the end): One UI's Wi-Fi joins again by itself; say this in the report", station);
    return rc ? rc : -ENOLINK;
  }
  if (r->link_before && memcmp(r->before.bssid, r->after.bssid, 6)) br_log(1, "wonder: %s is associated to another access point than before (a roam)", station);
  else if (r->link_before) br_log(1, "wonder: %s: the same link as before", station);
  return rc;
}
