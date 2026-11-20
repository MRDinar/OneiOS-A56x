// SPDX-License-Identifier: MIT
// LE scan for a controller that refuses the extended scan commands, as iOSPlus10's laurel 9012 does in its
// kernel (patches/laurel/xnu-12377/post/9012-laurel-bt-legacy-le-scan.patch): iOS 26/27's bluetoothd starts its
// LE scan with LE Set Extended Scan Parameters (0x2041) and Enable (0x2042) whatever the controller's supported
// commands say; on the Mi A3's WCN3950 the answer was status 1, OI status 701, "Could not start LE scan" and a new
// bluetoothd every 10 s.
//
// Unlike laurel this is adaptive: nothing changes until the controller refuses 0x2041 (Unknown HCI Command 0x01 or
// Unsupported Feature 0x11). Then that answer is kept from the guest, the same parameters go out as the legacy
// LE Set Scan Parameters (0x200b) and its answer goes back under 0x2041; from then on 0x2042 goes out as LE Set Scan
// Enable (0x200c), LE Advertising Report (bit 1 of the LE event mask) is turned on, and each legacy report goes up
// as one LE Extended Advertising Report (subevent 0x0d). The S24 Ultra's WCN785x (Bluetooth 5.3) is expected to
// accept the extended commands, so this should stay idle there [I]. Lost in legacy mode: coded PHY, scan duration.
#include "le_scan.h"
#include <string.h>

static uint16_t le16(const uint8_t *p) { return (uint16_t)(p[0] | p[1] << 8); }

static void command(br_le_scan *s, const uint8_t *c, size_t n)
{
  if (s->out_n < BR_LE_SCAN_OUT) {
    memcpy(s->out[s->out_n].data, c, n);
    s->out[s->out_n].length = n;
    s->out_n++;
  }
}

// 0x2041 -> 0x200b: own, filter, phys, then per PHY type, interval, window; the first PHY's values go on.
static bool legacy_params(const uint8_t *c, size_t n, uint8_t out[10])
{
  if (n < 3 + 8 || c[2] < 8 || !c[5]) return false;
  out[0] = 0x0b; out[1] = 0x20; out[2] = 7;
  out[3] = c[6];                                      // scan type
  out[4] = c[7]; out[5] = c[8];                       // interval
  out[6] = c[9]; out[7] = c[10];                      // window
  out[8] = c[3];                                      // own address type
  out[9] = c[4];                                      // filter policy
  return true;
}

// 0x2042 -> 0x200c: enable, filter duplicates (2, "reset each period", has no legacy value: 1).
static bool legacy_enable(const uint8_t *c, size_t n, uint8_t out[5])
{
  if (n < 3 + 6 || c[2] < 6) return false;
  out[0] = 0x0c; out[1] = 0x20; out[2] = 2;
  out[3] = c[3];
  out[4] = c[4] != 0;
  return true;
}

bool br_le_scan_command(br_le_scan *s, const uint8_t *c, size_t n)
{
  s->out_n = 0;
  if (n < 3) return false;
  uint16_t op = le16(c);
  if (op == 0x2001 && n == 11) {                      // LE Set Event Mask: remembered; in legacy mode with reports on
    memcpy(s->mask, c + 3, 8);
    s->have_mask = true;
    if (s->legacy && !(c[3] & 0x02)) {
      uint8_t fixed[11];
      memcpy(fixed, c, 11);
      fixed[3] |= 0x02;
      command(s, fixed, 11);
      return true;
    }
    return false;
  }
  if (op == 0x2041) {
    uint8_t legacy[10];
    s->ext_used = true;
    if (n <= sizeof s->last_params) { memcpy(s->last_params, c, n); s->last_params_n = n; }
    if (!s->legacy || !legacy_params(c, n, legacy)) return false;
    s->params_out++;
    command(s, legacy, sizeof legacy);
    return true;
  }
  if (op == 0x2042 && s->legacy) {
    uint8_t legacy[5];
    if (!legacy_enable(c, n, legacy)) return false;
    s->enable_out++;
    command(s, legacy, sizeof legacy);
    return true;
  }
  return false;
}

// Command Complete (0x0e: ncmd, opcode, status) or Command Status (0x0f: status, ncmd, opcode) for `op`: its status.
static int answer_status(const uint8_t *e, size_t n, uint16_t op)
{
  if (n >= 6 && e[0] == 0x0e && e[1] + 2u == n && le16(e + 3) == op) return e[5];
  if (n >= 6 && e[0] == 0x0f && e[1] + 2u == n && le16(e + 4) == op) return e[2];
  return -1;
}

static void rewrite_opcode(uint8_t *e, uint16_t op)
{
  uint8_t *at = e[0] == 0x0e ? e + 3 : e + 4;
  at[0] = (uint8_t)op;
  at[1] = (uint8_t)(op >> 8);
}

unsigned br_le_scan_event(br_le_scan *s, const uint8_t *e, size_t n)
{
  s->out_n = 0;
  s->up_n = 0;
  if (n < 2 || n > BR_LE_SCAN_EVENT) return BR_LE_SCAN_PASS;
  int status = answer_status(e, n, 0x2041);
  if (status >= 0 && !s->legacy && (status == 0x01 || status == 0x11) && s->last_params_n) {
    uint8_t legacy[10];
    if (!legacy_params(s->last_params, s->last_params_n, legacy)) return BR_LE_SCAN_PASS;
    s->legacy = true;
    s->switched++;
    if (s->have_mask && !(s->mask[0] & 0x02)) {        // LE Advertising Report on; its answer is not the guest's
      uint8_t mask[11] = { 0x01, 0x20, 8 };
      memcpy(mask + 3, s->mask, 8);
      mask[3] |= 0x02;
      command(s, mask, sizeof mask);
      s->mask_out++;
    }
    command(s, legacy, sizeof legacy);
    s->params_out++;
    return BR_LE_SCAN_DROP;
  }
  if (s->mask_out && answer_status(e, n, 0x2001) >= 0) { s->mask_out--; return BR_LE_SCAN_DROP; }
  if (s->params_out && answer_status(e, n, 0x200b) >= 0) {
    s->params_out--;
    memcpy(s->up[0], e, n); s->up_length[0] = n; s->up_n = 1;
    rewrite_opcode(s->up[0], 0x2041);
    return BR_LE_SCAN_REPLACE;
  }
  if (s->enable_out && answer_status(e, n, 0x200c) >= 0) {
    s->enable_out--;
    memcpy(s->up[0], e, n); s->up_length[0] = n; s->up_n = 1;
    rewrite_opcode(s->up[0], 0x2042);
    return BR_LE_SCAN_REPLACE;
  }
  // LE Advertising Report (0x3e, subevent 0x02): each report as one LE Extended Advertising Report.
  if (s->legacy && s->ext_used && n >= 3 && e[0] == 0x3e && e[1] + 2u == n && e[2] == 0x02) {
    static const uint8_t props[5] = { 0x13, 0x15, 0x12, 0x10, 0x1b };   // Core 7.7.65.13, the legacy PDU types
    size_t p = 4;
    unsigned count = n >= 4 ? e[3] : 0;
    for (unsigned i = 0; i < count && s->up_n < BR_LE_SCAN_UP; i++) {
      if (p + 9 > n) break;
      size_t len = e[p + 8];
      if (p + 9 + len + 1 > n || e[p] > 4 || len > 31) break;
      uint8_t *o = s->up[s->up_n];
      size_t q = 0;
      o[q++] = 0x3e;
      o[q++] = (uint8_t)(2 + 24 + len);
      o[q++] = 0x0d;                                   // LE Extended Advertising Report
      o[q++] = 1;
      o[q++] = props[e[p]]; o[q++] = 0;                // event type
      o[q++] = e[p + 1];                               // address type
      memcpy(o + q, e + p + 2, 6); q += 6;
      o[q++] = 1;                                      // primary PHY LE 1M
      o[q++] = 0;                                      // no secondary PHY
      o[q++] = 0xff;                                   // no SID
      o[q++] = 0x7f;                                   // TX power not known
      o[q++] = e[p + 9 + len];                         // RSSI
      o[q++] = 0; o[q++] = 0;                          // no periodic advertising
      o[q++] = 0; memset(o + q, 0, 6); q += 6;         // no direct address
      o[q++] = (uint8_t)len;
      memcpy(o + q, e + p + 9, len); q += len;
      s->up_length[s->up_n++] = q;
      p += 9 + len + 1;
    }
    return s->up_n ? BR_LE_SCAN_REPLACE : BR_LE_SCAN_PASS;
  }
  return BR_LE_SCAN_PASS;
}
