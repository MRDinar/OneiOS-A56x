// SPDX-License-Identifier: MIT
// xnuhost's Wi-Fi bridge (docs/hardware.md section 3, README.md here): the guest's en0 on the phone's wlan0.
//   frames.c      which frames cross, and GRO frames cut back to the link's size
//   station.c     the station the guest's driver talks to (drivers/net/oneios_wifi_proto.h), over a radio back end
//   p2p.c         what every radio offers for a peer-to-peer link (AirDrop's AWDL, Google's Wonder), for `wifictl p2p`
//   wonder.c      `wifictl wonder`: a session on Wonder's raw 802.11 path beside wlan0, its AWDL frames counted
//   wlan.c        the back end on the phone: AF_PACKET on wlan0, nl80211 for scans and the link, wlan0's host state
//                 kept and given back
//   supplicant.c  joins: the vendor's wpa_supplicant through its AIDL service (aidl_parcel.c, aidl_binder.c)
#ifndef ONEIOS_BRIDGES_WIFI_H
#define ONEIOS_BRIDGES_WIFI_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include "oneios_wifi_proto.h"
#include "bridges/bridges.h"
#include "bridges/devices.h"

#define BR_WIFI_FRAME 1514u
#define BR_WIFI_CONTROL_TYPE OW_ETHERTYPE

// ---- frames.c ----
uint16_t br_wifi_ethertype(const uint8_t *frame, size_t length);
// A frame of the guest's that may go on the air: not EAPOL, not the bridge's own type, sent with the radio's address.
bool br_wifi_to_radio(const uint8_t *frame, size_t length, const uint8_t mac[6]);
// A frame of the radio's that the guest gets: to its address, broadcast or multicast; not EAPOL.
bool br_wifi_to_guest(const uint8_t *frame, size_t length, const uint8_t mac[6]);
// A frame for the guest: as it is when it fits the link and the receive path did not coalesce it; a coalesced TCP
// frame (IPv4 or IPv6) cut into segments of at most mss payload bytes with their checksums made again. fn gets each
// frame. Returns the frames given (0: the frame was too long and not TCP: dropped).
typedef void (*br_wifi_segment_fn)(void *ctx, const uint8_t *frame, size_t length);
size_t br_wifi_segment(const uint8_t *frame, size_t length, uint16_t mss, bool coalesced, br_wifi_segment_fn fn, void *ctx);

// A DHCP message as the log names it (frames.c): whose it is (the client's hardware address), what it asks or gives.
// A REQUEST never has a yiaddr: the address it asks for is option 50 (SELECTING, INIT-REBOOT) or its ciaddr (RENEWING,
// REBINDING), and only a SELECTING REQUEST names the server (option 54).
typedef struct {
  uint8_t type;                     // option 53: 1 DISCOVER, 2 OFFER, 3 REQUEST, 4 DECLINE, 5 ACK, 6 NAK, 7 RELEASE, 8 INFORM
  uint8_t client[6];                // chaddr
  uint8_t ciaddr[4], yiaddr[4];
  uint8_t requested[4];             // option 50; 0.0.0.0 when absent
  uint8_t server[4];                // option 54; 0.0.0.0 when absent
  bool has_requested, has_server;
  bool ip_broadcast;                // sent to 255.255.255.255 (a REBINDING REQUEST is; a RENEWING one goes to the server)
} br_wifi_dhcp_msg;
bool br_wifi_dhcp(const uint8_t *frame, size_t length, br_wifi_dhcp_msg *out);
// A REQUEST's state (RFC 2131 4.3.2): "SELECTING", "INIT-REBOOT", "RENEWING", "REBINDING"; "" for other messages.
const char *br_wifi_dhcp_request_kind(const br_wifi_dhcp_msg *m);
// The address a message is about: a REQUEST's or DECLINE's (option 50, else ciaddr), a RELEASE's or INFORM's ciaddr,
// a server's answer's yiaddr.
void br_wifi_dhcp_address(const br_wifi_dhcp_msg *m, uint8_t out[4]);
// A message a client sends (DISCOVER, REQUEST, DECLINE, RELEASE, INFORM), not a server's answer.
bool br_wifi_dhcp_from_client(const br_wifi_dhcp_msg *m);
// The log line of a message that crossed the bridge (wlan.c), without the "wifi: DHCP: " prefix. from_guest: iOS sent
// it. BR_WIFI_DHCP_OTHER: a message from the radio whose client is not mac (another station's broadcast; wlan.c counts
// those and logs only the first); BR_WIFI_DHCP_IOS: iOS's own exchange.
enum { BR_WIFI_DHCP_IOS = 1, BR_WIFI_DHCP_OTHER = 2 };
int br_wifi_dhcp_describe(const br_wifi_dhcp_msg *m, bool from_guest, const uint8_t mac[6], char *out, size_t size);

// One multicast group of a generic netlink family (the attributes nested in one entry of CTRL_ATTR_MCAST_GROUPS):
// true and its id when its name is `name`. The kernel writes the id before the name.
bool br_wifi_nl_group(const uint8_t *attrs, size_t length, const char *name, uint32_t *id);

// The channels of an NL80211_ATTR_WIPHY_BANDS attribute (its payload: one nested attribute per band, each with
// NL80211_BAND_ATTR_FREQS), appended to out[count..max): the frequency and the OW_CH_* flags of each channel that is
// not disabled. Returns the new count. A split wiphy dump spreads the bands over several messages: call it for each.
unsigned br_wifi_nl_bands(const uint8_t *bands, size_t length, struct ow_channel *out, unsigned count, unsigned max);

// ---- p2p.c ----
// What the radio offers for a peer-to-peer link (docs/hardware.md "AirDrop"), from nl80211's GET_WIPHY messages (a
// split dump: br_wifi_caps_wiphy for each message's attributes) and GET_INTERFACE messages (br_wifi_caps_iface for
// each). Interface types are bit masks: bit n is nl80211_iftype n.
#define BR_WIFI_IFTYPES 13u          // NUM_NL80211_IFTYPES
#define BR_WIFI_COMBINATIONS 8u
#define BR_WIFI_COMB_LIMITS 6u
#define BR_WIFI_VENDOR_CMDS 256u
#define BR_WIFI_IFACES 16u
typedef struct { uint32_t oui, subcmd; } br_wifi_vendor_cmd;
typedef struct {
  uint32_t max_interfaces, channels;
  uint32_t limit_types[BR_WIFI_COMB_LIMITS], limit_max[BR_WIFI_COMB_LIMITS];
  unsigned limit_count;
} br_wifi_combination;
typedef struct {
  char name[16];
  uint32_t ifindex, wiphy, iftype, freq_mhz;
  uint8_t mac[6];
} br_wifi_iface;
typedef struct {
  bool have_wiphy;
  uint32_t wiphy;
  char name[32];
  uint32_t iftypes, software_iftypes;
  br_wifi_combination combinations[BR_WIFI_COMBINATIONS];
  unsigned combination_count;
  uint32_t action_tx, action_rx;     // the interface types that may send / register for action frames
  bool offchannel_tx, cmd_frame, cmd_remain_on_channel, cmd_start_nan, cmd_start_p2p_device, cmd_vendor;
  uint32_t max_roc_ms, features;
  uint8_t ext_features[16];
  uint16_t ext_length;
  br_wifi_vendor_cmd commands[BR_WIFI_VENDOR_CMDS], events[BR_WIFI_VENDOR_CMDS];
  unsigned command_count, event_count;
  br_wifi_iface ifaces[BR_WIFI_IFACES];
  unsigned iface_count;
} br_wifi_caps;
void br_wifi_caps_wiphy(br_wifi_caps *c, const uint8_t *attrs, size_t length);
void br_wifi_caps_iface(br_wifi_caps *c, const uint8_t *attrs, size_t length);
// Interface types a and b can be up at the same time in one of the radio's combinations.
bool br_wifi_caps_combines(const br_wifi_caps *c, uint32_t a, uint32_t b);
const char *br_wifi_iftype_name(uint32_t type);
// The report of one radio: the capabilities, every interface, then one "p2p:" line per way to an AWDL link.
// Returns the length it needed (as snprintf).
size_t br_wifi_caps_report(const br_wifi_caps *c, char *text, size_t size);
bool br_wifi_caps_has_vendor(const br_wifi_caps *c, uint32_t oui, uint32_t subcmd);

// Every radio of the phone (docs/hardware.md 4a): a GET_WIPHY dump with no interface named gives each wiphy, split
// over several messages that each name their wiphy (NL80211_ATTR_WIPHY first); a GET_INTERFACE dump gives every
// interface. br_wifi_radios_wiphy and br_wifi_radios_iface take one message's attributes each.
#define BR_WIFI_WIPHYS 4u
#define BR_WIFI_OUI_GOOGLE 0x001a11u     // Wonder's vendor commands (Google's OUI_GOOGLE)
typedef struct {
  br_wifi_caps wiphys[BR_WIFI_WIPHYS];
  unsigned count, wiphys_dropped;    // wiphys past BR_WIFI_WIPHYS are counted, not kept
  br_wifi_iface ifaces[BR_WIFI_IFACES];
  unsigned iface_count;
} br_wifi_radios;
void br_wifi_radios_wiphy(br_wifi_radios *r, const uint8_t *attrs, size_t length);
void br_wifi_radios_iface(br_wifi_radios *r, const uint8_t *attrs, size_t length);
// Google's Wonder radio (a mac80211 soft-MAC over the Wi-Fi driver's PASSTHRU vdev): the wiphy named "wonder...",
// else one with vendor commands under Google's OUI that offers monitor mode but no access point (the Wi-Fi driver's
// own wiphy offers an access point; a Broadcom driver also has commands under Google's OUI). NULL: none.
const br_wifi_caps *br_wifi_wonder(const br_wifi_radios *r);
// Wonder's vendor subcommand by its number ("SET_FREQUENCY", ...), "?" for one we do not know.
const char *br_wifi_wonder_subcmd_name(uint32_t subcmd);
// The report `wifictl p2p` prints: each radio's capabilities, every interface, a "p2p: Wonder:" block, then the
// "p2p:" verdicts for the station's radio (the wiphy of the interface named station, else the first radio).
size_t br_wifi_radios_report(const br_wifi_radios *r, const char *station, char *text, size_t size);

// ---- station.c ----
// What the radio back end reports about the link.
typedef struct {
  bool     up;
  int32_t  rssi_dbm, noise_dbm;
  uint32_t tx_rate_mbps, freq_mhz, reason;
  uint8_t  bssid[6];
} br_wifi_link;

typedef struct {
  struct ow_bss bss;
  uint8_t ies[OW_MAX_IES];
} br_wifi_bss;

// The radio, as the station uses it. Each call returns at once; scan and join answer later through
// br_wifi_scan_add/br_wifi_scan_done and br_wifi_join_done.
typedef struct {
  int  (*scan)(void *ctx);
  int  (*join)(void *ctx, const struct ow_join *join, const br_wifi_bss *bss);   // bss: the last scan's record, or NULL
  int  (*leave)(void *ctx);
  int  (*link)(void *ctx, br_wifi_link *out);    // 0 and *out, or -errno
  int  (*channels)(void *ctx, struct ow_channel *out, unsigned max);   // the channels it may use: their number, or -errno (NULL: none)
} br_wifi_radio;

typedef struct br_wifi_station br_wifi_station;
// to_guest gets each frame for the guest (the station's messages, as frames of the control type).
typedef void (*br_wifi_guest_fn)(void *ctx, const uint8_t *frame, size_t length);
br_wifi_station *br_wifi_station_create(const br_wifi_radio *radio, void *radio_ctx, const uint8_t mac[6], const char country[2],
                                        br_wifi_guest_fn to_guest, void *guest_ctx);
void br_wifi_station_destroy(br_wifi_station *s);
// A control frame from the guest (br_wifi_ethertype == BR_WIFI_CONTROL_TYPE).
void br_wifi_station_message(br_wifi_station *s, const uint8_t *frame, size_t length);
void br_wifi_scan_add(br_wifi_station *s, const struct ow_bss *bss, const uint8_t *ies, size_t ie_length);
void br_wifi_scan_done(br_wifi_station *s, int error);
void br_wifi_join_done(br_wifi_station *s, uint32_t status, uint32_t code);
// The link changed (or a periodic look): the guest gets OW_STATE.
void br_wifi_link_changed(br_wifi_station *s);
// What the station sends for a state (for the tests and the tool).
size_t br_wifi_state_frame(br_wifi_station *s, uint8_t *out, size_t out_size);


// ---- wonder.c: `wifictl wonder`, a session on Wonder's raw 802.11 path beside wlan0 (docs/hardware.md 4a) ----
// The frames Wonder's monitor interface gives (radiotap, then 802.11), counted: how many are AWDL's (an action frame
// of category 127 under Apple's OUI 00:17:f2 with AWDL's type 8, or a data frame to AWDL's BSSID 00:25:00:ff:94:73),
// and from whom.
#define BR_AWDL_PEERS 16u
typedef struct {
  int kind;                          // BR_AWDL_*
  uint8_t type;                      // 802.11: 0 management, 1 control, 2 data, 3 extension
  uint8_t subtype;                   // an AWDL action frame's subtype (0 PSF, 3 MIF)
  uint8_t source[6];                 // addr2
  bool tsft, bad_fcs, has_signal;
  int8_t signal_dbm;
} br_awdl_frame;
enum { BR_AWDL_NOT = 0, BR_AWDL_ACTION = 1, BR_AWDL_DATA = 2, BR_AWDL_OTHER_ACTION = 3 };
// false: not a radiotap frame with a whole 802.11 header (counted as unparsed).
bool br_wifi_radiotap_frame(const uint8_t *frame, size_t length, br_awdl_frame *out);
typedef struct {
  uint64_t frames, unparsed, bad_fcs, with_tsft, management, control, data;
  uint64_t action, awdl_action, awdl_psf, awdl_mif, awdl_other, awdl_data;
  struct { uint8_t mac[6]; uint32_t action, data; int8_t best_dbm; bool has_signal; } peers[BR_AWDL_PEERS];
  unsigned peer_count, peers_dropped;
} br_awdl_count;
void br_awdl_count_add(br_awdl_count *c, const uint8_t *frame, size_t length);
size_t br_awdl_count_report(const br_awdl_count *c, unsigned seconds, char *text, size_t size);

// The vendor data of Wonder's commands (nlattrs under NL80211_ATTR_VENDOR_DATA, Samsung's Wonder 1.5 and Google's
// wonder_ven_cmd.h): the length written, 0 when it does not fit.
size_t br_wonder_set_frequency(uint8_t *out, size_t size, uint32_t freq_mhz);          // 20 MHz
size_t br_wonder_set_regulatory(uint8_t *out, size_t size, const char country[2]);
size_t br_wonder_set_fixed_tx_rate(uint8_t *out, size_t size);                          // HT MCS 0, 1 stream, 20 MHz, 0.8 us
size_t br_wonder_set_bssid_filter(uint8_t *out, size_t size, const uint8_t bssid[6]);
// GET_IF_MAC_ADDR's reply (the attributes of the reply's VENDOR_DATA): true and the address.
bool br_wonder_mac_reply(const uint8_t *data, size_t length, uint8_t mac[6]);

// What the session does on the phone; wlan.c has the phone's (nl80211, an ioctl, AF_PACKET), the tests a stand-in.
// Every call returns 0 (or a count, a handle) or -errno.
typedef struct {
  int  (*radios)(void *ctx, br_wifi_radios *out);                                   // every wiphy and interface
  int  (*country)(void *ctx, uint32_t wiphy, char cc[3]);                            // the radio's regulatory country
  int  (*new_monitor)(void *ctx, uint32_t wiphy, const char *name, uint32_t *ifindex);
  int  (*del_iface)(void *ctx, uint32_t ifindex);
  int  (*set_up)(void *ctx, const char *name, bool up);
  int  (*vendor)(void *ctx, uint32_t ifindex, uint32_t subcmd, const uint8_t *data, size_t length, uint8_t *reply, size_t *reply_length);
  int  (*open_frames)(void *ctx, uint32_t ifindex);                                 // a handle, >= 0
  int  (*receive)(void *ctx, int handle, uint8_t *buffer, size_t size, unsigned wait_ms);   // a length, 0 none came
  void (*close_frames)(void *ctx, int handle);
  int  (*link)(void *ctx, const char *ifname, br_wifi_link *out);                    // up: associated, and its AP
  uint64_t (*now_ms)(void *ctx);
  void (*pause_ms)(void *ctx, unsigned ms);
} br_wonder_ops;
typedef struct {
  const char *station;               // wlan0
  const char *name;                  // the monitor interface made on Wonder's radio (BR_WONDER_IFNAME)
  uint32_t freq_mhz;                 // an AWDL channel: 2437 (6), 5220 (44) or 5745 (149)
  unsigned seconds;                  // how long frames are counted
  char country[3];                   // "": the station's radio's own (Wonder's init sets the driver's country to it)
  volatile int *stop;                // set by a signal: the count ends and the teardown runs
} br_wonder_params;
#define BR_WONDER_IFNAME "wondermon0"
typedef struct {
  br_awdl_count count;
  uint8_t mac[6];                    // the PASSTHRU vdev's address (GET_IF_MAC_ADDR)
  bool have_mac, opened, link_before, link_after;
  br_wifi_link before, after;
} br_wonder_result;
// The whole session: refused when Wonder is missing or in use, the frequency is no AWDL channel, or the country is
// not known; else the interface made and set, up, counted, then down and removed in reverse (also after a failed
// step), and wlan0's link checked against its link before. Each step is a "wonder:" log line. 0: done and wlan0 as
// it was; -errno of the step that failed; -ENOLINK: wlan0 lost its link.
int br_wonder_run(const br_wonder_ops *ops, void *ctx, const br_wonder_params *p, br_wonder_result *r);
bool br_wonder_freq_ok(uint32_t freq_mhz);

// ---- wlan.c ----
// br_wlan_prepare, br_wlan_send, br_wlan_close and br_wlan_restore: bridges/devices.h (xnuhost uses them).
// One scan on the interface, for the tool: fn gets each network. The number of networks, or -errno.
typedef void (*br_wlan_bss_fn)(void *ctx, const struct ow_bss *bss, const uint8_t *ies, size_t ie_length);
int  br_wlan_scan_once(const char *ifname, unsigned wait_ms, br_wlan_bss_fn fn, void *ctx);
// Every radio and interface (a split GET_WIPHY dump of every wiphy, a GET_INTERFACE dump). Read only. 0, or -errno.
int  br_wlan_radios(br_wifi_radios *out);
// The phone's br_wonder_ops (nl80211, SIOCSIFFLAGS, AF_PACKET); ctx from br_wlan_wonder_open, freed by _close.
extern const br_wonder_ops br_wlan_wonder_ops;
void *br_wlan_wonder_open(void);
void br_wlan_wonder_close(void *ctx);

#endif
