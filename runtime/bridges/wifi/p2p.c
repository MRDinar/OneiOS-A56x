// SPDX-License-Identifier: MIT
// What the phone's Wi-Fi radios offer for a peer-to-peer link, as nl80211 reports them (docs/hardware.md "AirDrop"):
// the interface types and the combinations of them, the action frames a station may send and receive, off-channel
// sends, Wi-Fi Aware (NAN), Wi-Fi Direct, the vendor's commands, every radio (wiphy) and every interface. AirDrop runs
// on AWDL, Apple's own peer-to-peer link; on this phone it can come from Google's Wonder radio (a soft-MAC over the
// Wi-Fi driver's PASSTHRU vdev, vendor commands under 00:1a:11: docs/hardware.md 4a), from a monitor interface with
// injection beside the station (OWL's way), or from the vendor's own AWDL. `wifictl p2p` prints this report on the
// phone; the parser and the report are pure functions (tests/test_wifi.c).
#include "wifi.h"
#include <stdarg.h>
#include <stdio.h>
#include <string.h>

// nl80211's numbers (linux/nl80211.h of NDK r28c), so that this file needs no Linux header.
enum {
  A_WIPHY = 1, A_WIPHY_NAME = 2, A_IFINDEX = 3, A_IFNAME = 4, A_IFTYPE = 5, A_MAC = 6, A_SUPPORTED_IFTYPES = 32,
  A_WIPHY_FREQ = 38, A_SUPPORTED_COMMANDS = 50, A_FRAME_TYPE = 101, A_TX_FRAME_TYPES = 99, A_RX_FRAME_TYPES = 100,
  A_OFFCHANNEL_TX_OK = 108, A_MAX_ROC_DURATION = 111, A_INTERFACE_COMBINATIONS = 120, A_SOFTWARE_IFTYPES = 121,
  A_FEATURE_FLAGS = 143, A_VENDOR_DATA = 197, A_VENDOR_EVENTS = 198, A_EXT_FEATURES = 217,
  COMB_LIMITS = 1, COMB_MAXNUM = 2, COMB_NUM_CHANNELS = 4, LIMIT_MAX = 1, LIMIT_TYPES = 2,
  CMD_REMAIN_ON_CHANNEL = 55, CMD_FRAME = 59, CMD_START_P2P_DEVICE = 89, CMD_VENDOR = 103, CMD_START_NAN = 115,
  FEATURE_ACTIVE_MONITOR = 1 << 17,
  EXT_SECURE_NAN = 63,
  FC_ACTION = 0x00d0,   // a management frame of subtype 13 (action), as nl80211 writes the frame control field
};
enum { IF_STATION = 2, IF_AP = 3, IF_MONITOR = 6, IF_P2P_CLIENT = 8, IF_P2P_GO = 9, IF_P2P_DEVICE = 10, IF_NAN = 12 };

static const char *const iftype_names[BR_WIFI_IFTYPES] = {
  "unspecified", "ad-hoc", "station", "access point", "ap-vlan", "wds", "monitor", "mesh", "p2p-client", "p2p-go",
  "p2p-device", "ocb", "nan",
};
const char *br_wifi_iftype_name(uint32_t type) { return type < BR_WIFI_IFTYPES ? iftype_names[type] : "?"; }

static uint32_t u32_of(const uint8_t *p) { uint32_t v; memcpy(&v, p, 4); return v; }

// Calls fn for each attribute of a payload (netlink's 4-byte alignment); stops at a cut.
typedef void (*each_fn)(void *ctx, uint16_t type, const uint8_t *data, uint16_t size);
static void each(const uint8_t *p, size_t length, each_fn fn, void *ctx)
{
  while (length >= 4) {
    const uint16_t len = (uint16_t)(p[0] | p[1] << 8), type = (uint16_t)((p[2] | p[3] << 8) & 0x3fff);
    if (len < 4 || len > length) return;
    fn(ctx, type, p + 4, (uint16_t)(len - 4));
    const size_t step = (len + 3u) & ~3u;
    if (step >= length) return;
    p += step;
    length -= step;
  }
}

// A set of interface types: one flag attribute per type (the attribute's type is the interface type).
static void type_flag(void *ctx, uint16_t type, const uint8_t *data, uint16_t size)
{
  (void)data; (void)size;
  if (type < 32) *(uint32_t *)ctx |= 1u << type;
}

struct limit_ctx { br_wifi_combination *c; uint32_t types, max; };
static void limit_attr(void *ctx, uint16_t type, const uint8_t *data, uint16_t size)
{
  struct limit_ctx *l = ctx;
  if (type == LIMIT_MAX && size == 4) l->max = u32_of(data);
  if (type == LIMIT_TYPES) each(data, size, type_flag, &l->types);
}
static void limit_entry(void *ctx, uint16_t type, const uint8_t *data, uint16_t size)
{
  br_wifi_combination *c = ctx;
  (void)type;
  struct limit_ctx l = { c, 0, 0 };
  each(data, size, limit_attr, &l);
  if (c->limit_count < BR_WIFI_COMB_LIMITS) { c->limit_types[c->limit_count] = l.types; c->limit_max[c->limit_count] = l.max; c->limit_count++; }
}
static void comb_attr(void *ctx, uint16_t type, const uint8_t *data, uint16_t size)
{
  br_wifi_combination *c = ctx;
  if (type == COMB_LIMITS) each(data, size, limit_entry, c);
  else if (type == COMB_MAXNUM && size == 4) c->max_interfaces = u32_of(data);
  else if (type == COMB_NUM_CHANNELS && size == 4) c->channels = u32_of(data);
}
static void comb_entry(void *ctx, uint16_t type, const uint8_t *data, uint16_t size)
{
  br_wifi_caps *caps = ctx;
  (void)type;
  if (caps->combination_count >= BR_WIFI_COMBINATIONS) return;
  br_wifi_combination *c = &caps->combinations[caps->combination_count];
  memset(c, 0, sizeof *c);
  each(data, size, comb_attr, c);
  caps->combination_count++;
}

// TX_FRAME_TYPES and RX_FRAME_TYPES: one nested attribute per interface type, holding the frame control values.
struct frames_ctx { uint32_t *mask; uint16_t iftype; };
static void frame_value(void *ctx, uint16_t type, const uint8_t *data, uint16_t size)
{
  struct frames_ctx *f = ctx;
  if (type == A_FRAME_TYPE && size == 2) {
    const uint16_t fc = (uint16_t)(data[0] | data[1] << 8);
    if (fc == FC_ACTION && f->iftype < 32) *f->mask |= 1u << f->iftype;
  }
}
static void frames_entry(void *ctx, uint16_t type, const uint8_t *data, uint16_t size)
{
  struct frames_ctx f = { ctx, type };
  each(data, size, frame_value, &f);
}

static void command_entry(void *ctx, uint16_t type, const uint8_t *data, uint16_t size)
{
  br_wifi_caps *c = ctx;
  (void)type;
  if (size != 4) return;
  const uint32_t cmd = u32_of(data);
  if (cmd == CMD_FRAME) c->cmd_frame = true;
  else if (cmd == CMD_REMAIN_ON_CHANNEL) c->cmd_remain_on_channel = true;
  else if (cmd == CMD_START_NAN) c->cmd_start_nan = true;
  else if (cmd == CMD_START_P2P_DEVICE) c->cmd_start_p2p_device = true;
  else if (cmd == CMD_VENDOR) c->cmd_vendor = true;
}

// VENDOR_DATA and VENDOR_EVENTS: each entry a struct nl80211_vendor_cmd_info { u32 vendor_id; u32 subcmd; }.
struct vendor_ctx { br_wifi_vendor_cmd *list; unsigned *count; };
static void vendor_entry(void *ctx, uint16_t type, const uint8_t *data, uint16_t size)
{
  struct vendor_ctx *v = ctx;
  (void)type;
  if (size < 8) return;
  const br_wifi_vendor_cmd e = { u32_of(data), u32_of(data + 4) };
  for (unsigned i = 0; i < *v->count; i++) if (v->list[i].oui == e.oui && v->list[i].subcmd == e.subcmd) return;   // a split dump repeats nothing, but be safe
  if (*v->count < BR_WIFI_VENDOR_CMDS) v->list[(*v->count)++] = e;
}

static void wiphy_attr(void *ctx, uint16_t type, const uint8_t *data, uint16_t size)
{
  br_wifi_caps *c = ctx;
  switch (type) {
  case A_WIPHY: if (size == 4) { c->wiphy = u32_of(data); c->have_wiphy = true; } break;
  case A_WIPHY_NAME: snprintf(c->name, sizeof c->name, "%.*s", (int)(size && !data[size - 1] ? size - 1 : size), (const char *)data); break;
  case A_SUPPORTED_IFTYPES: each(data, size, type_flag, &c->iftypes); break;
  case A_SOFTWARE_IFTYPES: each(data, size, type_flag, &c->software_iftypes); break;
  case A_INTERFACE_COMBINATIONS: each(data, size, comb_entry, c); break;
  case A_TX_FRAME_TYPES: each(data, size, frames_entry, &c->action_tx); break;
  case A_RX_FRAME_TYPES: each(data, size, frames_entry, &c->action_rx); break;
  case A_OFFCHANNEL_TX_OK: c->offchannel_tx = true; break;
  case A_MAX_ROC_DURATION: if (size == 4) c->max_roc_ms = u32_of(data); break;
  case A_SUPPORTED_COMMANDS: each(data, size, command_entry, c); break;
  case A_FEATURE_FLAGS: if (size == 4) c->features = u32_of(data); break;
  case A_EXT_FEATURES: c->ext_length = size < sizeof c->ext_features ? size : (uint16_t)sizeof c->ext_features; memcpy(c->ext_features, data, c->ext_length); break;
  case A_VENDOR_DATA: { struct vendor_ctx v = { c->commands, &c->command_count }; each(data, size, vendor_entry, &v); break; }
  case A_VENDOR_EVENTS: { struct vendor_ctx v = { c->events, &c->event_count }; each(data, size, vendor_entry, &v); break; }
  default: break;
  }
}

void br_wifi_caps_wiphy(br_wifi_caps *c, const uint8_t *attrs, size_t length) { each(attrs, length, wiphy_attr, c); }

static void iface_attr(void *ctx, uint16_t type, const uint8_t *data, uint16_t size)
{
  br_wifi_iface *i = ctx;
  if (type == A_IFNAME) snprintf(i->name, sizeof i->name, "%.*s", (int)(size && !data[size - 1] ? size - 1 : size), (const char *)data);
  else if (type == A_IFINDEX && size == 4) i->ifindex = u32_of(data);
  else if (type == A_WIPHY && size == 4) i->wiphy = u32_of(data);
  else if (type == A_IFTYPE && size == 4) i->iftype = u32_of(data);
  else if (type == A_MAC && size == 6) memcpy(i->mac, data, 6);
  else if (type == A_WIPHY_FREQ && size == 4) i->freq_mhz = u32_of(data);
}

void br_wifi_caps_iface(br_wifi_caps *c, const uint8_t *attrs, size_t length)
{
  br_wifi_iface i;
  memset(&i, 0, sizeof i);
  each(attrs, length, iface_attr, &i);
  if (!i.iftype && !i.ifindex && !i.name[0]) return;
  if (c->iface_count < BR_WIFI_IFACES) c->ifaces[c->iface_count++] = i;
}

// ---- the report -------------------------------------------------------------------------------
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
static void types(struct out *o, uint32_t mask)
{
  bool first = true;
  for (uint32_t t = 0; t < BR_WIFI_IFTYPES; t++) if (mask >> t & 1) { put(o, "%s%s", first ? "" : ", ", iftype_names[t]); first = false; }
  if (first) put(o, "none");
}
static const char *oui_name(uint32_t oui)
{
  switch (oui) {
  case 0x001374: return "Qualcomm";
  case 0x001018: return "Broadcom";
  case 0x001a11: return "Google";
  case 0x0017f2: return "Apple";
  case 0x0000f0: return "Samsung";
  case 0x001632: return "Samsung";
  case 0x506f9a: return "Wi-Fi Alliance";
  default: return "";
  }
}

// Two interface types can be up at the same time: a combination allows two interfaces, and a sits in one limit and
// b in another (each limit allowing one or more), or both in one limit that allows two or more.
bool br_wifi_caps_combines(const br_wifi_caps *c, uint32_t a, uint32_t b)
{
  for (unsigned i = 0; i < c->combination_count; i++) {
    const br_wifi_combination *k = &c->combinations[i];
    if (k->max_interfaces < 2) continue;
    for (unsigned la = 0; la < k->limit_count; la++)
      for (unsigned lb = 0; lb < k->limit_count; lb++) {
        if (!(k->limit_types[la] >> a & 1) || !(k->limit_types[lb] >> b & 1)) continue;
        if (la != lb ? k->limit_max[la] >= 1 && k->limit_max[lb] >= 1 : k->limit_max[la] >= 2) return true;
      }
  }
  return false;
}

// The facts of one radio, from its name to its vendor events.
static void facts(struct out *o, const br_wifi_caps *c)
{
  put(o, "radio: %s (wiphy %u)\n", c->name[0] ? c->name : "?", c->wiphy);
  put(o, "interface types: "); types(o, c->iftypes); put(o, "\n");
  if (c->software_iftypes) { put(o, "software interface types: "); types(o, c->software_iftypes); put(o, "\n"); }
  for (unsigned i = 0; i < c->combination_count; i++) {
    const br_wifi_combination *k = &c->combinations[i];
    put(o, "combination %u: up to %u interfaces on %u channel(s):", i + 1, k->max_interfaces, k->channels);
    for (unsigned l = 0; l < k->limit_count; l++) { put(o, "%s %u of {", l ? ";" : "", k->limit_max[l]); types(o, k->limit_types[l]); put(o, "}"); }
    put(o, "\n");
  }
  put(o, "action frames: sent by "); types(o, c->action_tx); put(o, "; received by "); types(o, c->action_rx); put(o, "\n");
  put(o, "off-channel sends: %s; remain on channel: %s", c->offchannel_tx ? "yes" : "no", c->cmd_remain_on_channel ? "yes" : "no");
  if (c->max_roc_ms) put(o, " (up to %u ms)", c->max_roc_ms);
  put(o, "\n");
  put(o, "vendor commands: %u, vendor events: %u\n", c->command_count, c->event_count);
  for (unsigned i = 0; i < c->command_count; i++) {
    if (!i || c->commands[i].oui != c->commands[i - 1].oui) put(o, "%s  %06x %s:", i ? "\n" : "", c->commands[i].oui, oui_name(c->commands[i].oui));
    put(o, " %u", c->commands[i].subcmd);
  }
  if (c->command_count) put(o, "\n");
  for (unsigned i = 0; i < c->event_count; i++) {
    if (!i || c->events[i].oui != c->events[i - 1].oui) put(o, "%s  events %06x %s:", i ? "\n" : "", c->events[i].oui, oui_name(c->events[i].oui));
    put(o, " %u", c->events[i].subcmd);
  }
  if (c->event_count) put(o, "\n");
}

static void iface_line(struct out *o, const br_wifi_iface *f, const br_wifi_caps *wonder)
{
  put(o, "interface: %s (index %u) %s, wiphy %u, %02x:%02x:%02x:%02x:%02x:%02x", f->name[0] ? f->name : "(no netdev)", f->ifindex,
      br_wifi_iftype_name(f->iftype), f->wiphy, f->mac[0], f->mac[1], f->mac[2], f->mac[3], f->mac[4], f->mac[5]);
  if (f->freq_mhz) put(o, ", %u MHz", f->freq_mhz);
  if (wonder && f->wiphy == wonder->wiphy) put(o, " (on Wonder's radio)");
  else if (!strncmp(f->name, "wondertap", 9)) put(o, " (the Wi-Fi driver's PASSTHRU vdev for Wonder)");
  put(o, "\n");
}

bool br_wifi_caps_has_vendor(const br_wifi_caps *c, uint32_t oui, uint32_t subcmd)
{
  for (unsigned i = 0; i < c->command_count; i++) if (c->commands[i].oui == oui && c->commands[i].subcmd == subcmd) return true;
  return false;
}

// The verdicts: each way an AWDL link could be made on the station's radio, and Wonder's path when there is one.
static void verdicts(struct out *o, const br_wifi_caps *c, const br_wifi_caps *wonder)
{
  const bool monitor = c->iftypes >> IF_MONITOR & 1;
  const bool monitor_beside = monitor && br_wifi_caps_combines(c, IF_STATION, IF_MONITOR);
  put(o, "p2p: monitor mode: %s", monitor ? "yes" : "no");
  if (monitor) put(o, " (beside the station: %s; active monitor, with ACKs: %s; frame injection is not reported by nl80211)",
                   monitor_beside ? "yes" : "no", c->features & FEATURE_ACTIVE_MONITOR ? "yes" : "no");
  put(o, "\n");
  const bool sta_tx = c->action_tx >> IF_STATION & 1, sta_rx = c->action_rx >> IF_STATION & 1;
  put(o, "p2p: AWDL's action frames (vendor action, Apple's 00:17:f2) on the station: send %s, receive %s%s\n", sta_tx && c->cmd_frame ? "yes" : "no",
      sta_rx ? "yes" : "no", sta_tx && c->offchannel_tx ? ", off channel too" : "");
  const bool ext_secure_nan = c->ext_length > EXT_SECURE_NAN / 8 && (c->ext_features[EXT_SECURE_NAN / 8] >> (EXT_SECURE_NAN % 8) & 1);
  put(o, "p2p: Wi-Fi Aware (NAN) through nl80211: %s%s (the vendor's NAN goes through its own HAL and vendor commands, not this)\n",
      (c->iftypes >> IF_NAN & 1) && c->cmd_start_nan ? "yes" : "no", ext_secure_nan ? ", secure NAN" : "");
  put(o, "p2p: Wi-Fi Direct: %s (AirDrop does not use it)\n", (c->iftypes >> IF_P2P_GO & 1) && (c->iftypes >> IF_P2P_CLIENT & 1) ? "yes" : "no");
  bool apple = false;
  for (unsigned i = 0; i < c->command_count; i++) apple |= c->commands[i].oui == 0x0017f2;
  if (apple) put(o, "p2p: the driver has vendor commands under Apple's OUI 00:17:f2 [I: an AWDL interface of the vendor's]\n");
  if (wonder)
    put(o, "p2p: for AirDrop iOS needs AWDL: Wonder's raw 802.11 path is offered (wiphy %u): an AWDL engine on the host can run on a "
           "monitor interface of it beside wlan0 (`wifictl wonder` opens one and counts the AWDL frames)\n", wonder->wiphy);
  else
    put(o, "p2p: for AirDrop iOS needs AWDL: %s\n",
        monitor_beside ? "a monitor interface beside the station is offered (OWL's way, if the driver injects frames); no Wonder radio"
                       : "no Wonder radio and no monitor beside the station: only the vendor's own AWDL can carry it (compare this report with "
                         "Quick Share's \"Share with Apple devices\" on and off: Wonder may come only with it)");
}

size_t br_wifi_caps_report(const br_wifi_caps *c, char *text, size_t size)
{
  struct out o = { text, size, 0 };
  if (size) text[0] = 0;
  facts(&o, c);
  for (unsigned i = 0; i < c->iface_count; i++) iface_line(&o, &c->ifaces[i], NULL);
  verdicts(&o, c, NULL);
  return o.used;
}

// ---- every radio --------------------------------------------------------------------------------
static void find_wiphy(void *ctx, uint16_t type, const uint8_t *data, uint16_t size)
{
  int64_t *id = ctx;
  if (type == A_WIPHY && size == 4 && *id < 0) *id = u32_of(data);
}

void br_wifi_radios_wiphy(br_wifi_radios *r, const uint8_t *attrs, size_t length)
{
  int64_t id = -1;
  each(attrs, length, find_wiphy, &id);
  unsigned i = 0;
  if (id < 0) {                              // a message that names no wiphy: the last one's (never seen from cfg80211)
    if (!r->count) return;
    i = r->count - 1;
  } else {
    while (i < r->count && r->wiphys[i].wiphy != (uint32_t)id) i++;
    if (i == r->count) {
      if (r->count == BR_WIFI_WIPHYS) { r->wiphys_dropped++; return; }
      memset(&r->wiphys[i], 0, sizeof r->wiphys[i]);
      r->count++;
    }
  }
  br_wifi_caps_wiphy(&r->wiphys[i], attrs, length);
}

void br_wifi_radios_iface(br_wifi_radios *r, const uint8_t *attrs, size_t length)
{
  br_wifi_iface f;
  memset(&f, 0, sizeof f);
  each(attrs, length, iface_attr, &f);
  if (!f.iftype && !f.ifindex && !f.name[0]) return;
  if (r->iface_count < BR_WIFI_IFACES) r->ifaces[r->iface_count++] = f;
}

const br_wifi_caps *br_wifi_wonder(const br_wifi_radios *r)
{
  for (unsigned i = 0; i < r->count; i++) if (!strncmp(r->wiphys[i].name, "wonder", 6)) return &r->wiphys[i];
  for (unsigned i = 0; i < r->count; i++) {
    const br_wifi_caps *c = &r->wiphys[i];
    bool google = false;
    for (unsigned k = 0; k < c->command_count; k++) google |= c->commands[k].oui == BR_WIFI_OUI_GOOGLE;
    if (google && (c->iftypes >> IF_MONITOR & 1) && !(c->iftypes >> IF_AP & 1)) return c;
  }
  return NULL;
}

const char *br_wifi_wonder_subcmd_name(uint32_t subcmd)
{
  static const char *const names[] = { "UNSPEC", "SET_FREQUENCY", "SET_FILTER", "SET_FIXED_TX_RATE", "SET_REGULATORY", "GET_IF_MAC_ADDR",
                                       "SET_CHANNEL_SCHEDULE_REQ", "GET_MAC_TSF", "GET_CAP", "GET_CHANNEL_STATUS_REPORT", "SET_STATION_INFO",
                                       "SET_FEATURES", "?", "?", "?", "SET_TX_RATE_TEST" };
  return subcmd < sizeof names / sizeof names[0] ? names[subcmd] : "?";
}

// What Wonder offers and who uses it now.
static void wonder_block(struct out *o, const br_wifi_radios *r, const br_wifi_caps *w)
{
  const br_wifi_iface *tap = NULL;
  for (unsigned i = 0; i < r->iface_count; i++) if (!strncmp(r->ifaces[i].name, "wondertap", 9)) tap = &r->ifaces[i];
  if (!w) {
    put(o, "p2p: Wonder: no Wonder radio (no wiphy named wonder, none with vendor commands under Google's 00:1a:11 and monitor mode "
           "without an access point)%s\n", tap ? "; but a wondertap interface is up: its Wonder radio is not in the dump [U]" : "");
    return;
  }
  put(o, "p2p: Wonder: wiphy %u (%s), interface types ", w->wiphy, w->name[0] ? w->name : "?");
  types(o, w->iftypes);
  put(o, "\n");
  unsigned top = 0, n = 0;
  put(o, "p2p: Wonder: vendor subcommands under 00:1a:11:");
  for (unsigned i = 0; i < w->command_count; i++)
    if (w->commands[i].oui == BR_WIFI_OUI_GOOGLE) {
      put(o, " %u %s", w->commands[i].subcmd, br_wifi_wonder_subcmd_name(w->commands[i].subcmd));
      if (w->commands[i].subcmd > top) top = w->commands[i].subcmd;
      n++;
    }
  put(o, "%s\n", n ? "" : " none");
  const bool basic = br_wifi_caps_has_vendor(w, BR_WIFI_OUI_GOOGLE, 1) && br_wifi_caps_has_vendor(w, BR_WIFI_OUI_GOOGLE, 4) &&
                     br_wifi_caps_has_vendor(w, BR_WIFI_OUI_GOOGLE, 5);
  put(o, "p2p: Wonder: version %s\n",
      !basic ? "unknown: SET_FREQUENCY, SET_REGULATORY or GET_IF_MAC_ADDR is missing (`wifictl wonder` refuses it)"
      : top >= 6 ? "1.6 or later (a channel schedule: the firmware can hop channels with the TSF)"
                 : "1.4 or 1.5 (subcommands 1 to 5 only: one channel, no hopping; Samsung's S928BXXU5DZDP source)");
  unsigned users = 0;
  for (unsigned i = 0; i < r->iface_count; i++) {
    const br_wifi_iface *f = &r->ifaces[i];
    if (f->wiphy != w->wiphy) continue;
    put(o, "p2p: Wonder: in use: %s (%s)%s\n", f->name[0] ? f->name : "(no netdev)", br_wifi_iftype_name(f->iftype),
        !strcmp(f->name, BR_WONDER_IFNAME) ? ": wifictl wonder's own (a session that did not end: the next `wifictl wonder` removes it)" : "");
    users++;
  }
  if (tap) {
    put(o, "p2p: Wonder: the PASSTHRU vdev %s (index %u, wiphy %u) is up", tap->name, tap->ifindex, tap->wiphy);
    if (tap->freq_mhz) put(o, " on %u MHz", tap->freq_mhz);
    put(o, "\n");
  }
  if (!users && !tap) put(o, "p2p: Wonder: idle (no interface on it, no wondertap vdev)\n");
}

size_t br_wifi_radios_report(const br_wifi_radios *r, const char *station, char *text, size_t size)
{
  struct out o = { text, size, 0 };
  if (size) text[0] = 0;
  if (!r->count) { put(&o, "radio: none (nl80211 gave no wiphy)\n"); return o.used; }
  const br_wifi_caps *wonder = br_wifi_wonder(r), *home = &r->wiphys[0];
  for (unsigned i = 0; i < r->iface_count; i++)
    if (station && !strcmp(r->ifaces[i].name, station))
      for (unsigned k = 0; k < r->count; k++) if (r->wiphys[k].wiphy == r->ifaces[i].wiphy) home = &r->wiphys[k];
  put(&o, "radios: %u%s\n", r->count, r->wiphys_dropped ? " (more were not kept)" : "");
  for (unsigned i = 0; i < r->count; i++) {
    facts(&o, &r->wiphys[i]);
    if (&r->wiphys[i] == wonder) put(&o, "  (Google's Wonder radio)\n");
    if (&r->wiphys[i] == home && station) put(&o, "  (%s's radio)\n", station);
  }
  for (unsigned i = 0; i < r->iface_count; i++) iface_line(&o, &r->ifaces[i], wonder);
  wonder_block(&o, r, wonder);
  verdicts(&o, home, wonder);
  return o.used;
}
