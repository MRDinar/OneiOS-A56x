// SPDX-License-Identifier: MIT
// Unit tests of the Wi-Fi bridge's pure parts (no phone, no binder, no radio): the frame rules and the cut of
// coalesced frames, the station against a fake radio, the AIDL parcels byte for byte, what a network's information
// elements ask for, the supplicant's callbacks decoded. The binder path: tests/test_aidl_binder.c.
// Every radio and Wonder (wifi/p2p.c), AWDL's frames and `wifictl wonder` on a stand-in nl80211 (wifi/wonder.c).
// ONEIOS_TEST_SHOW=1: also print the stand-in phone's `wifictl p2p` report and its Wonder session's lines.
#define _GNU_SOURCE
#include "../wifi/supplicant.h"
#include "../wifi/wifi.h"
#include <errno.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static int checks, failures;
#define CHECK(cond) do { checks++; if (!(cond)) { failures++; printf("FAIL %s:%d: %s\n", __FILE__, __LINE__, #cond); } } while (0)

static const uint8_t mac[6] = { 0x02, 0x11, 0x22, 0x33, 0x44, 0x55 };

// ---- frames ----
typedef struct { uint8_t frame[8][1600]; size_t length[8]; unsigned count; } sink;
static void take(void *ctx, const uint8_t *frame, size_t length)
{
  sink *s = ctx;
  if (s->count < 8 && length <= 1600) { memcpy(s->frame[s->count], frame, length); s->length[s->count] = length; }
  s->count++;
}
static uint32_t sum16(uint32_t sum, const uint8_t *p, size_t n)
{
  for (size_t i = 0; i + 1 < n; i += 2) sum += (uint32_t)(p[i] << 8 | p[i + 1]);
  if (n & 1) sum += (uint32_t)p[n - 1] << 8;
  return sum;
}
static bool sum_ok(uint32_t sum) { while (sum >> 16) sum = (sum & 0xffff) + (sum >> 16); return sum == 0xffff; }
// The IPv4 header and TCP checksums of a frame are right.
static bool tcp4_ok(const uint8_t *f, size_t n)
{
  const uint8_t *ip = f + 14, *tcp = ip + 20;
  size_t tcp_len = n - 34;
  if ((size_t)(ip[2] << 8 | ip[3]) != n - 14) return false;
  return sum_ok(sum16(0, ip, 20)) && sum_ok(sum16(sum16(0, ip + 12, 8) + 6 + (uint32_t)tcp_len, tcp, tcp_len));
}
static bool tcp6_ok(const uint8_t *f, size_t n)
{
  const uint8_t *ip = f + 14, *tcp = ip + 40;
  size_t tcp_len = n - 54;
  if ((size_t)(ip[4] << 8 | ip[5]) != tcp_len) return false;
  return sum_ok(sum16(sum16(0, ip + 8, 32) + 6 + (uint32_t)tcp_len, tcp, tcp_len));
}
static size_t make_tcp(uint8_t *f, bool v6, size_t payload, uint8_t flags)
{
  size_t l3 = v6 ? 40 : 20;
  memset(f, 0, 14 + l3 + 20);
  memcpy(f, mac, 6);
  memcpy(f + 6, "\x02\xaa\xbb\xcc\xdd\xee", 6);
  f[12] = v6 ? 0x86 : 0x08; f[13] = v6 ? 0xdd : 0x00;
  uint8_t *ip = f + 14;
  if (v6) { ip[0] = 0x60; ip[6] = 6; ip[7] = 64; ip[8] = 0xfe; ip[9] = 0x80; ip[23] = 1; ip[24] = 0xfe; ip[25] = 0x80; ip[39] = 2; }
  else { ip[0] = 0x45; ip[4] = 0x12; ip[5] = 0x34; ip[8] = 64; ip[9] = 6; memcpy(ip + 12, "\xc0\xa8\x01\x01\xc0\xa8\x01\x02", 8); }
  uint8_t *tcp = ip + l3;
  tcp[0] = 0x01; tcp[1] = 0xbb; tcp[2] = 0xc0; tcp[3] = 0x00; tcp[4] = 0x00; tcp[5] = 0x00; tcp[6] = 0xff; tcp[7] = 0x00;   // seq 0x0000ff00
  tcp[12] = 0x50; tcp[13] = flags; tcp[14] = 0xff; tcp[15] = 0xff;
  for (size_t i = 0; i < payload; i++) tcp[20 + i] = (uint8_t)(i * 7 + 1);
  return 14 + l3 + 20 + payload;
}

static void test_frames(void)
{
  uint8_t f[64] = { 0 };
  memcpy(f, "\xff\xff\xff\xff\xff\xff", 6); memcpy(f + 6, mac, 6); f[12] = 0x08; f[13] = 0x06;
  CHECK(br_wifi_ethertype(f, 60) == 0x0806 && br_wifi_ethertype(f, 13) == 0);
  CHECK(br_wifi_to_radio(f, 60, mac));                                     // the guest's ARP request
  f[6] ^= 4;
  CHECK(!br_wifi_to_radio(f, 60, mac));                                    // another source address: a private address of iOS's
  f[6] ^= 4; f[12] = 0x88; f[13] = 0x8e;
  CHECK(!br_wifi_to_radio(f, 60, mac) && !br_wifi_to_guest(f, 60, mac));   // EAPOL is the supplicant's, both ways
  f[13] = 0xb5;
  CHECK(!br_wifi_to_radio(f, 60, mac) && !br_wifi_to_guest(f, 60, mac));   // the station's own messages never cross
  f[12] = 0x08; f[13] = 0x00;
  CHECK(br_wifi_to_guest(f, 60, mac));                                     // broadcast
  memcpy(f, "\x33\x33\x00\x00\x00\x01", 6);
  CHECK(br_wifi_to_guest(f, 60, mac));                                     // multicast (IPv6 all nodes)
  memcpy(f, mac, 6);
  CHECK(br_wifi_to_guest(f, 60, mac));
  f[5] ^= 1;
  CHECK(!br_wifi_to_guest(f, 60, mac) && !br_wifi_to_guest(f, 10, mac) && !br_wifi_to_radio(f, 1515, mac));

  // A DHCP OFFER (server to client) and a DISCOVER; a frame that is UDP but not DHCP; one cut inside its options.
  static uint8_t d[14 + 20 + 8 + 240 + 16];
  memset(d, 0, sizeof d);
  memcpy(d, mac, 6); d[12] = 0x08; d[14] = 0x45; d[23] = 17;
  d[34] = 0; d[35] = 67; d[36] = 0; d[37] = 68;                              // UDP 67 -> 68
  uint8_t *bootp = d + 42;
  bootp[0] = 2; memcpy(bootp + 16, "\x0a\x00\x01\x37", 4);                   // yiaddr 10.0.1.55
  memcpy(bootp + 236, "\x63\x82\x53\x63", 4);
  memcpy(bootp + 240, "\x35\x01\x02\x36\x04\x0a\x00\x01\x01\xff", 10);       // OFFER, server 10.0.1.1
  br_wifi_dhcp_msg dm;
  CHECK(br_wifi_dhcp(d, sizeof d, &dm) && dm.type == 2 && dm.yiaddr[3] == 0x37 && dm.server[3] == 1 && dm.has_server);
  d[35] = 68; d[37] = 67; bootp[242] = 1;
  CHECK(br_wifi_dhcp(d, sizeof d, &dm) && dm.type == 1);
  d[37] = 53;
  CHECK(!br_wifi_dhcp(d, sizeof d, &dm));
  d[37] = 67;
  CHECK(!br_wifi_dhcp(d, 14 + 20 + 8 + 240 + 2, &dm) && !br_wifi_dhcp(d, 100, &dm));
}

// ---- DHCP: the REQUEST forms of RFC 2131 4.3.2 as clients send them ----
// An Ethernet frame with a DHCP message: the IPv4 header (checksummed), UDP, the BOOTP header with chaddr, the cookie
// and the options as iOS's and Android's clients put them (53 first, client id 61, the parameter list 55, 57, the
// requested address 50 and the server 54 where the state has them, 12 a host name, then the end).
typedef struct {
  const uint8_t *eth_dst, *eth_src, *client;
  const char *ip_src, *ip_dst, *ciaddr, *yiaddr, *requested, *server;   // dotted, NULL: none / 0.0.0.0
  uint8_t op, type;
  bool broadcast_flag;
} dhcp_spec;
static void ip4(const char *s, uint8_t out[4])
{
  unsigned a = 0, b = 0, c = 0, e = 0;
  if (s) sscanf(s, "%u.%u.%u.%u", &a, &b, &c, &e);
  out[0] = (uint8_t)a; out[1] = (uint8_t)b; out[2] = (uint8_t)c; out[3] = (uint8_t)e;
}
static size_t make_dhcp(uint8_t *f, const dhcp_spec *s)
{
  memset(f, 0, 600);
  memcpy(f, s->eth_dst, 6); memcpy(f + 6, s->eth_src, 6); f[12] = 0x08; f[13] = 0x00;
  uint8_t *ip = f + 14, *udp = ip + 20, *b = udp + 8, *o = b + 240;
  memcpy(b + 236, "\x63\x82\x53\x63", 4);
  b[0] = s->op; b[1] = 1; b[2] = 6; b[4] = 0x5a; b[5] = 0x1e; b[6] = 0x77; b[7] = 0x03;   // xid
  if (s->broadcast_flag) b[10] = 0x80;
  ip4(s->ciaddr, b + 12); ip4(s->yiaddr, b + 16); memcpy(b + 28, s->client, 6);
  *o++ = 53; *o++ = 1; *o++ = s->type;
  *o++ = 61; *o++ = 7; *o++ = 1; memcpy(o, s->client, 6); o += 6;
  if (s->op == 1) { static const uint8_t prl[] = { 55, 6, 1, 121, 3, 6, 15, 119 }; memcpy(o, prl, sizeof prl); o += sizeof prl; *o++ = 57; *o++ = 2; *o++ = 5; *o++ = 0xdc; }
  if (s->requested) { *o++ = 50; *o++ = 4; ip4(s->requested, o); o += 4; }
  if (s->server) { *o++ = 54; *o++ = 4; ip4(s->server, o); o += 4; }
  if (s->op == 1) { *o++ = 12; *o++ = 6; memcpy(o, "iPhone", 6); o += 6; }
  else { *o++ = 51; *o++ = 4; o[2] = 0x0e; o[3] = 0x10; o += 4; *o++ = 1; *o++ = 4; ip4("255.255.0.0", o); o += 4; }
  *o++ = 255;
  while ((size_t)(o - b) < 300) *o++ = 0;   // BOOTP's minimum of 300 bytes, as clients pad
  const size_t bootp = (size_t)(o - b), udp_len = 8 + bootp, ip_len = 20 + udp_len;
  ip[0] = 0x45; ip[2] = (uint8_t)(ip_len >> 8); ip[3] = (uint8_t)ip_len; ip[8] = 64; ip[9] = 17;
  ip4(s->ip_src, ip + 12); ip4(s->ip_dst, ip + 16);
  uint32_t sum = sum16(0, ip, 20);
  while (sum >> 16) sum = (sum & 0xffff) + (sum >> 16);
  ip[10] = (uint8_t)(~sum >> 8); ip[11] = (uint8_t)~sum;
  udp[1] = s->op == 1 ? 68 : 67; udp[3] = s->op == 1 ? 67 : 68; udp[4] = (uint8_t)(udp_len >> 8); udp[5] = (uint8_t)udp_len;
  return 14 + ip_len;
}

static void test_dhcp(void)
{
  static const uint8_t bcast[6] = { 0xff, 0xff, 0xff, 0xff, 0xff, 0xff };
  static const uint8_t other[6] = { 0x3c, 0x22, 0xfb, 0x10, 0x20, 0x30 };    // another station on the network
  static const uint8_t router[6] = { 0x00, 0x1a, 0x2b, 0x3c, 0x4d, 0x5e };   // the access point's gateway
  uint8_t f[600];
  char line[160];
  br_wifi_dhcp_msg m;
  uint8_t a[4];

  // What the log said before 2026-11-13 for a REQUEST: its yiaddr and option 54. Only a SELECTING REQUEST has a
  // server; no REQUEST has a yiaddr. So "REQUEST of 0.0.0.0 from 0.0.0.0" is every INIT-REBOOT, RENEWING or
  // REBINDING REQUEST that came in from the radio (frames iOS sent were logged as "iOS sends ...").
  const dhcp_spec init_reboot = { bcast, other, other, "0.0.0.0", "255.255.255.255", NULL, NULL, "172.16.235.99", NULL, 1, 3, false };
  const dhcp_spec rebinding = { bcast, other, other, "172.16.235.80", "255.255.255.255", "172.16.235.80", NULL, NULL, NULL, 1, 3, false };
  const dhcp_spec renewing = { router, mac, mac, "172.16.235.71", "172.16.0.1", "172.16.235.71", NULL, NULL, NULL, 1, 3, false };
  const dhcp_spec selecting = { bcast, mac, mac, "0.0.0.0", "255.255.255.255", NULL, NULL, "172.16.235.71", "172.16.0.1", 1, 3, false };
  const dhcp_spec discover = { bcast, other, other, "0.0.0.0", "255.255.255.255", NULL, NULL, NULL, NULL, 1, 1, true };
  const dhcp_spec ack = { mac, router, mac, "172.16.0.1", "172.16.235.71", NULL, "172.16.235.71", NULL, "172.16.0.1", 2, 5, false };
  const dhcp_spec *forms[] = { &init_reboot, &rebinding, &renewing };
  for (unsigned i = 0; i < 3; i++) {
    const size_t n = make_dhcp(f, forms[i]);
    CHECK(br_wifi_dhcp(f, n, &m) && m.type == 3 && !(m.yiaddr[0] | m.yiaddr[1] | m.yiaddr[2] | m.yiaddr[3]) && !m.has_server);
  }

  // INIT-REBOOT of another station (a broadcast every station gets): its own line, counted apart from iOS's.
  size_t n = make_dhcp(f, &init_reboot);
  CHECK(br_wifi_dhcp(f, n, &m) && m.ip_broadcast && m.has_requested && !memcmp(m.client, other, 6));
  CHECK(!strcmp(br_wifi_dhcp_request_kind(&m), "INIT-REBOOT"));
  br_wifi_dhcp_address(&m, a);
  CHECK(a[0] == 172 && a[3] == 99);
  CHECK(br_wifi_dhcp_describe(&m, false, mac, line, sizeof line) == BR_WIFI_DHCP_OTHER &&
        !strcmp(line, "another station's REQUEST INIT-REBOOT for 172.16.235.99 (client 3c:22:fb:..)"));
  // REBINDING of another station: ciaddr, to the broadcast address.
  n = make_dhcp(f, &rebinding);
  CHECK(br_wifi_dhcp(f, n, &m) && !strcmp(br_wifi_dhcp_request_kind(&m), "REBINDING"));
  CHECK(br_wifi_dhcp_describe(&m, false, mac, line, sizeof line) == BR_WIFI_DHCP_OTHER &&
        !strcmp(line, "another station's REQUEST REBINDING for 172.16.235.80 (client 3c:22:fb:..)"));
  // iOS's own RENEWING REQUEST (unicast to the server) and SELECTING one, as the guest sends them.
  n = make_dhcp(f, &renewing);
  CHECK(br_wifi_dhcp(f, n, &m) && !m.ip_broadcast && !strcmp(br_wifi_dhcp_request_kind(&m), "RENEWING"));
  CHECK(br_wifi_dhcp_describe(&m, true, mac, line, sizeof line) == BR_WIFI_DHCP_IOS && !strcmp(line, "iOS sends REQUEST RENEWING for 172.16.235.71"));
  n = make_dhcp(f, &selecting);
  CHECK(br_wifi_dhcp(f, n, &m) && !strcmp(br_wifi_dhcp_request_kind(&m), "SELECTING"));
  CHECK(br_wifi_dhcp_describe(&m, true, mac, line, sizeof line) == BR_WIFI_DHCP_IOS && !strcmp(line, "iOS sends REQUEST SELECTING for 172.16.235.71"));
  // The same REQUEST back from the radio (an access point that reflects a station's broadcast to it): iOS's own.
  CHECK(br_wifi_dhcp_describe(&m, false, mac, line, sizeof line) == BR_WIFI_DHCP_IOS &&
        !strcmp(line, "iOS's own REQUEST SELECTING for 172.16.235.71 came back from the radio"));
  // iOS's INIT-REBOOT (a known network's lease asked again after a join) sent by the guest.
  dhcp_spec own_reboot = init_reboot;
  own_reboot.eth_src = mac; own_reboot.client = mac; own_reboot.requested = "172.16.235.71";
  n = make_dhcp(f, &own_reboot);
  CHECK(br_wifi_dhcp(f, n, &m) && br_wifi_dhcp_describe(&m, true, mac, line, sizeof line) == BR_WIFI_DHCP_IOS &&
        !strcmp(line, "iOS sends REQUEST INIT-REBOOT for 172.16.235.71"));
  // Another station's DISCOVER; the server's ACK to iOS.
  n = make_dhcp(f, &discover);
  CHECK(br_wifi_dhcp(f, n, &m) && m.type == 1 && br_wifi_dhcp_describe(&m, false, mac, line, sizeof line) == BR_WIFI_DHCP_OTHER &&
        !strcmp(line, "another station's DISCOVER for 0.0.0.0 (client 3c:22:fb:..)"));
  n = make_dhcp(f, &ack);
  CHECK(br_wifi_dhcp(f, n, &m) && m.type == 5 && !strcmp(br_wifi_dhcp_request_kind(&m), ""));
  CHECK(br_wifi_dhcp_describe(&m, false, mac, line, sizeof line) == BR_WIFI_DHCP_IOS && !strcmp(line, "ACK of 172.16.235.71 from 172.16.0.1"));
  // An answer to another station (a server that broadcasts its OFFER or ACK): another station's.
  dhcp_spec other_ack = ack;
  other_ack.eth_dst = bcast; other_ack.client = other; other_ack.yiaddr = "172.16.235.99";
  n = make_dhcp(f, &other_ack);
  CHECK(br_wifi_dhcp(f, n, &m) && br_wifi_dhcp_describe(&m, false, mac, line, sizeof line) == BR_WIFI_DHCP_OTHER &&
        !strcmp(line, "another station's ACK of 172.16.235.99 (client 3c:22:fb:..)"));
  // Cut inside the options: the type still read when option 53 came whole; cut in the header: not DHCP.
  n = make_dhcp(f, &init_reboot);
  CHECK(br_wifi_dhcp(f, 14 + 20 + 8 + 240 + 3, &m) && m.type == 3 && !m.has_requested);
  CHECK(!br_wifi_dhcp(f, 14 + 20 + 8 + 239, &m));
}

// A netlink attribute (4-byte aligned) at the end of buf; returns where its payload starts, for a nested one.
static size_t nla(uint8_t *buf, size_t *len, uint16_t type, const void *data, uint16_t size)
{
  const size_t at = *len;
  buf[at] = (uint8_t)(4 + size); buf[at + 1] = (uint8_t)((4 + size) >> 8); buf[at + 2] = (uint8_t)type; buf[at + 3] = (uint8_t)(type >> 8);
  if (size) memcpy(buf + at + 4, data, size);
  *len = at + ((4u + size + 3u) & ~3u);
  return at;
}
// Closes a nested attribute opened at `at`: its length is what was added after it.
static void nla_end(uint8_t *buf, size_t len, size_t at) { buf[at] = (uint8_t)(len - at); buf[at + 1] = (uint8_t)((len - at) >> 8); }

static void test_bands(void)
{
  // NL80211_ATTR_WIPHY_BANDS as the kernel writes it: band 0 (2.4 GHz) with three frequencies, one disabled and one
  // passive; band 1 (5 GHz) with its bitrates first, then a DFS channel (passive, radar, indoor only, no 80 MHz) and
  // one with no 40 MHz above it.
  uint8_t b[512];
  size_t len = 0;
  uint32_t f;
  size_t band = nla(b, &len, 0, NULL, 0), freqs = nla(b, &len, 1, NULL, 0), e;
  e = nla(b, &len, 0, NULL, 0); f = 2412; nla(b, &len, 1, &f, 4); nla_end(b, len, e);
  e = nla(b, &len, 1, NULL, 0); f = 2467; nla(b, &len, 1, &f, 4); nla(b, &len, 2, NULL, 0); nla_end(b, len, e);
  e = nla(b, &len, 2, NULL, 0); f = 2472; nla(b, &len, 1, &f, 4); nla(b, &len, 3, NULL, 0); nla_end(b, len, e);
  nla_end(b, len, freqs); nla_end(b, len, band);
  band = nla(b, &len, 1, NULL, 0);
  const uint8_t rates[8] = { 0 };
  nla(b, &len, 2, rates, sizeof rates);
  freqs = nla(b, &len, 1, NULL, 0);
  e = nla(b, &len, 0, NULL, 0); f = 5260; nla(b, &len, 1, &f, 4); nla(b, &len, 3, NULL, 0); nla(b, &len, 5, NULL, 0); nla(b, &len, 14, NULL, 0);
  nla(b, &len, 11, NULL, 0); nla_end(b, len, e);
  e = nla(b, &len, 1, NULL, 0); f = 5745; nla(b, &len, 1, &f, 4); nla(b, &len, 10, NULL, 0); nla_end(b, len, e);
  nla_end(b, len, freqs); nla_end(b, len, band);
  struct ow_channel ch[8];
  memset(ch, 0, sizeof ch);
  unsigned n = br_wifi_nl_bands(b, len, ch, 0, 8);
  CHECK(n == 4);
  CHECK(ch[0].freq_mhz == 2412 && ch[0].flags == 0 && ch[1].freq_mhz == 2472 && ch[1].flags == OW_CH_PASSIVE);
  CHECK(ch[2].freq_mhz == 5260 && ch[2].flags == (OW_CH_PASSIVE | OW_CH_RADAR | OW_CH_INDOOR | OW_CH_NO_80MHZ));
  CHECK(ch[3].freq_mhz == 5745 && ch[3].flags == OW_CH_NO_HT40);
  // A second message of a split dump appends; a full list stops; a cut band is left out, the bands before it stay.
  CHECK(br_wifi_nl_bands(b, len, ch, 4, 6) == 6 && ch[5].freq_mhz == 2472);
  CHECK(br_wifi_nl_bands(b, len, ch, 0, 2) == 2);
  CHECK(br_wifi_nl_bands(b, len - 8, ch, 0, 8) == 2 && br_wifi_nl_bands(b, 3, ch, 0, 8) == 0);   // band 1 cut: band 0 stays
}

// A flag attribute per interface type (SUPPORTED_IFTYPES, a limit's TYPES).
static void iftype_flags(uint8_t *b, size_t *len, uint16_t type, uint32_t mask)
{
  const size_t at = nla(b, len, type, NULL, 0);
  for (uint16_t t = 0; t < 13; t++) if (mask >> t & 1) nla(b, len, t, NULL, 0);
  nla_end(b, *len, at);
}

static void test_p2p(void)   // wifictl p2p: the radio's peer-to-peer abilities from nl80211 (wifi/p2p.c)
{
  enum { STA = 2, AP = 3, MON = 6, P2PC = 8, P2PGO = 9, P2PDEV = 10, NANIF = 12 };
  static br_wifi_caps c;
  memset(&c, 0, sizeof c);
  // The first message of a split wiphy dump: the radio, its types, two combinations, the action frames, the commands.
  uint8_t b[2048];
  size_t len = 0, at, e, l;
  uint32_t v = 0;
  nla(b, &len, 1, &v, 4);                                  // WIPHY 0
  nla(b, &len, 2, "phy0", 5);                              // WIPHY_NAME
  iftype_flags(b, &len, 32, 1u << STA | 1u << AP | 1u << MON | 1u << P2PC | 1u << P2PGO | 1u << P2PDEV);
  at = nla(b, &len, 120, NULL, 0);                         // INTERFACE_COMBINATIONS
  e = nla(b, &len, 1, NULL, 0);                            //   1: 2 interfaces on 1 channel: 1 station, 1 of {ap, p2p}
  l = nla(b, &len, 1, NULL, 0);                            //     LIMITS
  size_t one = nla(b, &len, 1, NULL, 0); v = 1; nla(b, &len, 1, &v, 4); iftype_flags(b, &len, 2, 1u << STA); nla_end(b, len, one);
  one = nla(b, &len, 2, NULL, 0); v = 1; nla(b, &len, 1, &v, 4); iftype_flags(b, &len, 2, 1u << AP | 1u << P2PC | 1u << P2PGO); nla_end(b, len, one);
  nla_end(b, len, l);
  v = 2; nla(b, &len, 2, &v, 4); v = 1; nla(b, &len, 4, &v, 4);
  nla_end(b, len, e);
  e = nla(b, &len, 2, NULL, 0);                            //   2: the monitor alone (1 interface)
  l = nla(b, &len, 1, NULL, 0);
  one = nla(b, &len, 1, NULL, 0); v = 1; nla(b, &len, 1, &v, 4); iftype_flags(b, &len, 2, 1u << MON | 1u << STA); nla_end(b, len, one);
  nla_end(b, len, l);
  v = 1; nla(b, &len, 2, &v, 4); nla(b, &len, 4, &v, 4);
  nla_end(b, len, e);
  nla_end(b, len, at);
  uint16_t fc;
  at = nla(b, &len, 99, NULL, 0);                          // TX_FRAME_TYPES: the station sends action and probe requests
  e = nla(b, &len, STA, NULL, 0); fc = 0x00d0; nla(b, &len, 101, &fc, 2); fc = 0x0040; nla(b, &len, 101, &fc, 2); nla_end(b, len, e);
  e = nla(b, &len, AP, NULL, 0); fc = 0x0050; nla(b, &len, 101, &fc, 2); nla_end(b, len, e);   // the AP: probe responses only
  nla_end(b, len, at);
  at = nla(b, &len, 100, NULL, 0);                         // RX_FRAME_TYPES: the station registers for action frames
  e = nla(b, &len, STA, NULL, 0); fc = 0x00d0; nla(b, &len, 101, &fc, 2); nla_end(b, len, e);
  nla_end(b, len, at);
  nla(b, &len, 108, NULL, 0);                              // OFFCHANNEL_TX_OK
  v = 5000; nla(b, &len, 111, &v, 4);                      // MAX_REMAIN_ON_CHANNEL_DURATION
  at = nla(b, &len, 50, NULL, 0);                          // SUPPORTED_COMMANDS: FRAME, REMAIN_ON_CHANNEL, VENDOR
  v = 59; nla(b, &len, 1, &v, 4); v = 55; nla(b, &len, 2, &v, 4); v = 103; nla(b, &len, 3, &v, 4);
  nla_end(b, len, at);
  const uint32_t vendor[3][2] = { { 0x001374, 12 }, { 0x001374, 81 }, { 0x0000f0, 7 } };
  at = nla(b, &len, 197, NULL, 0);                         // VENDOR_DATA
  for (int i = 0; i < 3; i++) nla(b, &len, (uint16_t)(i + 1), vendor[i], 8);
  nla_end(b, len, at);
  br_wifi_caps_wiphy(&c, b, len);
  CHECK(c.have_wiphy && c.wiphy == 0 && !strcmp(c.name, "phy0"));
  CHECK(c.iftypes == (1u << STA | 1u << AP | 1u << MON | 1u << P2PC | 1u << P2PGO | 1u << P2PDEV));
  CHECK(c.combination_count == 2 && c.combinations[0].max_interfaces == 2 && c.combinations[0].channels == 1 && c.combinations[0].limit_count == 2);
  CHECK(c.combinations[0].limit_types[1] == (1u << AP | 1u << P2PC | 1u << P2PGO) && c.combinations[1].limit_max[0] == 1);
  CHECK(c.action_tx == 1u << STA && c.action_rx == 1u << STA);
  CHECK(c.offchannel_tx && c.max_roc_ms == 5000 && c.cmd_frame && c.cmd_remain_on_channel && c.cmd_vendor && !c.cmd_start_nan);
  CHECK(c.command_count == 3 && c.commands[1].oui == 0x001374 && c.commands[1].subcmd == 81);
  // Station and AP together (two limits); station and monitor never (only in the 1-interface combination); two
  // stations never (one limit of 1).
  CHECK(br_wifi_caps_combines(&c, STA, AP) && br_wifi_caps_combines(&c, AP, STA));
  CHECK(!br_wifi_caps_combines(&c, STA, MON) && !br_wifi_caps_combines(&c, STA, STA));
  // A later message of the split dump: vendor events, and the same command again (kept once).
  len = 0;
  const uint32_t ev[2] = { 0x001374, 33 };
  at = nla(b, &len, 198, NULL, 0); nla(b, &len, 1, ev, 8); nla_end(b, len, at);
  at = nla(b, &len, 197, NULL, 0); nla(b, &len, 1, vendor[0], 8); nla_end(b, len, at);
  br_wifi_caps_wiphy(&c, b, len);
  CHECK(c.event_count == 1 && c.events[0].subcmd == 33 && c.command_count == 3);
  // Two GET_INTERFACE messages: wlan0 (a station on 5180 MHz) and a P2P device with no network device.
  len = 0;
  v = 44; nla(b, &len, 3, &v, 4); nla(b, &len, 4, "wlan0", 6); v = 0; nla(b, &len, 1, &v, 4); v = STA; nla(b, &len, 5, &v, 4);
  nla(b, &len, 6, mac, 6); v = 5180; nla(b, &len, 38, &v, 4);
  br_wifi_caps_iface(&c, b, len);
  len = 0;
  v = 0; nla(b, &len, 1, &v, 4); v = P2PDEV; nla(b, &len, 5, &v, 4);
  br_wifi_caps_iface(&c, b, len);
  CHECK(c.iface_count == 2 && c.ifaces[0].ifindex == 44 && !strcmp(c.ifaces[0].name, "wlan0") && c.ifaces[0].freq_mhz == 5180);
  CHECK(c.ifaces[1].iftype == P2PDEV && !c.ifaces[1].name[0] && !strcmp(br_wifi_iftype_name(P2PDEV), "p2p-device"));
  // The report: the facts, then the verdicts.
  static char text[8192];
  const size_t n = br_wifi_caps_report(&c, text, sizeof text);
  CHECK(n == strlen(text));
  CHECK(strstr(text, "radio: phy0 (wiphy 0)\n") != NULL);
  CHECK(strstr(text, "combination 1: up to 2 interfaces on 1 channel(s): 1 of {station}; 1 of {access point, p2p-client, p2p-go}\n") != NULL);
  CHECK(strstr(text, "action frames: sent by station; received by station\n") != NULL);
  CHECK(strstr(text, "remain on channel: yes (up to 5000 ms)") != NULL);
  CHECK(strstr(text, "  001374 Qualcomm: 12 81\n  0000f0 Samsung: 7\n") != NULL && strstr(text, "  events 001374 Qualcomm: 33\n") != NULL);
  CHECK(strstr(text, "interface: wlan0 (index 44) station, wiphy 0, 02:11:22:33:44:55, 5180 MHz\n") != NULL);
  CHECK(strstr(text, "interface: (no netdev) (index 0) p2p-device") != NULL);
  CHECK(strstr(text, "p2p: monitor mode: yes (beside the station: no; active monitor, with ACKs: no;") != NULL);
  CHECK(strstr(text, "on the station: send yes, receive yes, off channel too\n") != NULL);
  CHECK(strstr(text, "p2p: Wi-Fi Aware (NAN) through nl80211: no") != NULL && strstr(text, "p2p: Wi-Fi Direct: yes") != NULL);
  CHECK(strstr(text, "only the vendor's own AWDL can carry it") != NULL && !strstr(text, "Apple's OUI 00:17:f2 [I"));
  // A short buffer: cut, never past its end, and the length it needed is still given.
  char small[40];
  CHECK(br_wifi_caps_report(&c, small, sizeof small) == n && strlen(small) == sizeof small - 1);
  // Cut and broken input reads what came before the cut and nothing after it.
  br_wifi_caps cut;
  memset(&cut, 0, sizeof cut);
  br_wifi_caps_wiphy(&cut, b, 3);
  uint8_t bad[8] = { 2, 0, 1, 0, 0, 0, 0, 0 };              // an attribute shorter than its header
  br_wifi_caps_wiphy(&cut, bad, sizeof bad);
  br_wifi_caps_iface(&cut, bad, sizeof bad);
  CHECK(!cut.have_wiphy && cut.iface_count == 0);
  // A monitor beside the station, NAN, and a command under Apple's OUI change the verdicts.
  c.combinations[1].max_interfaces = 2; c.combinations[1].limit_max[0] = 2;
  c.iftypes |= 1u << NANIF; c.cmd_start_nan = true;
  c.commands[c.command_count++] = (br_wifi_vendor_cmd){ 0x0017f2, 1 };
  br_wifi_caps_report(&c, text, sizeof text);
  CHECK(strstr(text, "beside the station: yes") != NULL && strstr(text, "(NAN) through nl80211: yes") != NULL);
  CHECK(strstr(text, "Apple's OUI 00:17:f2 [I") != NULL && strstr(text, "a monitor interface beside the station is offered") != NULL);
}

static void test_groups_segments(void)   // the netlink groups, the cut of coalesced frames
{
  // A netlink multicast group as the kernel writes it (the id, then the name) and the other way round; another
  // name; a name that only begins with the wanted one; cut short.
  static const uint8_t scan_group[] = { 8, 0, 2, 0, 3, 0, 0, 0, 9, 0, 1, 0, 's', 'c', 'a', 'n', 0, 0, 0, 0 };
  static const uint8_t name_first[] = { 9, 0, 1, 0, 's', 'c', 'a', 'n', 0, 0, 0, 0, 8, 0, 2, 0, 7, 0, 0, 0 };
  static const uint8_t regulatory[] = { 8, 0, 2, 0, 4, 0, 0, 0, 15, 0, 1, 0, 'r', 'e', 'g', 'u', 'l', 'a', 't', 'o', 'r', 'y', 0, 0 };
  static const uint8_t scanner[] = { 8, 0, 2, 0, 5, 0, 0, 0, 12, 0, 1, 0, 's', 'c', 'a', 'n', 'n', 'e', 'r', 0 };
  uint32_t id = 0;
  CHECK(br_wifi_nl_group(scan_group, sizeof scan_group, "scan", &id) && id == 3);
  CHECK(br_wifi_nl_group(name_first, sizeof name_first, "scan", &id) && id == 7);
  CHECK(!br_wifi_nl_group(regulatory, sizeof regulatory, "scan", &id) && !br_wifi_nl_group(scanner, sizeof scanner, "scan", &id));
  CHECK(br_wifi_nl_group(regulatory, sizeof regulatory, "regulatory", &id) && id == 4);
  for (size_t cut = 0; cut < sizeof scan_group - 3; cut++) CHECK(!br_wifi_nl_group(scan_group, cut, "scan", &id));

  static uint8_t big[14 + 40 + 20 + 4000];
  static sink s;
  // A frame of the link's size goes as it is.
  size_t n = make_tcp(big, false, 1000, 0x18);
  memset(&s, 0, sizeof s);
  CHECK(br_wifi_segment(big, n, 1448, false, take, &s) == 1 && s.count == 1 && s.length[0] == n && !memcmp(s.frame[0], big, n));
  // A coalesced IPv4 frame of 4000 bytes: three segments, lengths, sequence numbers, identifiers, flags, checksums.
  n = make_tcp(big, false, 4000, 0x19);                                    // FIN PSH ACK
  memset(&s, 0, sizeof s);
  CHECK(br_wifi_segment(big, n, 1448, true, take, &s) == 3 && s.count == 3);
  CHECK(s.length[0] == 54 + 1448 && s.length[1] == 54 + 1448 && s.length[2] == 54 + 1104);
  CHECK(tcp4_ok(s.frame[0], s.length[0]) && tcp4_ok(s.frame[1], s.length[1]) && tcp4_ok(s.frame[2], s.length[2]));
  CHECK(s.frame[1][38] == 0x00 && s.frame[1][39] == 0x01 && s.frame[1][40] == 0x04 && s.frame[1][41] == 0xa8);   // 0xff00 + 1448
  CHECK(s.frame[0][47] == 0x10 && s.frame[1][47] == 0x10 && s.frame[2][47] == 0x19);                             // FIN and PSH on the last
  CHECK(s.frame[0][19] == 0x34 && s.frame[1][19] == 0x35 && s.frame[2][19] == 0x36);
  CHECK(!memcmp(s.frame[2] + 54, big + 54 + 2896, 1104));
  // A larger MSS than the link holds is cut to the link.
  memset(&s, 0, sizeof s);
  CHECK(br_wifi_segment(big, n, 9000, true, take, &s) == 3 && s.length[0] == 1514);
  // IPv6.
  n = make_tcp(big, true, 3000, 0x10);
  memset(&s, 0, sizeof s);
  CHECK(br_wifi_segment(big, n, 1428, true, take, &s) == 3 && s.length[2] == 74 + 144 && tcp6_ok(s.frame[0], s.length[0]) && tcp6_ok(s.frame[2], s.length[2]));
  // Coalesced but of the link's size: one frame, its checksum made (the receive path left none).
  n = make_tcp(big, false, 900, 0x18);
  memset(&s, 0, sizeof s);
  CHECK(br_wifi_segment(big, n, 1448, true, take, &s) == 1 && s.length[0] == n && tcp4_ok(s.frame[0], n));
  // Too long and not TCP: dropped. Of the link's size and not TCP: as it is.
  big[14 + 9] = 17;
  memset(&s, 0, sizeof s);
  CHECK(br_wifi_segment(big, 3000, 1448, true, take, &s) == 0 && s.count == 0);
  CHECK(br_wifi_segment(big, 600, 1448, true, take, &s) == 1 && s.count == 1 && !memcmp(s.frame[0], big, 600));
}

// ---- the station against a fake radio ----
typedef struct { unsigned scans, joins, leaves; int join_rc; struct ow_join join; bool had_bss; br_wifi_bss bss; br_wifi_link link; } fake_radio;
static int fr_scan(void *ctx) { ((fake_radio *)ctx)->scans++; return 0; }
static int fr_join(void *ctx, const struct ow_join *j, const br_wifi_bss *bss)
{
  fake_radio *r = ctx;
  r->joins++; r->join = *j; r->had_bss = bss != NULL;
  if (bss) r->bss = *bss;
  return r->join_rc;
}
static int fr_leave(void *ctx) { ((fake_radio *)ctx)->leaves++; return 0; }
static int fr_link(void *ctx, br_wifi_link *out) { *out = ((fake_radio *)ctx)->link; return 0; }
static const br_wifi_radio fake = { fr_scan, fr_join, fr_leave, fr_link, NULL };

static size_t guest_frame(uint8_t *f, uint16_t type, const void *payload, uint16_t n)
{
  struct ow_header h = { OW_MAGIC, type, n };
  memcpy(f, ow_address, 6); memcpy(f + 6, mac, 6); f[12] = 0x88; f[13] = 0xb5;
  memcpy(f + 14, &h, sizeof h);
  if (n) memcpy(f + 22, payload, n);
  return 22u + n;
}
// The message of host frame i: its type; *payload its bytes.
static unsigned host_message(const sink *s, unsigned i, const uint8_t **payload, size_t *n)
{
  struct ow_header h;
  if (i >= s->count || s->length[i] < 22 || memcmp(s->frame[i], mac, 6) || memcmp(s->frame[i] + 6, ow_address, 6)) return 0;
  memcpy(&h, s->frame[i] + 14, sizeof h);
  if (h.magic != OW_MAGIC || 22u + h.length != s->length[i]) return 0;
  *payload = s->frame[i] + 22; *n = h.length;
  return h.type;
}

static void test_station(void)
{
  static sink s;
  fake_radio r = { 0 };
  uint8_t f[256];
  const uint8_t *p = f;
  size_t n = 0;
  struct ow_state st;
  br_wifi_station *sta = br_wifi_station_create(&fake, &r, mac, NULL, take, &s);
  CHECK(sta != NULL);
  struct ow_hello hello = { OW_VERSION };
  br_wifi_station_message(sta, f, guest_frame(f, OW_HELLO, &hello, sizeof hello));
  CHECK(s.count == 1 && host_message(&s, 0, &p, &n) == OW_STATE && n == sizeof st);
  memcpy(&st, p, sizeof st);
  CHECK(st.version == OW_VERSION && st.radio == OW_RADIO_OK && st.link == OW_LINK_IDLE && !memcmp(st.mac, mac, 6) && st.country[0] == '0' && st.country[1] == '0');

  // A scan: two networks, the second seen twice (the stronger one is the join's), elements cut to whole ones.
  memset(&s, 0, sizeof s);
  br_wifi_station_message(sta, f, guest_frame(f, OW_SCAN, NULL, 0));
  br_wifi_station_message(sta, f, guest_frame(f, OW_SCAN, NULL, 0));       // a second request while one runs: one scan
  CHECK(r.scans == 1 && s.count == 0);
  static uint8_t ies[1100];
  ies[0] = 0; ies[1] = 4; memcpy(ies + 2, "home", 4);                      // SSID
  ies[6] = 7; ies[7] = 20; ies[8] = 'I'; ies[9] = 'N';                     // Country (20 bytes; the RSN element of a real beacon comes after it)
  for (unsigned at = 28; at + 102 <= sizeof ies; at += 102) { ies[at] = 221; ies[at + 1] = 100; }   // vendor elements up to 1048
  struct ow_bss b = { .bssid = { 1, 2, 3, 4, 5, 6 }, .freq_mhz = 5180, .signal_dbm = -70, .ssid_length = 4, .ssid = "home" };
  br_wifi_scan_add(sta, &b, ies, 1048);
  b.bssid[5] = 7; b.signal_dbm = -50; b.freq_mhz = 2437;
  br_wifi_scan_add(sta, &b, ies, 28);
  struct ow_bss other = { .bssid = { 9, 9, 9, 9, 9, 9 }, .freq_mhz = 2412, .signal_dbm = -80, .ssid_length = 5, .ssid = "other" };
  br_wifi_scan_add(sta, &other, NULL, 0);
  br_wifi_scan_done(sta, 0);
  // Five frames: three networks, the state with the country the strongest network's Country element names, the end.
  CHECK(s.count == 5 && host_message(&s, 3, &p, &n) == OW_STATE);
  memcpy(&st, p, sizeof st);
  CHECK(st.country[0] == 'I' && st.country[1] == 'N');
  CHECK(host_message(&s, 0, &p, &n) == OW_BSS && n == sizeof b + 946);     // 28 + 9 * 102: the tenth does not fit 1024
  struct ow_bss got;
  memcpy(&got, p, sizeof got);
  CHECK(got.ie_length == 946 && got.freq_mhz == 5180 && !memcmp(p + sizeof got, ies, 946));
  CHECK(host_message(&s, 2, &p, &n) == OW_BSS && n == sizeof b);
  struct ow_scan_done done;
  CHECK(host_message(&s, 4, &p, &n) == OW_SCAN_DONE && n == sizeof done);
  memcpy(&done, p, sizeof done);
  CHECK(done.count == 3 && done.error == 0);

  // A join: the radio gets the request and the stronger record; the result follows the network's state.
  memset(&s, 0, sizeof s);
  struct ow_join j = { .ssid_length = 4, .ssid = "home", .security = OW_SECURITY_AUTO, .key_length = 8, .key = "password" };
  br_wifi_station_message(sta, f, guest_frame(f, OW_JOIN, &j, sizeof j));
  CHECK(r.joins == 1 && r.had_bss && r.bss.bss.bssid[5] == 7 && r.join.key_length == 8 && !memcmp(r.join.key, "password", 8) && s.count == 0);
  r.link = (br_wifi_link){ .up = true, .rssi_dbm = -48, .noise_dbm = -95, .tx_rate_mbps = 433, .freq_mhz = 2437, .bssid = { 1, 2, 3, 4, 5, 7 } };
  br_wifi_join_done(sta, OW_JOIN_OK, 0);
  CHECK(s.count == 2 && host_message(&s, 0, &p, &n) == OW_STATE && n == sizeof st + 28);
  memcpy(&st, p, sizeof st);
  CHECK(st.link == OW_LINK_UP && st.rssi_dbm == -48 && st.tx_rate_mbps == 433 && st.bss.freq_mhz == 2437 && st.bss.ssid_length == 4 && st.bss.ie_length == 28);
  struct ow_join_result result;
  CHECK(host_message(&s, 1, &p, &n) == OW_JOIN_RESULT && n == sizeof result);
  memcpy(&result, p, sizeof result);
  CHECK(result.status == OW_JOIN_OK);
  br_wifi_join_done(sta, OW_JOIN_OK, 0);                                   // a second result: none is pending
  CHECK(s.count == 2);

  // The link drops: the state says idle and why.
  memset(&s, 0, sizeof s);
  r.link = (br_wifi_link){ .up = false, .reason = 4 };
  br_wifi_link_changed(sta);
  CHECK(s.count == 1 && host_message(&s, 0, &p, &n) == OW_STATE && n == sizeof st);
  memcpy(&st, p, sizeof st);
  CHECK(st.link == OW_LINK_IDLE && st.reason == 4);

  // A network no scan saw; the radio refuses: the result says so.
  memset(&s, 0, sizeof s);
  struct ow_join hidden = { .ssid_length = 6, .ssid = "hidden", .security = OW_SECURITY_OPEN };
  r.join_rc = -ENOENT;
  br_wifi_station_message(sta, f, guest_frame(f, OW_JOIN, &hidden, sizeof hidden));
  CHECK(r.joins == 2 && !r.had_bss && s.count == 1 && host_message(&s, 0, &p, &n) == OW_JOIN_RESULT);
  memcpy(&result, p, sizeof result);
  CHECK(result.status == OW_JOIN_NO_NETWORK);
  // A failed join reports its code.
  memset(&s, 0, sizeof s);
  r.join_rc = 0;
  br_wifi_station_message(sta, f, guest_frame(f, OW_JOIN, &j, sizeof j));
  br_wifi_join_done(sta, OW_JOIN_AUTH, 15);
  CHECK(s.count == 1 && host_message(&s, 0, &p, &n) == OW_JOIN_RESULT);
  memcpy(&result, p, sizeof result);
  CHECK(result.status == OW_JOIN_AUTH && result.code == 15);
  // Leave.
  memset(&s, 0, sizeof s);
  br_wifi_station_message(sta, f, guest_frame(f, OW_LEAVE, NULL, 0));
  CHECK(r.leaves == 1 && s.count == 1 && host_message(&s, 0, &p, &n) == OW_STATE);
  // iOS's Wi-Fi switch: on changes nothing, off leaves the network.
  struct ow_power power = { 1 };
  br_wifi_station_message(sta, f, guest_frame(f, OW_POWER, &power, sizeof power));
  CHECK(r.leaves == 1 && s.count == 1);
  power.on = 0;
  br_wifi_station_message(sta, f, guest_frame(f, OW_POWER, &power, sizeof power));
  CHECK(r.leaves == 2 && s.count == 2 && host_message(&s, 1, &p, &n) == OW_STATE);
  // Malformed: a short frame, a wrong magic, a length past the frame, a join with no name.
  memset(&s, 0, sizeof s);
  br_wifi_station_message(sta, f, 10);
  n = guest_frame(f, OW_SCAN, NULL, 0); f[14] ^= 1;
  br_wifi_station_message(sta, f, n);
  n = guest_frame(f, OW_JOIN, &j, sizeof j);
  br_wifi_station_message(sta, f, n - 1);
  struct ow_join unnamed = { 0 };
  br_wifi_station_message(sta, f, guest_frame(f, OW_JOIN, &unnamed, sizeof unnamed));
  CHECK(s.count == 0 && r.joins == 3 && r.scans == 1 && r.leaves == 2);
  br_wifi_station_destroy(sta);
}

// ---- the made-up radio (wifi=sim) through its back-end calls ----
static void test_sim(void)
{
  static sink s;
  uint8_t f[256];
  const uint8_t *p = f;
  size_t n = 0;
  br_net_open_fn open_fn = br_wifi_sim_prepare(mac);
  br_net *net = open_fn ? open_fn(NULL, take, &s) : NULL;
  CHECK(net != NULL);
  if (!net) return;
  struct ow_hello hello = { OW_VERSION };
  CHECK(br_wifi_sim_send(net, f, guest_frame(f, OW_HELLO, &hello, sizeof hello)) == 0 && s.count == 2 && host_message(&s, 0, &p, &n) == OW_STATE);
  // Then the radio's channels: 13 of 2.4 GHz and 25 of 5 GHz, the DFS ones passive and radar.
  CHECK(host_message(&s, 1, &p, &n) == OW_CHANNELS && n == sizeof(struct ow_channels) + 38 * sizeof(struct ow_channel));
  if (n >= sizeof(struct ow_channels) + 38 * sizeof(struct ow_channel)) {
    struct ow_channels head; struct ow_channel first, dfs, last;
    memcpy(&head, p, sizeof head);
    memcpy(&first, p + sizeof head, sizeof first);
    memcpy(&dfs, p + sizeof head + 17 * sizeof dfs, sizeof dfs);              // the 18th: 5 GHz channel 52
    memcpy(&last, p + sizeof head + 37 * sizeof last, sizeof last);
    CHECK(head.count == 38 && first.freq_mhz == 2412 && first.flags == 0 && dfs.freq_mhz == 5260 && dfs.flags == (OW_CH_PASSIVE | OW_CH_RADAR) &&
          last.freq_mhz == 5825 && last.flags == 0);
  }
  memset(&s, 0, sizeof s);
  br_wifi_sim_send(net, f, guest_frame(f, OW_SCAN, NULL, 0));
  struct ow_bss b;
  CHECK(s.count == 3 && host_message(&s, 0, &p, &n) == OW_BSS && host_message(&s, 2, &p, &n) == OW_SCAN_DONE);
  CHECK(host_message(&s, 1, &p, &n) == OW_BSS);
  memcpy(&b, p, sizeof b);
  br_wifi_security sec;
  br_wifi_security_parse(p + sizeof b, b.ie_length, &sec);
  CHECK(b.ssid_length == 11 && !memcmp(b.ssid, "OneiOS WPA2", 11) && sec.rsn && sec.psk && sec.ccmp && (b.capability & 0x10));
  // The secured network: a wrong passphrase is a refused key, the right one joins; a data frame goes nowhere.
  struct ow_join j = { .ssid_length = 11, .ssid = "OneiOS WPA2", .security = OW_SECURITY_AUTO, .key_length = 8, .key = "12345678" };
  struct ow_join_result result;
  struct ow_state st;
  memset(&s, 0, sizeof s);
  br_wifi_sim_send(net, f, guest_frame(f, OW_JOIN, &j, sizeof j));
  CHECK(s.count == 1 && host_message(&s, 0, &p, &n) == OW_JOIN_RESULT);
  memcpy(&result, p, sizeof result);
  CHECK(result.status == OW_JOIN_AUTH && result.code == 15);
  memcpy(j.key, "password", 8);
  memset(&s, 0, sizeof s);
  br_wifi_sim_send(net, f, guest_frame(f, OW_JOIN, &j, sizeof j));
  CHECK(s.count == 2 && host_message(&s, 0, &p, &n) == OW_STATE);
  memcpy(&st, p, sizeof st);
  CHECK(st.link == OW_LINK_UP && st.bss.freq_mhz == 5180 && st.rssi_dbm == -55);
  CHECK(host_message(&s, 1, &p, &n) == OW_JOIN_RESULT);
  memcpy(&result, p, sizeof result);
  CHECK(result.status == OW_JOIN_OK);
  memset(&s, 0, sizeof s);
  memset(f, 0xff, 60);
  CHECK(br_wifi_sim_send(net, f, 60) == 0 && s.count == 0);
  struct ow_join open = { .ssid_length = 11, .ssid = "OneiOS open", .security = OW_SECURITY_OPEN };
  br_wifi_sim_send(net, f, guest_frame(f, OW_JOIN, &open, sizeof open));
  CHECK(s.count == 2 && host_message(&s, 1, &p, &n) == OW_JOIN_RESULT);
  memcpy(&result, p, sizeof result);
  CHECK(result.status == OW_JOIN_OK);
  memset(&s, 0, sizeof s);
  br_wifi_sim_send(net, f, guest_frame(f, OW_LEAVE, NULL, 0));
  CHECK(s.count == 1 && host_message(&s, 0, &p, &n) == OW_STATE);
  memcpy(&st, p, sizeof st);
  CHECK(st.link == OW_LINK_IDLE);
}

// ---- the parcels ----
static void test_parcels(void)
{
  static ab_parcel p;
  sup_parcel_get_service(&p, "ab/c");
  static const uint8_t token[] = { 0, 0, 0, 0x80, 0xff, 0xff, 0xff, 0xff, 'T', 'S', 'Y', 'S', 26, 0, 0, 0, 'a', 0, 'n', 0, 'd', 0, 'r', 0 };
  CHECK(!p.overflow && p.nobjects == 0 && !memcmp(p.data, token, sizeof token));
  // "android.os.IServiceManager": 26 units and a terminator, 54 bytes padded to 56; then "ab/c": 4 units, 10 bytes padded to 12
  static const uint8_t name[] = { 4, 0, 0, 0, 'a', 0, 'b', 0, '/', 0, 'c', 0, 0, 0, 0, 0 };
  CHECK(p.size == 12 + 4 + 56 + 16 && !memcmp(p.data + 72, name, sizeof name));
  ab_reader r;
  char text[64];
  ab_reader_init(&r, p.data, p.size, NULL, 0);
  CHECK(ab_get_token(&r, SUP_MANAGER) && ab_get_string(&r, text, sizeof text) && !strcmp(text, "ab/c") && !r.bad && r.position == p.size);
  ab_reader_init(&r, p.data, p.size, NULL, 0);
  CHECK(!ab_get_token(&r, SUP_ISUPPLICANT));

  // A callback object: the kernel's object, listed, then the stability word.
  sup_parcel_callback(&p, 0x1122334455667788ull);
  size_t at = 12 + 4 + (((strlen(SUP_STA_IFACE) + 1) * 2 + 3) & ~3u);
  static const uint8_t object[] = { 0x85, 0x2a, 0x62, 0x73, 0x13, 0x01, 0, 0, 0x88, 0x77, 0x66, 0x55, 0x44, 0x33, 0x22, 0x11,
                                    0x88, 0x77, 0x66, 0x55, 0x44, 0x33, 0x22, 0x11, 0x3f, 0, 0, 0 };
  CHECK(p.nobjects == 1 && p.offsets[0] == at && p.size == at + 28 && !memcmp(p.data + at, object, sizeof object));
  // As its receiver sees it: a handle. Not listed in the offsets: refused.
  uint8_t copy[256];
  memcpy(copy, p.data, p.size);
  uint32_t handle_type = AB_TYPE_HANDLE, handle = 7, got = 0;
  memcpy(copy + at, &handle_type, 4); memcpy(copy + at + 8, &handle, 4); memset(copy + at + 12, 0, 12);
  ab_reader_init(&r, copy, p.size, p.offsets, 8);
  CHECK(ab_get_token(&r, SUP_STA_IFACE) && ab_get_binder(&r, &got, NULL) && got == 7 && !r.bad);
  ab_reader_init(&r, copy, p.size, NULL, 0);
  CHECK(ab_get_token(&r, SUP_STA_IFACE) && !ab_get_binder(&r, &got, NULL) && r.bad);
  // A null binder: not listed, not an error.
  ab_init(&p);
  ab_put_ok(&p);
  ab_put_null_binder(&p);
  ab_reader_init(&r, p.data, p.size, NULL, 0);
  CHECK(p.nobjects == 0 && ab_get_status(&r, NULL) == 0 && !ab_get_binder(&r, &got, NULL) && !r.bad && got == 0);

  // byte[] and int, a key wiped.
  sup_parcel_bytes(&p, SUP_STA_NETWORK, (const uint8_t *)"secret-key", 10);
  at = p.size - 16;
  CHECK(p.data[at] == 10 && !memcmp(p.data + at + 4, "secret-key\0\0", 12));
  ab_wipe(&p);
  CHECK(p.size == 0 && !memchr(p.data, 's', sizeof p.data));
  sup_parcel_i32(&p, SUP_STA_NETWORK, KM_WPA_PSK | KM_WPA_PSK_SHA256);
  CHECK(p.data[p.size - 4] == 0x02 && p.data[p.size - 3] == 0x01);

  // A reply's status: ok; a service's error; an exception with a stack trace header; a truncated one.
  ab_init(&p);
  ab_put_service_error(&p, 4);
  ab_put_i32(&p, 77);
  int32_t specific = 0;
  ab_reader_init(&r, p.data, p.size, NULL, 0);
  CHECK(ab_get_status(&r, &specific) == AB_EX_SERVICE_SPECIFIC && specific == 4 && ab_get_i32(&r) == 77 && !r.bad);
  ab_init(&p);
  ab_put_i32(&p, -3); ab_put_string(&p, "bad"); ab_put_i32(&p, 12); ab_put_i32(&p, 1); ab_put_i32(&p, 2); ab_put_i32(&p, 99);
  ab_reader_init(&r, p.data, p.size, NULL, 0);
  CHECK(ab_get_status(&r, &specific) == -3 && specific == -3 && ab_get_i32(&r) == 99);
  ab_reader_init(&r, p.data, 6, NULL, 0);
  (void)ab_get_status(&r, &specific);
  CHECK(r.bad);
  // Strings: a null one, one cut to the buffer, one whose length is past the parcel.
  ab_init(&p);
  ab_put_string(&p, NULL); ab_put_string(&p, "abcdefgh"); ab_put_i32(&p, 1000);
  ab_reader_init(&r, p.data, p.size, NULL, 0);
  CHECK(ab_get_string(&r, text, sizeof text) && !*text && ab_get_string(&r, text, 5) && !strcmp(text, "abcd") && !ab_get_string(&r, text, sizeof text) && r.bad);
  // The standard answers.
  CHECK(ab_standard(AB_INTERFACE, SUP_STA_CALLBACK, 2, SUP_HASH, &p) && p.data[0] == strlen(SUP_STA_CALLBACK));
  CHECK(ab_standard(AB_GET_VERSION, SUP_STA_CALLBACK, 2, SUP_HASH, &p) && p.size == 8 && p.data[4] == 2);
  CHECK(ab_standard(AB_GET_HASH, SUP_STA_CALLBACK, 2, SUP_HASH, &p) && p.data[4] == 40 && ab_standard(AB_PING, "", 0, "", &p) && p.size == 0);
  CHECK(!ab_standard(CB_DISCONNECTED, SUP_STA_CALLBACK, 2, SUP_HASH, &p));
  // An overflow is kept.
  ab_init(&p);
  static uint8_t much[5000];
  ab_put_bytes(&p, much, sizeof much);
  ab_put_i32(&p, 1);
  CHECK(p.overflow);
}

// ---- what a network asks for ----
static size_t rsn(uint8_t *out, const uint8_t *pairwise, unsigned npairwise, const uint8_t *akm, unsigned nakm, uint16_t caps)
{
  size_t n = 2;
  out[0] = 48;
  out[n++] = 1; out[n++] = 0;
  memcpy(out + n, "\x00\x0f\xac\x04", 4); n += 4;
  out[n++] = (uint8_t)npairwise; out[n++] = 0;
  for (unsigned i = 0; i < npairwise; i++) { memcpy(out + n, "\x00\x0f\xac", 3); out[n + 3] = pairwise[i]; n += 4; }
  out[n++] = (uint8_t)nakm; out[n++] = 0;
  for (unsigned i = 0; i < nakm; i++) { memcpy(out + n, "\x00\x0f\xac", 3); out[n + 3] = akm[i]; n += 4; }
  out[n++] = (uint8_t)caps; out[n++] = (uint8_t)(caps >> 8);
  out[1] = (uint8_t)(n - 2);
  return n;
}

static void test_security(void)
{
  uint8_t ies[128];
  br_wifi_security s;
  br_wifi_plan plan;
  size_t n = 6;
  memcpy(ies, "\x00\x04home", 6);
  struct ow_join pass = { .ssid_length = 4, .ssid = "home", .security = OW_SECURITY_AUTO, .key_length = 8, .key = "password" };
  struct ow_join raw = { .ssid_length = 4, .ssid = "home", .security = OW_SECURITY_AUTO, .key_length = 32, .key_raw = 1 };
  struct ow_join open = { .ssid_length = 4, .ssid = "home", .security = OW_SECURITY_OPEN };
  // WPA2: CCMP, PSK.
  n = 6 + rsn(ies + 6, (const uint8_t[]){ 4 }, 1, (const uint8_t[]){ 2 }, 1, 0x000c);
  br_wifi_security_parse(ies, n, &s);
  CHECK(s.rsn && s.psk && s.ccmp && !s.sae && !s.wpa && !s.tkip && !s.pmf_capable && !s.pmf_required);
  CHECK(br_wifi_plan_join(&pass, &s, &plan) == 0 && plan.key_mgmt == (KM_WPA_PSK | KM_WPA_PSK_SHA256) && plan.proto == PROTO_RSN && plan.psk_passphrase &&
        !plan.psk_raw && !plan.sae_password && !plan.set_pmf && !plan.scan_ssid && plan.pairwise == (PC_CCMP | PC_GCMP_128 | PC_GCMP_256));
  CHECK(br_wifi_plan_join(&raw, &s, &plan) == 0 && plan.psk_raw && !plan.psk_passphrase && plan.key_mgmt == (KM_WPA_PSK | KM_WPA_PSK_SHA256));
  // WPA3 only: SAE, management frame protection required.
  n = 6 + rsn(ies + 6, (const uint8_t[]){ 4 }, 1, (const uint8_t[]){ 8 }, 1, 0x00cc);
  br_wifi_security_parse(ies, n, &s);
  CHECK(s.sae && !s.psk && s.pmf_capable && s.pmf_required);
  CHECK(br_wifi_plan_join(&pass, &s, &plan) == 0 && plan.key_mgmt == KM_SAE && plan.sae_password && plan.set_pmf && plan.require_pmf && !plan.psk_passphrase);
  CHECK(br_wifi_plan_join(&raw, &s, &plan) == -EINVAL);                    // a WPA2 key cannot open it
  // WPA2/WPA3 transition: the passphrase as WPA2.
  n = 6 + rsn(ies + 6, (const uint8_t[]){ 4 }, 1, (const uint8_t[]){ 2, 8 }, 2, 0x008c);
  br_wifi_security_parse(ies, n, &s);
  CHECK(s.sae && s.psk && s.pmf_capable && !s.pmf_required);
  CHECK(br_wifi_plan_join(&pass, &s, &plan) == 0 && plan.psk_passphrase && plan.key_mgmt == (KM_WPA_PSK | KM_WPA_PSK_SHA256) && !plan.set_pmf);
  // WPA (version 1) with TKIP beside WPA2: both protocols and TKIP allowed.
  n = 6 + rsn(ies + 6, (const uint8_t[]){ 2, 4 }, 2, (const uint8_t[]){ 2 }, 1, 0);
  static const uint8_t wpa1[] = { 221, 22, 0x00, 0x50, 0xf2, 1, 1, 0, 0x00, 0x50, 0xf2, 2, 1, 0, 0x00, 0x50, 0xf2, 2, 1, 0, 0x00, 0x50, 0xf2, 2 };
  memcpy(ies + n, wpa1, sizeof wpa1);
  br_wifi_security_parse(ies, n + sizeof wpa1, &s);
  CHECK(s.rsn && s.wpa && s.tkip && s.ccmp && s.psk);
  CHECK(br_wifi_plan_join(&pass, &s, &plan) == 0 && plan.proto == (PROTO_RSN | PROTO_WPA) && (plan.pairwise & PC_TKIP) && (plan.group & GC_TKIP));
  // Open; enhanced open; no record of the network (hidden).
  br_wifi_security_parse(ies, 6, &s);
  CHECK(!s.rsn && !s.wpa && br_wifi_plan_join(&open, &s, &plan) == 0 && plan.key_mgmt == KM_NONE && !plan.proto && !plan.scan_ssid);
  n = 6 + rsn(ies + 6, (const uint8_t[]){ 4 }, 1, (const uint8_t[]){ 18 }, 1, 0x00cc);
  br_wifi_security_parse(ies, n, &s);
  CHECK(s.owe && br_wifi_plan_join(&open, &s, &plan) == 0 && plan.key_mgmt == KM_OWE && plan.require_pmf);
  CHECK(br_wifi_plan_join(&pass, NULL, &plan) == 0 && plan.scan_ssid && plan.psk_passphrase && (plan.proto & PROTO_WPA));
  CHECK(br_wifi_plan_join(&open, NULL, &plan) == 0 && plan.scan_ssid && plan.key_mgmt == KM_NONE);
  // Elements cut short or with lengths past the end: nothing read past them.
  n = 6 + rsn(ies + 6, (const uint8_t[]){ 4 }, 1, (const uint8_t[]){ 2 }, 1, 0x000c);
  for (size_t cut = 0; cut < n; cut++) br_wifi_security_parse(ies, cut, &s);
  ies[6 + 2 + 6] = 200;                                                    // a pairwise count past the element
  br_wifi_security_parse(ies, n, &s);
  CHECK(s.rsn && !s.psk && !s.ccmp);
}

// ---- the supplicant's callbacks ----
static void test_callbacks(void)
{
  static ab_parcel p;
  ab_reader r;
  sup_event e;
  static const uint8_t bssid[6] = { 1, 2, 3, 4, 5, 6 };
  // onStateChanged(newState, bssid, id, ssid, filsHlpSent)
  ab_init(&p);
  ab_put_token(&p, SUP_STA_CALLBACK);
  ab_put_i32(&p, ST_FOURWAY_HANDSHAKE); ab_put_bytes(&p, bssid, 6); ab_put_i32(&p, 3); ab_put_bytes(&p, (const uint8_t *)"home", 4); ab_put_i32(&p, 0);
  ab_reader_init(&r, p.data, p.size, NULL, 0);
  CHECK(ab_get_token(&r, SUP_STA_CALLBACK));
  sup_decode_callback(CB_STATE_CHANGED, &r, &e);
  CHECK(e.kind == CB_STATE_CHANGED && e.state == ST_FOURWAY_HANDSHAKE && e.id == 3 && !memcmp(e.bssid, bssid, 6));
  // onSupplicantStateChanged(SupplicantStateChangeData)
  ab_init(&p);
  ab_put_i32(&p, 1);
  size_t at = p.size;
  ab_put_i32(&p, 0);
  ab_put_i32(&p, ST_COMPLETED); ab_put_i32(&p, 3); ab_put_bytes(&p, (const uint8_t *)"home", 4); ab_put_bytes(&p, bssid, 6);
  ab_put_i32(&p, KM_WPA_PSK); ab_put_i32(&p, 5180); ab_put_i32(&p, 0);
  int32_t size = (int32_t)(p.size - at);
  memcpy(p.data + at, &size, 4);
  ab_reader_init(&r, p.data, p.size, NULL, 0);
  sup_decode_callback(CB_SUPPLICANT_STATE_CHANGED, &r, &e);
  CHECK(e.kind == CB_SUPPLICANT_STATE_CHANGED && e.state == ST_COMPLETED && e.frequency == 5180 && e.id == 3 && !memcmp(e.bssid, bssid, 6));
  ab_reader_init(&r, p.data, p.size - 8, NULL, 0);                         // cut short: not used
  sup_decode_callback(CB_SUPPLICANT_STATE_CHANGED, &r, &e);
  CHECK(e.kind == 0);
  // onDisconnected(bssid, locallyGenerated, reasonCode)
  ab_init(&p);
  ab_put_bytes(&p, bssid, 6); ab_put_i32(&p, 1); ab_put_i32(&p, 3);
  ab_reader_init(&r, p.data, p.size, NULL, 0);
  sup_decode_callback(CB_DISCONNECTED, &r, &e);
  CHECK(e.kind == CB_DISCONNECTED && e.local && e.code == 3);
  // onAssociationRejected(AssociationRejectionData: ssid, bssid, statusCode, timedOut, ...)
  ab_init(&p);
  ab_put_i32(&p, 1);
  at = p.size;
  ab_put_i32(&p, 0);
  ab_put_bytes(&p, (const uint8_t *)"home", 4); ab_put_bytes(&p, bssid, 6); ab_put_i32(&p, 17); ab_put_i32(&p, 0);
  ab_put_i32(&p, 0); ab_put_i32(&p, 0); ab_put_i32(&p, 0);
  size = (int32_t)(p.size - at);
  memcpy(p.data + at, &size, 4);
  ab_reader_init(&r, p.data, p.size, NULL, 0);
  sup_decode_callback(CB_ASSOCIATION_REJECTED, &r, &e);
  CHECK(e.kind == CB_ASSOCIATION_REJECTED && e.code == 17 && !e.timed_out);
  // One the bridge does not use.
  sup_decode_callback(20, &r, &e);
  CHECK(e.kind == 0);
}

// ---- every radio, Wonder, AWDL's frames, `wifictl wonder` on a stand-in (wifi/p2p.c, wifi/wonder.c) ----
// One message of a split GET_WIPHY dump: its wiphy and name first (as cfg80211 writes every message), then the rest.
static size_t wiphy_msg(uint8_t *b, uint32_t wiphy, const char *name)
{
  size_t len = 0;
  nla(b, &len, 1, &wiphy, 4);
  nla(b, &len, 2, name, (uint16_t)(strlen(name) + 1));
  return len;
}
static void vendor_cmds(uint8_t *b, size_t *len, uint32_t oui, const uint32_t *subcmds, unsigned n)
{
  const size_t at = nla(b, len, 197, NULL, 0);
  for (unsigned i = 0; i < n; i++) { const uint32_t e[2] = { oui, subcmds[i] }; nla(b, len, (uint16_t)(i + 1), e, 8); }
  nla_end(b, *len, at);
}
static void iface_msg(br_wifi_radios *r, uint32_t ifindex, const char *name, uint32_t wiphy, uint32_t type, uint32_t freq)
{
  uint8_t b[128];
  size_t len = 0;
  nla(b, &len, 3, &ifindex, 4); nla(b, &len, 4, name, (uint16_t)(strlen(name) + 1)); nla(b, &len, 1, &wiphy, 4); nla(b, &len, 5, &type, 4);
  if (freq) nla(b, &len, 38, &freq, 4);
  br_wifi_radios_iface(r, b, len);
}
enum { T_ADHOC = 1, T_STA = 2, T_AP = 3, T_MON = 6, T_P2PC = 8, T_P2PGO = 9, T_NAN = 12 };
static const uint32_t wonder15[5] = { 1, 2, 3, 4, 5 };
// The phone as Samsung's source says it is: phy0 (qcacld: station, AP, monitor, P2P; Qualcomm's commands) over two
// messages, and Wonder 1.5 (wiphy 1, "wonder": ad-hoc, station, monitor, NAN; Google's 1 to 5) between them.
static void phone_radios(br_wifi_radios *r)
{
  memset(r, 0, sizeof *r);
  uint8_t b[1024];
  size_t len = wiphy_msg(b, 0, "phy0");
  iftype_flags(b, &len, 32, 1u << T_STA | 1u << T_AP | 1u << T_MON | 1u << T_P2PC | 1u << T_P2PGO);
  br_wifi_radios_wiphy(r, b, len);
  len = wiphy_msg(b, 1, "wonder");
  iftype_flags(b, &len, 32, 1u << T_ADHOC | 1u << T_STA | 1u << T_MON | 1u << T_NAN);
  vendor_cmds(b, &len, BR_WIFI_OUI_GOOGLE, wonder15, 5);
  br_wifi_radios_wiphy(r, b, len);
  len = wiphy_msg(b, 0, "phy0");                         // phy0's second message, after Wonder's
  const uint32_t qca[2] = { 12, 81 };
  vendor_cmds(b, &len, 0x001374, qca, 2);
  br_wifi_radios_wiphy(r, b, len);
  iface_msg(r, 44, "wlan0", 0, T_STA, 5180);
}

static char logs[16384];
static size_t logs_used;
static void log_line(int level, const char *line)
{
  const int n = snprintf(logs + logs_used, sizeof logs - logs_used, "%s%s\n", level == 0 ? "E " : "", line);
  if (n > 0 && logs_used + (size_t)n < sizeof logs) logs_used += (size_t)n;
}
static bool logged(const char *text) { return strstr(logs, text) != NULL; }

static void test_radios(void)
{
  static br_wifi_radios r;
  static char text[16384];
  phone_radios(&r);
  if (getenv("ONEIOS_TEST_SHOW")) { br_wifi_radios_report(&r, "wlan0", text, sizeof text); printf("---- wifictl p2p on the stand-in phone (Wonder 1.5 idle)\n%s", text); }
  CHECK(r.count == 2 && r.wiphys[0].wiphy == 0 && r.wiphys[1].wiphy == 1 && !strcmp(r.wiphys[1].name, "wonder"));
  CHECK(r.wiphys[0].command_count == 2 && r.wiphys[0].commands[1].subcmd == 81 && r.wiphys[1].command_count == 5);   // each message to its wiphy
  CHECK(r.wiphys[0].iftypes >> T_AP & 1 && !(r.wiphys[1].iftypes >> T_AP & 1));
  const br_wifi_caps *w = br_wifi_wonder(&r);
  CHECK(w == &r.wiphys[1] && br_wifi_caps_has_vendor(w, BR_WIFI_OUI_GOOGLE, 5) && !br_wifi_caps_has_vendor(w, BR_WIFI_OUI_GOOGLE, 6));
  CHECK(!strcmp(br_wifi_wonder_subcmd_name(4), "SET_REGULATORY") && !strcmp(br_wifi_wonder_subcmd_name(15), "SET_TX_RATE_TEST") &&
        !strcmp(br_wifi_wonder_subcmd_name(12), "?") && !strcmp(br_wifi_wonder_subcmd_name(99), "?"));
  size_t n = br_wifi_radios_report(&r, "wlan0", text, sizeof text);
  CHECK(n == strlen(text));
  CHECK(strstr(text, "radios: 2\nradio: phy0 (wiphy 0)\n") != NULL && strstr(text, "radio: wonder (wiphy 1)\n") != NULL);
  CHECK(strstr(text, "  001374 Qualcomm: 12 81\n  (wlan0's radio)\n") != NULL && strstr(text, "  001a11 Google: 1 2 3 4 5\n  (Google's Wonder radio)\n") != NULL);
  CHECK(strstr(text, "interface: wlan0 (index 44) station, wiphy 0, 00:00:00:00:00:00, 5180 MHz\n") != NULL);
  CHECK(strstr(text, "p2p: Wonder: wiphy 1 (wonder), interface types ad-hoc, station, monitor, nan\n") != NULL);
  CHECK(strstr(text, "p2p: Wonder: vendor subcommands under 00:1a:11: 1 SET_FREQUENCY 2 SET_FILTER 3 SET_FIXED_TX_RATE 4 SET_REGULATORY 5 GET_IF_MAC_ADDR\n") != NULL);
  CHECK(strstr(text, "p2p: Wonder: version 1.4 or 1.5 (subcommands 1 to 5 only") != NULL && strstr(text, "p2p: Wonder: idle (no interface on it, no wondertap vdev)\n") != NULL);
  CHECK(strstr(text, "p2p: for AirDrop iOS needs AWDL: Wonder's raw 802.11 path is offered (wiphy 1)") != NULL);
  CHECK(strstr(text, "p2p: monitor mode: yes (beside the station: no;") != NULL);   // the verdicts are phy0's (wlan0's), not Wonder's
  // Wonder in use: a monitor of ours left over, and the PASSTHRU vdev on the driver's own wiphy.
  iface_msg(&r, 52, "wondermon0", 1, T_MON, 0);
  iface_msg(&r, 51, "wondertap0", 0, T_MON, 2437);
  br_wifi_radios_report(&r, "wlan0", text, sizeof text);
  CHECK(strstr(text, "interface: wondermon0 (index 52) monitor, wiphy 1, 00:00:00:00:00:00 (on Wonder's radio)\n") != NULL);
  CHECK(strstr(text, "interface: wondertap0 (index 51) monitor, wiphy 0, 00:00:00:00:00:00, 2437 MHz (the Wi-Fi driver's PASSTHRU vdev for Wonder)\n") != NULL);
  CHECK(strstr(text, "p2p: Wonder: in use: wondermon0 (monitor): wifictl wonder's own") != NULL);
  CHECK(strstr(text, "p2p: Wonder: the PASSTHRU vdev wondertap0 (index 51, wiphy 0) is up on 2437 MHz\n") != NULL && !strstr(text, "idle"));
  // Wonder 1.6 or later: a channel schedule.
  r.wiphys[1].commands[r.wiphys[1].command_count++] = (br_wifi_vendor_cmd){ BR_WIFI_OUI_GOOGLE, 6 };
  br_wifi_radios_report(&r, "wlan0", text, sizeof text);
  CHECK(strstr(text, "6 SET_CHANNEL_SCHEDULE_REQ\n") != NULL && strstr(text, "p2p: Wonder: version 1.6 or later") != NULL);
  // No GET_IF_MAC_ADDR: a Wonder the session does not know.
  r.wiphys[1].command_count = 3;
  br_wifi_radios_report(&r, "wlan0", text, sizeof text);
  CHECK(strstr(text, "p2p: Wonder: version unknown") != NULL);

  // By its commands, not its name: a wiphy "phy1" with Google's commands and monitor mode and no AP is Wonder; one
  // with an AP (a Broadcom driver's own wiphy has Google's commands too) is not.
  memset(&r, 0, sizeof r);
  uint8_t b[512];
  size_t len = wiphy_msg(b, 0, "phy0");
  iftype_flags(b, &len, 32, 1u << T_STA | 1u << T_AP | 1u << T_MON);
  vendor_cmds(b, &len, BR_WIFI_OUI_GOOGLE, wonder15, 2);
  br_wifi_radios_wiphy(&r, b, len);
  CHECK(br_wifi_wonder(&r) == NULL);
  len = wiphy_msg(b, 3, "phy1");
  iftype_flags(b, &len, 32, 1u << T_MON | 1u << T_STA);
  vendor_cmds(b, &len, BR_WIFI_OUI_GOOGLE, wonder15, 5);
  br_wifi_radios_wiphy(&r, b, len);
  CHECK(br_wifi_wonder(&r) == &r.wiphys[1] && r.wiphys[1].wiphy == 3);
  // No Wonder at all, and the station's radio without a monitor beside it: the old verdict, Wonder named.
  memset(&r, 0, sizeof r);
  len = wiphy_msg(b, 0, "phy0");
  iftype_flags(b, &len, 32, 1u << T_STA | 1u << T_AP);
  br_wifi_radios_wiphy(&r, b, len);
  br_wifi_radios_report(&r, "wlan0", text, sizeof text);
  CHECK(strstr(text, "p2p: Wonder: no Wonder radio (") != NULL && strstr(text, "no Wonder radio and no monitor beside the station: only the vendor's own AWDL can carry it") != NULL);
  iface_msg(&r, 51, "wondertap0", 0, T_MON, 0);
  br_wifi_radios_report(&r, "wlan0", text, sizeof text);
  CHECK(strstr(text, "but a wondertap interface is up: its Wonder radio is not in the dump") != NULL);
  // A message that names no wiphy before any did is dropped; more wiphys than kept are counted; no radio at all.
  memset(&r, 0, sizeof r);
  len = 0;
  iftype_flags(b, &len, 32, 1u << T_STA);
  br_wifi_radios_wiphy(&r, b, len);
  CHECK(r.count == 0);
  for (uint32_t i = 0; i < BR_WIFI_WIPHYS + 2; i++) { len = wiphy_msg(b, i, "phy"); br_wifi_radios_wiphy(&r, b, len); }
  CHECK(r.count == BR_WIFI_WIPHYS && r.wiphys_dropped == 2);
  br_wifi_radios_report(&r, NULL, text, sizeof text);
  CHECK(strstr(text, "radios: 4 (more were not kept)\n") != NULL);
  memset(&r, 0, sizeof r);
  CHECK(br_wifi_radios_report(&r, "wlan0", text, sizeof text) == strlen("radio: none (nl80211 gave no wiphy)\n"));
  char small[16];
  phone_radios(&r);
  CHECK(br_wifi_radios_report(&r, "wlan0", small, sizeof small) > sizeof small && strlen(small) == sizeof small - 1);
}

// A frame as Wonder's monitor interface gives it: radiotap (TSFT, FLAGS, RATE, CHANNEL, DBM_ANTSIGNAL, padded to 24),
// then the 802.11 header and body.
static const uint8_t awdl_bss[6] = { 0x00, 0x25, 0x00, 0xff, 0x94, 0x73 };
static size_t radiotap(uint8_t *f, bool tsft, uint8_t flags, int8_t dbm)
{
  memset(f, 0, 24);
  const uint32_t present = (tsft ? 1u : 0u) | 1u << 1 | 1u << 2 | 1u << 3 | 1u << 5;
  memcpy(f + 4, &present, 4);
  size_t at = 8;
  if (tsft) at += 8;                                     // aligned at 8 already
  f[at++] = flags;
  f[at++] = 12;                                          // 6 Mb/s
  const uint16_t freq = 2437, chflags = 0x00a0;
  memcpy(f + at, &freq, 2); memcpy(f + at + 2, &chflags, 2); at += 4;
  f[at++] = (uint8_t)dbm;
  const size_t len = (at + 3) & ~(size_t)3;
  f[2] = (uint8_t)len;
  return len;
}
static size_t awdl_action(uint8_t *f, const uint8_t src[6], uint8_t subtype, int8_t dbm)
{
  size_t at = radiotap(f, true, 0, dbm);
  uint8_t *h = f + at;
  memset(h, 0, 24 + 16);
  h[0] = 0xd0; memset(h + 4, 0xff, 6); memcpy(h + 10, src, 6); memcpy(h + 16, awdl_bss, 6);
  const uint8_t body[8] = { 127, 0x00, 0x17, 0xf2, 8, 0x10, subtype, 0 };
  memcpy(h + 24, body, 8);
  return at + 24 + 16;
}

static void test_awdl_frames(void)
{
  const uint8_t a[6] = { 0x02, 0xaa, 0, 0, 0, 1 }, b2[6] = { 0x02, 0xbb, 0, 0, 0, 2 };
  uint8_t f[256];
  br_awdl_frame k;
  size_t n = awdl_action(f, a, 0, -40);
  CHECK(br_wifi_radiotap_frame(f, n, &k) && k.kind == BR_AWDL_ACTION && k.subtype == 0 && k.type == 0 && k.tsft && k.has_signal && k.signal_dbm == -40 &&
        !memcmp(k.source, a, 6) && !k.bad_fcs);
  static br_awdl_count c;
  memset(&c, 0, sizeof c);
  br_awdl_count_add(&c, f, n);                                       // PSF from a, -40 dBm
  n = awdl_action(f, a, 0, -35); br_awdl_count_add(&c, f, n);        // PSF from a, -35 dBm (its best)
  n = awdl_action(f, b2, 3, -70); br_awdl_count_add(&c, f, n);       // MIF from b
  n = awdl_action(f, b2, 6, -71); br_awdl_count_add(&c, f, n);       // another subtype
  n = awdl_action(f, b2, 0, -60);
  f[24 + 24 + 4] = 9;                                                // vendor action of Apple's with a type that is not AWDL
  CHECK(br_wifi_radiotap_frame(f, n, &k) && k.kind == BR_AWDL_OTHER_ACTION);
  br_awdl_count_add(&c, f, n);
  n = awdl_action(f, b2, 0, -60);
  f[24 + 24] = 4;                                                    // a public action frame
  br_awdl_count_add(&c, f, n);
  // An AWDL data frame (addr3 AWDL's BSSID), an ordinary data frame, an ACK, a beacon.
  size_t at = radiotap(f, true, 0, -50);
  memset(f + at, 0, 40);
  f[at] = 0x08; memcpy(f + at + 4, b2, 6); memcpy(f + at + 10, a, 6); memcpy(f + at + 16, awdl_bss, 6);
  CHECK(br_wifi_radiotap_frame(f, at + 40, &k) && k.kind == BR_AWDL_DATA && k.type == 2);
  br_awdl_count_add(&c, f, at + 40);
  f[at + 16] = 0x12;
  CHECK(br_wifi_radiotap_frame(f, at + 40, &k) && k.kind == BR_AWDL_NOT);
  br_awdl_count_add(&c, f, at + 40);
  f[at + 16] = 0x00; f[at + 1] = 0x02;                               // from DS: not AWDL's
  CHECK(br_wifi_radiotap_frame(f, at + 40, &k) && k.kind == BR_AWDL_NOT);
  f[at] = 0xd4; f[at + 1] = 0;                                       // an ACK: 10 bytes, control
  CHECK(br_wifi_radiotap_frame(f, at + 10, &k) && k.type == 1 && k.kind == BR_AWDL_NOT);
  br_awdl_count_add(&c, f, at + 10);
  f[at] = 0x80;                                                      // a beacon (management) cut at 20 bytes: not parsed
  CHECK(!br_wifi_radiotap_frame(f, at + 20, &k));
  br_awdl_count_add(&c, f, at + 20);
  // A bad FCS (flags 0x40) is counted alone; an FCS at the end (0x10) is cut off before the body is read.
  n = awdl_action(f, a, 0, -40);
  f[16] = 0x40;
  CHECK(br_wifi_radiotap_frame(f, n, &k) && k.bad_fcs);
  br_awdl_count_add(&c, f, n);
  n = awdl_action(f, a, 3, -40);
  f[16] = 0x10;
  memset(f + n, 0xee, 4);
  CHECK(br_wifi_radiotap_frame(f, n + 4, &k) && k.kind == BR_AWDL_ACTION && k.subtype == 3);
  br_awdl_count_add(&c, f, n + 4);                                   // MIF from a
  // Without TSFT; with a second present word (bit 31): the fields start after it.
  n = radiotap(f, false, 0, -45);
  CHECK(n == 16);
  memset(f + n, 0, 40);
  f[n] = 0xd0; memcpy(f + n + 10, b2, 6); memcpy(f + n + 24, "\x7f\x00\x17\xf2\x08\x10\x00", 7);
  CHECK(br_wifi_radiotap_frame(f, n + 40, &k) && !k.tsft && k.kind == BR_AWDL_ACTION && k.signal_dbm == -45);
  br_awdl_count_add(&c, f, n + 40);                                  // PSF from b, no TSFT
  uint8_t g[256];
  memset(g, 0, sizeof g);
  const uint32_t p0 = 1u << 1 | 1u << 31, p1 = 0;
  memcpy(g + 4, &p0, 4); memcpy(g + 8, &p1, 4);
  g[12] = 0x00;                                                      // FLAGS, after both words
  g[2] = 16;
  g[16] = 0xd0; memcpy(g + 16 + 10, a, 6); memcpy(g + 16 + 24, "\x7f\x00\x17\xf2\x08\x10\x03", 7);
  CHECK(br_wifi_radiotap_frame(g, 16 + 40, &k) && k.kind == BR_AWDL_ACTION && k.subtype == 3 && !k.has_signal);
  // Broken radiotap: another version, a length past the frame, a present word past the header.
  g[0] = 1; CHECK(!br_wifi_radiotap_frame(g, 56, &k)); g[0] = 0;
  g[2] = 200; CHECK(!br_wifi_radiotap_frame(g, 56, &k));
  g[2] = 8; CHECK(!br_wifi_radiotap_frame(g, 56, &k));
  CHECK(!br_wifi_radiotap_frame(g, 4, &k));
  br_awdl_count_add(&c, g, 4);
  CHECK(c.frames == 14 && c.unparsed == 2 && c.bad_fcs == 1 && c.control == 1 && c.data == 2 && c.management == 8);
  CHECK(c.awdl_action == 6 && c.awdl_psf == 3 && c.awdl_mif == 2 && c.awdl_other == 1 && c.action == 8 && c.awdl_data == 1 && c.with_tsft == 10);
  CHECK(c.peer_count == 2 && c.peers[0].action == 3 && c.peers[0].data == 1 && c.peers[0].best_dbm == -35 && c.peers[1].action == 3 && c.peers[1].data == 0 && c.peers[1].best_dbm == -45);
  static char text[4096];
  n = br_awdl_count_report(&c, 30, text, sizeof text);
  CHECK(n == strlen(text));
  CHECK(strstr(text, "frames: 14 in 30 s: 8 management (8 action), 1 control, 2 data; 2 not parsed, 1 with a bad FCS; 10 carry the radiotap TSFT\n") != NULL);
  CHECK(strstr(text, "AWDL: 6 action frames (3 PSF, 2 MIF, 1 other subtypes), 1 data frames to AWDL's BSSID, from 2 peer(s)\n") != NULL);
  CHECK(strstr(text, "AWDL peer: 02:aa:00:00:00:01: 3 action, 1 data, best -35 dBm\n") != NULL && strstr(text, "AWDL peer: 02:bb:00:00:00:02: 3 action, 0 data, best -45 dBm\n") != NULL);
  CHECK(strstr(text, "radiotap: 1 frames without the TSFT") != NULL && !strstr(text, "no AWDL frame"));
  // More peers than kept; nothing at all; frames but no AWDL.
  for (unsigned i = 0; i < BR_AWDL_PEERS + 3; i++) { uint8_t s[6] = { 2, 0, 0, 0, 1, (uint8_t)i }; n = awdl_action(f, s, 0, -50); br_awdl_count_add(&c, f, n); }
  CHECK(c.peer_count == BR_AWDL_PEERS && c.peers_dropped == 5);
  memset(&c, 0, sizeof c);
  br_awdl_count_report(&c, 5, text, sizeof text);
  CHECK(strstr(text, "AWDL: no frame at all: Wonder's receive path gave nothing") != NULL);
  at = radiotap(f, true, 0, -50); memset(f + at, 0, 24); f[at] = 0x80;
  br_awdl_count_add(&c, f, at + 24);
  br_awdl_count_report(&c, 5, text, sizeof text);
  CHECK(strstr(text, "AWDL: no AWDL frame: no Apple device sends AWDL on this channel now") != NULL && !strstr(text, "without the TSFT"));
}

static void test_wonder_vendor(void)
{
  uint8_t d[64];
  // SET_FREQUENCY: CHANNEL_FREQ u32 5745, CHANNEL_BANDWIDTH u16 0 (20 MHz) with its pad
  CHECK(br_wonder_set_frequency(d, sizeof d, 5745) == 16 && !memcmp(d, "\x08\x00\x01\x00\x71\x16\x00\x00\x06\x00\x02\x00\x00\x00\x00\x00", 16));
  // SET_REGULATORY: REG_COUNTRY_CODE "GB" with its NUL
  CHECK(br_wonder_set_regulatory(d, sizeof d, "GB") == 8 && !memcmp(d, "\x07\x00\x01\x00GB\x00\x00", 8));
  // SET_FIXED_TX_RATE: PREAMBLE 1 (HT), BW 0, GI 2 (0.8 us), NSS 1, MCS 0
  CHECK(br_wonder_set_fixed_tx_rate(d, sizeof d) == 40 &&
        !memcmp(d, "\x08\x00\x01\x00\x01\x00\x00\x00" "\x06\x00\x02\x00\x00\x00\x00\x00" "\x08\x00\x03\x00\x02\x00\x00\x00" "\x05\x00\x04\x00\x01\x00\x00\x00"
                   "\x05\x00\x05\x00\x00\x00\x00\x00", 40));
  // SET_FILTER: TOP_FILTER_TYPE u32 0 (BSSID), TOP_FILTER_PARAMS nested { ENABLED u8 1, ADDR AWDL's BSSID }
  CHECK(br_wonder_set_bssid_filter(d, sizeof d, awdl_bss) == 32 &&
        !memcmp(d, "\x08\x00\x01\x00\x00\x00\x00\x00" "\x18\x00\x02\x80" "\x05\x00\x01\x00\x01\x00\x00\x00" "\x0a\x00\x02\x00\x00\x25\x00\xff\x94\x73\x00\x00", 32));
  CHECK(br_wonder_set_frequency(d, 12, 2437) == 0 && br_wonder_set_bssid_filter(d, 31, awdl_bss) == 0 && br_wonder_set_fixed_tx_rate(d, 39) == 0);
  // GET_IF_MAC_ADDR's reply; after another attribute; cut; absent.
  uint8_t m[6];
  CHECK(br_wonder_mac_reply((const uint8_t *)"\x0a\x00\x01\x00\x02\x11\x22\x33\x44\x55\x00\x00", 12, m) && !memcmp(m, "\x02\x11\x22\x33\x44\x55", 6));
  CHECK(br_wonder_mac_reply((const uint8_t *)"\x08\x00\x07\x00\x01\x02\x03\x04\x0a\x00\x01\x00\x02\x11\x22\x33\x44\x66\x00\x00", 20, m) && m[5] == 0x66);
  CHECK(!br_wonder_mac_reply((const uint8_t *)"\x0a\x00\x01\x00\x02\x11\x22", 7, m) && !br_wonder_mac_reply((const uint8_t *)"\x08\x00\x07\x00\x01\x02\x03\x04", 8, m));
  CHECK(br_wonder_freq_ok(2437) && br_wonder_freq_ok(5745) && br_wonder_freq_ok(5220) && !br_wonder_freq_ok(5180) && !br_wonder_freq_ok(0));
}

// ---- the stand-in nl80211 for br_wonder_run: what it was asked, in order, and what it answers
struct fake_nl {
  br_wifi_radios radios;
  char country[3];
  int country_rc, new_rc, up_rc, del_rc, open_rc, recv_rc, vendor_rc;
  uint32_t vendor_fail;                // the subcommand that answers vendor_rc
  bool up_now;                         // the monitor is up (wondertap0 shows)
  bool monitor;                        // the monitor exists
  br_wifi_link before, after;
  bool linked_after;                   // wlan0 is up again after the session
  unsigned links, lose_for;            // link calls; after the session, wlan0 is down for this many more calls
  unsigned pauses;
  uint8_t frames[8][128];
  size_t frame_len[8];
  unsigned frame_count, frame_next, stop_after;
  volatile int *stop;
  uint64_t now;
  uint8_t freq_data[64];
  size_t freq_length;
  char calls[1024];
};
static void call(struct fake_nl *f, const char *what) { snprintf(f->calls + strlen(f->calls), sizeof f->calls - strlen(f->calls), "%s%s", f->calls[0] ? " " : "", what); }
static int f_radios(void *ctx, br_wifi_radios *out)
{
  struct fake_nl *f = ctx;
  call(f, "radios");
  *out = f->radios;
  if (f->monitor) {
    if (out->iface_count < BR_WIFI_IFACES) out->ifaces[out->iface_count++] = (br_wifi_iface){ "wondermon0", 52, 1, T_MON, 0, { 0 } };
    if (f->up_now && out->iface_count < BR_WIFI_IFACES) out->ifaces[out->iface_count++] = (br_wifi_iface){ "wondertap0", 51, 0, T_MON, 5745, { 0 } };
  }
  return 0;
}
static int f_country(void *ctx, uint32_t wiphy, char cc[3]) { struct fake_nl *f = ctx; char t[24]; snprintf(t, sizeof t, "country(%u)", wiphy); call(f, t); memcpy(cc, f->country, 3); return f->country_rc; }
static int f_new(void *ctx, uint32_t wiphy, const char *name, uint32_t *ifindex)
{
  struct fake_nl *f = ctx;
  char t[48];
  snprintf(t, sizeof t, "new(%u,%s)", wiphy, name);
  call(f, t);
  if (f->new_rc) return f->new_rc;
  f->monitor = true;
  *ifindex = 52;
  return 0;
}
static int f_del(void *ctx, uint32_t ifindex)
{
  struct fake_nl *f = ctx;
  char t[24];
  snprintf(t, sizeof t, "del(%u)", ifindex);
  call(f, t);
  if (f->del_rc) return f->del_rc;
  f->monitor = false;
  return 0;
}
static int f_up(void *ctx, const char *name, bool up)
{
  struct fake_nl *f = ctx;
  call(f, up ? "up" : "down");
  (void)name;
  if (up && f->up_rc) return f->up_rc;
  f->up_now = up;
  return 0;
}
static int f_vendor(void *ctx, uint32_t ifindex, uint32_t subcmd, const uint8_t *data, size_t length, uint8_t *reply, size_t *reply_length)
{
  struct fake_nl *f = ctx;
  char t[24];
  snprintf(t, sizeof t, "vendor(%u)", subcmd);
  call(f, t);
  if (ifindex != 52) return -ENODEV;
  if (subcmd == f->vendor_fail) return f->vendor_rc;
  if (subcmd == 1 && length <= sizeof f->freq_data) { memcpy(f->freq_data, data, length); f->freq_length = length; }
  if (subcmd == 5) {                                     // Wonder answers it only when up
    if (!f->up_now) return -ENODEV;
    memcpy(reply, "\x0a\x00\x01\x00\x02\x99\x88\x77\x66\x55\x00\x00", 12);
    *reply_length = 12;
  }
  return 0;
}
static int f_open(void *ctx, uint32_t ifindex) { struct fake_nl *f = ctx; call(f, "open"); return f->open_rc ? f->open_rc : ifindex == 52 ? 7 : -ENODEV; }
static int f_receive(void *ctx, int handle, uint8_t *buffer, size_t size, unsigned wait_ms)
{
  struct fake_nl *f = ctx;
  if (handle != 7) return -EBADF;
  if (f->recv_rc && f->frame_next == f->frame_count) return f->recv_rc;
  if (f->stop && f->stop_after && f->frame_next == f->stop_after) { *f->stop = 1; return -EINTR; }
  if (f->frame_next < f->frame_count && f->frame_len[f->frame_next] <= size) {
    memcpy(buffer, f->frames[f->frame_next], f->frame_len[f->frame_next]);
    f->now += 1;
    return (int)f->frame_len[f->frame_next++];
  }
  f->now += wait_ms;                                     // nothing came in the wait
  return 0;
}
static void f_close(void *ctx, int handle) { struct fake_nl *f = ctx; (void)handle; call(f, "close"); }
static int f_link(void *ctx, const char *ifname, br_wifi_link *out)
{
  struct fake_nl *f = ctx;
  call(f, "link");
  if (strcmp(ifname, "wlan0")) return -ENODEV;
  const bool first = f->links++ == 0;
  if (first) { *out = f->before; return 0; }
  if (f->lose_for) { f->lose_for--; memset(out, 0, sizeof *out); return 0; }
  *out = f->linked_after ? f->after : (br_wifi_link){ 0 };
  return 0;
}
static uint64_t f_now(void *ctx) { return ((struct fake_nl *)ctx)->now; }
static void f_pause(void *ctx, unsigned ms) { struct fake_nl *f = ctx; f->pauses++; f->now += ms; call(f, "pause"); }
static const br_wonder_ops fake_ops = { f_radios, f_country, f_new, f_del, f_up, f_vendor, f_open, f_receive, f_close, f_link, f_now, f_pause };

static void fake_phone(struct fake_nl *f)
{
  memset(f, 0, sizeof *f);
  phone_radios(&f->radios);
  memcpy(f->country, "GB", 3);
  const br_wifi_link l = { .up = true, .rssi_dbm = -55, .tx_rate_mbps = 433, .freq_mhz = 5180, .bssid = { 0x0c, 1, 2, 3, 4, 5 } };
  f->before = f->after = l;
  f->linked_after = true;
  const uint8_t a[6] = { 0x02, 0xaa, 0, 0, 0, 1 }, b2[6] = { 0x02, 0xbb, 0, 0, 0, 2 };
  f->frame_len[0] = awdl_action(f->frames[0], a, 0, -40);
  f->frame_len[1] = awdl_action(f->frames[1], a, 0, -42);
  f->frame_len[2] = awdl_action(f->frames[2], b2, 3, -61);
  f->frame_len[3] = radiotap(f->frames[3], true, 0, -70);
  memset(f->frames[3] + f->frame_len[3], 0, 24); f->frames[3][f->frame_len[3]] = 0x80; f->frame_len[3] += 24;   // a beacon
  f->frame_count = 4;
  f->now = 1000;
}
static int run(struct fake_nl *f, br_wonder_params *p, br_wonder_result *r)
{
  logs_used = 0;
  logs[0] = 0;
  void (*old)(int, const char *) = br_log_sink;
  br_log_sink = log_line;
  const int rc = br_wonder_run(&fake_ops, f, p, r);
  br_log_sink = old;
  return rc;
}

static void test_wonder_session(void)
{
  static struct fake_nl f;
  static br_wonder_result r;
  br_wonder_params p = { .station = "wlan0", .name = BR_WONDER_IFNAME, .freq_mhz = 5745, .seconds = 30 };
  // ---- a session that works: every step in Samsung's order, the frames counted, everything back
  fake_phone(&f);
  int rc = run(&f, &p, &r);
  CHECK(rc == 0);
  CHECK(!strcmp(f.calls, "link radios country(0) new(1,wondermon0) vendor(4) vendor(1) vendor(3) vendor(2) up vendor(5) radios open close down del(52) link"));
  if (strcmp(f.calls, "link radios country(0) new(1,wondermon0) vendor(4) vendor(1) vendor(3) vendor(2) up vendor(5) radios open close down del(52) link")) printf("  calls: %s\n", f.calls);
  uint8_t want[64];
  CHECK(f.freq_length == br_wonder_set_frequency(want, sizeof want, 5745) && !memcmp(f.freq_data, want, f.freq_length));
  CHECK(r.have_mac && !memcmp(r.mac, "\x02\x99\x88\x77\x66\x55", 6) && r.opened && r.link_before && r.link_after && !f.monitor && !f.up_now);
  CHECK(r.count.frames == 4 && r.count.awdl_action == 3 && r.count.awdl_psf == 2 && r.count.awdl_mif == 1 && r.count.peer_count == 2);
  CHECK(f.now >= 1000 + 30000 && f.now < 1000 + 30000 + 600);                     // 30 s of the stand-in's clock
  CHECK(logged("wonder: wlan0 before: associated to 0c:01:02:03:04:05 on 5180 MHz, -55 dBm, 433 Mb/s\n"));
  CHECK(logged("wonder: radio: wiphy 1 (wonder)\n") && logged("wonder: country GB (the driver's own)\n"));
  CHECK(logged("wonder: wlan0 is on 5180 MHz, Wonder on 5745 MHz: the radio shares its time between them (MCC), on one band\n"));
  CHECK(logged("wonder: wondermon0: a monitor interface on wiphy 1 (NL80211_CMD_NEW_INTERFACE): ok\n") && logged("wonder: wondermon0 is index 52\n"));
  CHECK(logged("wonder: country (vendor 00:1a:11 subcommand 4 SET_REGULATORY): ok\n") && logged("wonder: 5745 MHz, 20 MHz wide (vendor 00:1a:11 subcommand 1 SET_FREQUENCY): ok\n"));
  CHECK(logged("subcommand 3 SET_FIXED_TX_RATE): ok\n") && logged("wonder: BSSID filter 00:25:00:ff:94:73 (AWDL's) (vendor 00:1a:11 subcommand 2 SET_FILTER): ok\n"));
  CHECK(logged("wonder: wondermon0 up (Wonder's start: the provider's init opens the PASSTHRU vdev): ok\n"));
  CHECK(logged("wonder: the PASSTHRU vdev's address 02:99:88:77:66:55 (GET_IF_MAC_ADDR)\n"));
  CHECK(logged("wonder: up: wondermon0 (index 52) monitor, wiphy 1, 0 MHz\n") && logged("wonder: up: wondertap0 (index 51) monitor, wiphy 0, 5745 MHz\n"));
  CHECK(logged("wonder: counting wondermon0's frames for 30 s\n") && logged("wonder: frames: 4 in 30 s: 4 management (3 action)"));
  CHECK(logged("wonder: AWDL: 3 action frames (2 PSF, 1 MIF, 0 other subtypes), 0 data frames to AWDL's BSSID, from 2 peer(s)\n"));
  CHECK(logged("wonder: AWDL peer: 02:aa:00:00:00:01: 2 action, 0 data, best -40 dBm\n"));
  CHECK(logged("wonder: AF_PACKET on wondermon0 closed\n") && logged("wonder: wondermon0 down (Wonder's stop: the provider's deinit closes the PASSTHRU vdev): ok\n"));
  CHECK(logged("wonder: wondermon0 removed (NL80211_CMD_DEL_INTERFACE): ok\n") && logged("wonder: wlan0: the same link as before\n") && !logged("\nE "));
  if (getenv("ONEIOS_TEST_SHOW")) printf("---- wifictl wonder --seconds 30 --freq 5745 on the stand-in nl80211\n%s", logs);
  // On 2437 MHz beside a 5 GHz station: MCC across the bands; a roam during it is said.
  fake_phone(&f);
  p.freq_mhz = 2437;
  f.after.bssid[5] = 9;
  CHECK(run(&f, &p, &r) == 0 && logged("on 2437 MHz: the radio shares its time between them (MCC)\n") && logged("associated to another access point than before (a roam)"));
  p.freq_mhz = 5745;

  // ---- the refusals: nothing made, nothing changed
  fake_phone(&f);
  p.freq_mhz = 5180;
  CHECK(run(&f, &p, &r) == -EINVAL && !f.calls[0] && logged("E wonder: refused: 5180 MHz is no AWDL channel"));
  p.freq_mhz = 5745;
  fake_phone(&f);
  f.radios.count = 1;                                    // no Wonder radio
  CHECK(run(&f, &p, &r) == -ENODEV && !strcmp(f.calls, "link radios") && logged("E wonder: refused: no Wonder radio"));
  fake_phone(&f);
  f.radios.wiphys[1].command_count = 4;                  // no GET_IF_MAC_ADDR
  CHECK(run(&f, &p, &r) == -EOPNOTSUPP && !strcmp(f.calls, "link radios"));
  fake_phone(&f);
  iface_msg(&f.radios, 51, "wondertap0", 0, T_MON, 2437);   // Quick Share holds Wonder
  CHECK(run(&f, &p, &r) == -EBUSY && !strcmp(f.calls, "link radios") && logged("E wonder: refused: Wonder is in use: wondertap0 (monitor, wiphy 0) is up"));
  fake_phone(&f);
  iface_msg(&f.radios, 60, "mosey0", 1, T_MON, 0);          // another's interface on Wonder's radio
  CHECK(run(&f, &p, &r) == -EBUSY && logged("Wonder is in use: mosey0 (monitor, wiphy 1)"));
  fake_phone(&f);
  f.radios.iface_count = 0;                              // no wlan0
  CHECK(run(&f, &p, &r) == -ENODEV && logged("E wonder: refused: no wlan0"));
  fake_phone(&f);
  memcpy(f.country, "00", 3);                            // the world domain: not the phone's
  CHECK(run(&f, &p, &r) == -EINVAL && !strcmp(f.calls, "link radios country(0)") && logged("E wonder: refused: the driver's country is not known (\"00)"));
  fake_phone(&f);
  f.country_rc = -EIO;
  CHECK(run(&f, &p, &r) == -EINVAL && logged("not known (Input/output error)"));
  fake_phone(&f);
  memcpy(p.country, "DE", 3);                            // --country against a known, other one
  CHECK(run(&f, &p, &r) == -EINVAL && logged("E wonder: refused: the driver's country is GB, --country DE would change it"));
  fake_phone(&f);
  memcpy(f.country, "00", 3);                            // --country where the driver's is not known: used
  CHECK(run(&f, &p, &r) == 0 && logged("wonder: country DE (the driver's own) (as --country)\n"));
  p.country[0] = 0;

  // ---- a leftover of a killed session is removed first, then the session runs
  fake_phone(&f);
  f.monitor = true;
  rc = run(&f, &p, &r);
  CHECK(rc == 0 && !strncmp(f.calls, "link radios down del(52) radios country(0) new(1,wondermon0) ", strlen("link radios down del(52) radios country(0) new(1,wondermon0) ")) && logged("wonder: wondermon0 (index 52) is left from a session that did not end: removed first\n"));
  fake_phone(&f);
  f.monitor = true;
  f.del_rc = -EPERM;
  CHECK(run(&f, &p, &r) == -EPERM && !strcmp(f.calls, "link radios down del(52)"));

  // ---- failed steps: everything made is closed in reverse, and wlan0 checked
  fake_phone(&f);
  f.new_rc = -EOPNOTSUPP;
  CHECK(run(&f, &p, &r) == -EOPNOTSUPP && !strcmp(f.calls, "link radios country(0) new(1,wondermon0) link") && r.link_after);
  fake_phone(&f);
  f.vendor_fail = 1; f.vendor_rc = -EINVAL;              // SET_FREQUENCY refused: removed, never up
  CHECK(run(&f, &p, &r) == -EINVAL && !strcmp(f.calls, "link radios country(0) new(1,wondermon0) vendor(4) vendor(1) del(52) link") && !f.monitor);
  CHECK(logged("E wonder: 5745 MHz, 20 MHz wide (vendor 00:1a:11 subcommand 1 SET_FREQUENCY): Invalid argument\n"));
  fake_phone(&f);
  f.up_rc = -EBUSY;                                      // the provider's init: a connection in progress
  CHECK(run(&f, &p, &r) == -EBUSY && !strcmp(f.calls, "link radios country(0) new(1,wondermon0) vendor(4) vendor(1) vendor(3) vendor(2) up del(52) link"));
  CHECK(logged("E wonder: wondermon0 up (Wonder's start: the provider's init opens the PASSTHRU vdev): Device or resource busy (a connection or a roam of wlan0 in progress, or Wonder already up)\n"));
  fake_phone(&f);
  f.open_rc = -EPERM;
  CHECK(run(&f, &p, &r) == -EPERM && !strcmp(f.calls, "link radios country(0) new(1,wondermon0) vendor(4) vendor(1) vendor(3) vendor(2) up vendor(5) radios open down del(52) link") && !r.opened);
  fake_phone(&f);
  f.recv_rc = -ENETDOWN;                                 // the interface went away under the count
  CHECK(run(&f, &p, &r) == -ENETDOWN && r.count.frames == 4 && strstr(f.calls, "open close down del(52) link") && logged("E wonder: receive on wondermon0: Network is down\n"));
  fake_phone(&f);
  f.del_rc = -EBUSY;                                     // removal refused: the session's error, said
  CHECK(run(&f, &p, &r) == -EBUSY && logged("E wonder: wondermon0 removed (NL80211_CMD_DEL_INTERFACE): Device or resource busy (the next `wifictl wonder` removes it)\n"));
  // A Wonder without SET_FILTER and SET_FIXED_TX_RATE: those two are left out.
  fake_phone(&f);
  f.radios.wiphys[1].commands[1].subcmd = 7; f.radios.wiphys[1].commands[2].subcmd = 8;
  CHECK(run(&f, &p, &r) == 0 && strstr(f.calls, "vendor(4) vendor(1) up vendor(5)") && logged("wonder: radio: wiphy 1 (wonder), no SET_FILTER, no SET_FIXED_TX_RATE\n"));

  // ---- a signal ends the count; the teardown still runs
  volatile int stop = 0;
  fake_phone(&f);
  p.stop = &stop; f.stop = &stop; f.stop_after = 2;
  CHECK(run(&f, &p, &r) == 0 && r.count.frames == 2 && stop && strstr(f.calls, "open close down del(52) link") && logged("wonder: stopped by a signal after 2 ms\n"));
  p.stop = NULL;

  // ---- wlan0 afterwards: back within the 10 s, or lost
  fake_phone(&f);
  f.lose_for = 3;
  CHECK(run(&f, &p, &r) == 0 && f.pauses == 3 && r.link_after);
  fake_phone(&f);
  f.linked_after = false;
  CHECK(run(&f, &p, &r) == -ENOLINK && f.pauses == 10 && !r.link_after && logged("E wonder: wlan0 lost its link (associated before, not 10 s after the end)"));
  fake_phone(&f);
  f.before.up = false; f.linked_after = false;           // not associated before: nothing to wait for
  CHECK(run(&f, &p, &r) == 0 && f.pauses == 0 && logged("wonder: wlan0 before: not associated\n") && !logged("MCC"));
}

int main(void)
{
  test_frames();
  test_dhcp();
  test_bands();
  test_p2p();
  test_radios();
  test_awdl_frames();
  test_wonder_vendor();
  test_wonder_session();
  test_groups_segments();
  test_station();
  test_sim();
  test_parcels();
  test_security();
  test_callbacks();
  printf("wifi: %d checks, %d failed\n", checks, failures);
  return failures ? 1 : 0;
}
