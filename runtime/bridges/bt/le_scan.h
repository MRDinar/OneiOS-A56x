// SPDX-License-Identifier: MIT
// Extended LE scan commands as legacy ones for a controller that refuses them (le_scan.c). Packets are HCI
// packets without the H4 indicator. The caller holds a lock around each call.
#ifndef ONEIOS_LE_SCAN_H
#define ONEIOS_LE_SCAN_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#define BR_LE_SCAN_OUT 2u          // commands to the controller per call
#define BR_LE_SCAN_UP 8u           // events to the guest per call
#define BR_LE_SCAN_EVENT 258u      // an event's largest size (code, length, 255 bytes)

typedef struct {
  bool legacy;                     // the controller refused 0x2041: legacy commands from now on
  bool ext_used, have_mask;
  uint8_t mask[8];                 // the guest's LE event mask
  uint8_t last_params[64];         // the last 0x2041, resent as 0x200b when it is refused
  size_t last_params_n;
  unsigned params_out, enable_out, mask_out;    // legacy commands sent whose answers go up renamed (or not at all)
  unsigned switched;
  // outputs of the last call
  struct { uint8_t data[64]; size_t length; } out[BR_LE_SCAN_OUT];
  unsigned out_n;
  uint8_t up[BR_LE_SCAN_UP][BR_LE_SCAN_EVENT];
  size_t up_length[BR_LE_SCAN_UP];
  unsigned up_n;
} br_le_scan;

// A command from the guest. true: send out[0..out_n) instead of it; false: send it as it is.
bool br_le_scan_command(br_le_scan *s, const uint8_t *c, size_t n);
// An event from the controller. Send out[] to the controller in every case; then pass the event, drop it, or
// give the guest up[0..up_n) instead.
enum { BR_LE_SCAN_PASS, BR_LE_SCAN_DROP, BR_LE_SCAN_REPLACE };
unsigned br_le_scan_event(br_le_scan *s, const uint8_t *e, size_t n);

#endif
