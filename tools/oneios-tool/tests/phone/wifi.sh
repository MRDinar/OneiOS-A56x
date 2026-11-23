# Sourced by tests/phone/run.sh (with lib.sh's helpers, HERE, W and TOOL set): `phone wifi p2p` and `phone wifi
# wonder` against a stand-in phone of their own ($W/wifiphone) whose staged runtime has a stand-in wifictl
# (runtime/bin/wifictl: a shell script that notes its arguments in .fake/wifictl.log and prints what the phone's
# wifictl prints: the p2p report of a phone with Wonder 1.5, a Wonder session's lines; with .fake/wifictl-fail it
# fails as wifictl does). The real wifictl's steps are host-tested in runtime/bridges/wifi (cargo test -p onebridges wifi).
# Checked: root asked for first; no wifictl staged; the arguments refused before anything reaches the phone (a
# station name with shell characters, a frequency that is no AWDL channel, --seconds out of range, a country that is
# not two letters); p2p's report passed through, its summary, --save, --station; a p2p that fails; wonder's lines
# streamed, its command line on the phone, --country upper-cased, --save; a wonder that wifictl refuses.
wifi_checks() {
  local P=$W/wifiphone before=${FAKE_PHONE:-} out st
  local S=$P/data/local/tmp/oneios L=$P/.fake/wifictl.log
  export FAKE_PHONE=$P
  make_phone "$P"
  wphone() { "$TOOL" phone wifi "$@"; }
  ran() { cat "$L" 2>/dev/null; }

  # ---- root first, and a runtime without wifictl
  check "wifi p2p: no root says so" 1 "no root on the phone FAKE0001" env FAKE_NO_ROOT=1 "$TOOL" phone wifi p2p
  check "wifi p2p: no wifictl staged" 1 "the staged runtime has no /data/local/tmp/oneios/runtime/bin/wifictl: stage a runtime that has it" wphone p2p
  cat > "$S/runtime/bin/wifictl" <<'SH'
#!/bin/sh
# the stand-in wifictl (tests/phone/wifi.sh)
echo "$*" >> "$FAKE_PHONE/.fake/wifictl.log"
if [ -e "$FAKE_PHONE/.fake/wifictl-fail" ]; then
  case "$1" in
  p2p) echo "p2p: nl80211: No such device" ;;
  wonder) echo "  wonder: wlan0 before: associated to 0c:01:02:03:04:05 on 5180 MHz, -55 dBm, 433 Mb/s"
          echo "  error: wonder: refused: Wonder is in use: wondertap0 (monitor, wiphy 0) is up (Quick Share's \"Share with Apple devices\"? try again after it)"
          echo "wonder: Device or resource busy: 0 frames, 0 AWDL action frames from 0 peer(s); wlan0 associated as before" ;;
  esac
  echo failed; exit 1
fi
case "$1" in
p2p) cat <<'EOF'
radios: 2
radio: phy0 (wiphy 0)
interface types: station, access point, monitor, p2p-client, p2p-go
  (wlan0's radio)
radio: wonder (wiphy 1)
interface types: ad-hoc, station, monitor, nan
vendor commands: 5, vendor events: 0
  001a11 Google: 1 2 3 4 5
  (Google's Wonder radio)
interface: wlan0 (index 44) station, wiphy 0, 0c:aa:bb:cc:dd:ee, 5180 MHz
p2p: Wonder: wiphy 1 (wonder), interface types ad-hoc, station, monitor, nan
p2p: Wonder: vendor subcommands under 00:1a:11: 1 SET_FREQUENCY 2 SET_FILTER 3 SET_FIXED_TX_RATE 4 SET_REGULATORY 5 GET_IF_MAC_ADDR
p2p: Wonder: version 1.4 or 1.5 (subcommands 1 to 5 only: one channel, no hopping; Samsung's S928BXXU5DZDP source)
p2p: Wonder: idle (no interface on it, no wondertap vdev)
p2p: monitor mode: yes (beside the station: no; active monitor, with ACKs: no; frame injection is not reported by nl80211)
p2p: for AirDrop iOS needs AWDL: Wonder's raw 802.11 path is offered (wiphy 1): an AWDL engine on the host can run on a monitor interface of it beside wlan0 (`wifictl wonder` opens one and counts the AWDL frames)
wonder: /sys/module/wonder: present
wonder: /sys/module/wonder/parameters/physical_name: present: wondertap0
wonder: /system_ext/bin/mosey_server: present
netdev: wlan0 (a Wi-Fi radio's)
ok
EOF
;;
wonder) for l in "wonder: wlan0 before: associated to 0c:01:02:03:04:05 on 5180 MHz, -55 dBm, 433 Mb/s" "wonder: radio: wiphy 1 (wonder)" \
          "wonder: country GB (the driver's own)" "wonder: wondermon0: a monitor interface on wiphy 1 (NL80211_CMD_NEW_INTERFACE): ok" \
          "wonder: wondermon0 up (Wonder's start: the provider's init opens the PASSTHRU vdev): ok" "wonder: counting wondermon0's frames for 30 s" \
          "wonder: AWDL: 3 action frames (2 PSF, 1 MIF, 0 other subtypes), 0 data frames to AWDL's BSSID, from 2 peer(s)" \
          "wonder: wondermon0 removed (NL80211_CMD_DEL_INTERFACE): ok" "wonder: wlan0: the same link as before"; do echo "  $l"; done
        echo "wonder: done: 4 frames, 3 AWDL action frames from 2 peer(s); wlan0 associated as before"
        echo ok ;;
*) echo "unknown command"; exit 2 ;;
esac
SH
  chmod 755 "$S/runtime/bin/wifictl"

  # ---- arguments that never reach the phone
  for c in "p2p --station wlan0;id" "p2p --station=-x" "wonder --freq 5180" "wonder --seconds 0" "wonder --seconds 601" "wonder --country G1" "wonder --country GBR"; do
    check "wifi $c: refused here" 1 "is no interface name|is no AWDL channel|is not 1 to 600|is not two letters" wphone $c
  done
  [ -z "$(ran)" ] && ok "wifi: the refused arguments ran nothing on the phone" || bad "wifi: a refused argument ran" "$(ran | tr '\n' '|')"

  # ---- p2p: the report as the phone printed it, then the summary; --save; --station
  out=$(wphone p2p --save "$W/p2p.txt" 2>&1); st=$?
  [ $st = 0 ] && [ "$(ran)" = "p2p wlan0" ] && ok "wifi p2p: status 0, wifictl p2p wlan0 ran" || bad "wifi p2p" "status $st, ran: $(ran | tr '\n' '|'): $(printf '%s' "$out" | tail -n 3 | tr '\n' '|')"
  printf '%s\n' "$out" | grep -q "^p2p: Wonder: vendor subcommands under 00:1a:11: 1 SET_FREQUENCY 2 SET_FILTER" && printf '%s\n' "$out" | grep -q "^netdev: wlan0" \
    && ok "wifi p2p: the phone's report is printed as it is" || bad "wifi p2p: the report" "$(printf '%s' "$out" | head -n 3 | tr '\n' '|')"
  [ "$(printf '%s\n' "$out" | sed -n '/^---- summary$/,$p' | grep -c .)" = 10 ] && printf '%s\n' "$out" | sed -n '/^---- summary$/,$p' | grep -q "^p2p: for AirDrop iOS needs AWDL: Wonder's raw" \
    && ! printf '%s\n' "$out" | sed -n '/^---- summary$/,$p' | grep -q "^interface: wlan0" \
    && ok "wifi p2p: the summary: radios, the Wonder block, the verdict, Wonder's files (9 lines), not wlan0" || bad "wifi p2p: the summary" "$(printf '%s' "$out" | sed -n '/^---- summary$/,$p' | tr '\n' '|')"
  grep -q "^p2p: Wonder: idle" "$W/p2p.txt" && [ "$(tail -n 1 "$W/p2p.txt")" = ok ] && ok "wifi p2p --save: the phone's report in the file" || bad "wifi p2p --save" "$(tail -n 2 "$W/p2p.txt" 2>&1 | tr '\n' '|')"
  rm -f "$L"
  check "wifi p2p --station wlan1" 0 "^p2p: Wonder:" wphone p2p --station wlan1
  [ "$(ran)" = "p2p wlan1" ] && ok "wifi p2p --station: wifictl p2p wlan1 ran" || bad "wifi p2p --station" "$(ran)"
  touch "$P/.fake/wifictl-fail"
  check "wifi p2p: a failure on the phone is the command's" 1 "wifictl p2p failed on the phone .*No such device" wphone p2p
  rm -f "$P/.fake/wifictl-fail" "$L"

  # ---- wonder: the lines as they come, the command line on the phone, --country, --save
  out=$(wphone wonder --seconds 30 --freq 5745 --country gb --save "$W/wonder.txt" 2>&1); st=$?
  [ $st = 0 ] && [ "$(ran)" = "wonder --seconds 30 --freq 5745 --country GB" ] && ok "wifi wonder: status 0, wifictl wonder --seconds 30 --freq 5745 --country GB ran" \
    || bad "wifi wonder" "status $st, ran: $(ran): $(printf '%s' "$out" | tail -n 3 | tr '\n' '|')"
  printf '%s\n' "$out" | grep -q "^wonder: 30 s on 5745 MHz, on the phone: /data/local/tmp/oneios/runtime/bin/wifictl wonder --seconds 30 --freq 5745 --country GB$" \
    && printf '%s\n' "$out" | grep -q "^  wonder: AWDL: 3 action frames (2 PSF, 1 MIF" && printf '%s\n' "$out" | grep -q "^  wonder: wlan0: the same link as before$" \
    && ! printf '%s\n' "$out" | grep -q "a session is open" \
    && ok "wifi wonder: its command, then wifictl's lines (the AWDL count, wlan0 as before); no session note without a session" || bad "wifi wonder: the lines" "$(printf '%s' "$out" | tr '\n' '|')"
  grep -q "^wonder: done: 4 frames" "$W/wonder.txt" && [ "$(tail -n 1 "$W/wonder.txt")" = ok ] && ok "wifi wonder --save: the phone's lines in the file" || bad "wifi wonder --save" "$(tail -n 2 "$W/wonder.txt" 2>&1 | tr '\n' '|')"
  rm -f "$L"
  check "wifi wonder: the defaults (30 s on 2437 MHz)" 0 "^wonder: 30 s on 2437 MHz" wphone wonder
  [ "$(ran)" = "wonder --seconds 30 --freq 2437" ] && ok "wifi wonder: no --country: none sent" || bad "wifi wonder: the defaults" "$(ran)"
  touch "$P/.fake/wifictl-fail"
  check "wifi wonder: refused by wifictl (Wonder in use)" 1 "refused: Wonder is in use.*|wifictl wonder ended \"failed\"" wphone wonder --seconds 5
  out=$(wphone wonder --seconds 5 2>&1)
  printf '%s\n' "$out" | grep -q "^  error: wonder: refused: Wonder is in use: wondertap0" && printf '%s\n' "$out" | grep -q 'wifictl wonder ended "failed": the lines above say which step' \
    && ok "wifi wonder: wifictl's refusal is printed, and the command fails with it" || bad "wifi wonder: the refusal" "$(printf '%s' "$out" | tr '\n' '|')"
  rm -f "$P/.fake/wifictl-fail"
  export FAKE_PHONE=$before
}
