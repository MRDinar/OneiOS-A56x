# Sourced by tests/phone/run.sh (with lib.sh's helpers, HERE, ROOT and W set): the bridges' two hardware backends
# (hw-backend=samsung|treble, docs/hardware.md 7e) on stand-in phones of their own. The bridges' phone tools
# (brightctl, ...; their host builds, made here with the build host's cc) run against each stand-in's sysfs, and for
# the HAL-based treble forms against a fake servicemanager with fake HALs on a binderfs (fake-hal, runtime/bridges/
# tests/fake_hal.c, in a user namespace; skipped with a line where the kernel has no binderfs):
#   s24            the S24 Ultra as the bridges see it: sys/class/backlight/panel0-backlight (max 51000, Samsung's
#                  doubled range), sys/class/sec/tsp (the touch IC's power node, name sec_touchscreen),
#                  sys/class/power_supply/{battery,usb}; .fake/hal/health (the same battery for the health HAL)
#   other          another phone: sys/class/backlight/lcd-backlight (max 2047), no sec class, no power_supply nodes;
#                  .fake/hal/health (its battery only through the health HAL)
#   odd            a phone whose backlight has a vendor's own name (vendor-bl): samsung's folder listing finds it, the
#                  treble backend (named paths only) does not
# Checked per backend: which node is used and its range, the line naming the backend and why, what is written and
# restored, what is left alone (the touch IC's node in treble), the one "missing" line and the fallback rule.
# The stand-ins are under a short folder of their own (mktemp -d /tmp/oneios-hw.XXXXXX), removed at the end.
hw_backend_checks() {
  local B=$ROOT/runtime/bridges out st
  if ! command -v cc > /dev/null 2>&1 || ! make -s -C "$B/brightness" build/host/brightctl > "$W/hw-build.txt" 2>&1; then
    echo "skipped hw-backend (the bridges' host tools do not build here: $(tail -n 1 "$W/hw-build.txt" 2>/dev/null))"; return
  fi
  local BRIGHTCTL=$B/brightness/build/host/brightctl
  # the stand-ins live under a short folder of their own: the bridges keep a sysfs path in 200 bytes, as the phone's are
  local T; T=$(mktemp -d /tmp/oneios-hw.XXXXXX) || { bad "hw-backend: no scratch folder"; return; }
  local S24=$T/s24 OTHER=$T/other ODD=$T/odd
  mkdir -p "$S24/sys/class/backlight/panel0-backlight" "$S24/sys/class/sec/tsp/input" "$OTHER/sys/class/backlight/lcd-backlight" \
    "$ODD/sys/class/backlight/vendor-bl"
  echo 51000 > "$S24/sys/class/backlight/panel0-backlight/max_brightness"; echo 9000 > "$S24/sys/class/backlight/panel0-backlight/brightness"
  echo sec_touchscreen > "$S24/sys/class/sec/tsp/input/name"; echo 0,0 > "$S24/sys/class/sec/tsp/enabled"
  echo 2047 > "$OTHER/sys/class/backlight/lcd-backlight/max_brightness"; echo 700 > "$OTHER/sys/class/backlight/lcd-backlight/brightness"
  echo 510 > "$ODD/sys/class/backlight/vendor-bl/max_brightness"; echo 100 > "$ODD/sys/class/backlight/vendor-bl/brightness"
  bl() { local p=$1; shift; env ASAN_OPTIONS=detect_leaks=0 ONEIOS_BRIGHT_DIR="$p/sys/class/backlight" ONEIOS_TSP_DIR="$p/sys/class/sec" ONEIOS_INPUT_DIR="$p/dev/input" "$@" "$BRIGHTCTL" "${BL_ARGS[@]}"; }

  # ---- backlight: samsung (the default) on the S24: unchanged, the doubled range halved, the touch IC woken
  BL_ARGS=(info)
  out=$(bl "$S24" 2>&1); st=$?
  [ $st = 0 ] && printf '%s\n' "$out" | grep -q '^bridges: hw: backlight: backend samsung (the default)$' && printf '%s\n' "$out" | grep -q '^backend: samsung$' \
    && printf '%s\n' "$out" | grep -q 'panel: .*/panel0-backlight/brightness  max_brightness 51000, slider range 1..25500, One UI.s value 9000' \
    && [ "$(cat "$S24/sys/class/sec/tsp/enabled")" = 2,1 ] \
    && ok "hw-backend: backlight samsung (the default) on the S24: panel0-backlight, 1..25500 of 51000, the touch IC woken" || bad "hw-backend: backlight samsung on the S24" "status $st: $(printf '%s' "$out" | tr '\n' '|')"
  # ---- treble on the S24: the standard class name, the whole range, the touch IC's node not written
  echo 0,0 > "$S24/sys/class/sec/tsp/enabled"
  out=$(bl "$S24" ONEIOS_HW_BACKEND=treble 2>&1); st=$?
  [ $st = 0 ] && printf '%s\n' "$out" | grep -q '^bridges: hw: backlight: backend treble (hw-backend=treble)$' \
    && printf '%s\n' "$out" | grep -q 'slider range 1..51000, One UI.s value 9000' && printf '%s\n' "$out" | grep -q 'touch: treble: no vendor-neutral power node' \
    && printf '%s\n' "$out" | grep -q 'light sensor: treble: missing: .*off (hw-backend-fallback=0)' && [ "$(cat "$S24/sys/class/sec/tsp/enabled")" = 0,0 ] \
    && ok "hw-backend: backlight treble on the S24: panel0-backlight by name, 1..51000, the touch IC and the light sensor left alone" || bad "hw-backend: backlight treble on the S24" "status $st: $(printf '%s' "$out" | tr '\n' '|')"
  # levels: written and One UI's value back, both backends
  BL_ARGS=(levels --ms 100 100)
  out=$(bl "$S24" ONEIOS_HW_BACKEND_BACKLIGHT=treble 2>&1); st=$?
  [ $st = 0 ] && printf '%s\n' "$out" | grep -q 'backend treble (hw-backend-backlight=treble)' && printf '%s\n' "$out" | grep -q '^slider 100% -> wanted 51000, the node was written 51000$' \
    && [ "$(cat "$S24/sys/class/backlight/panel0-backlight/brightness")" = 9000 ] \
    && ok "hw-backend: backlight treble levels: 51000 written, One UI's 9000 back" || bad "hw-backend: backlight treble levels" "status $st: $(printf '%s' "$out" | tr '\n' '|')"
  out=$(bl "$S24" 2>&1); st=$?
  [ $st = 0 ] && printf '%s\n' "$out" | grep -q '^slider 100% -> wanted 25500, the node was written 25500$' && [ "$(cat "$S24/sys/class/backlight/panel0-backlight/brightness")" = 9000 ] \
    && ok "hw-backend: backlight samsung levels: 25500 written (the manual top), One UI's 9000 back" || bad "hw-backend: backlight samsung levels" "status $st: $(printf '%s' "$out" | tr '\n' '|')"
  # ---- another phone: both find lcd-backlight (samsung by listing the class, treble by its name)
  BL_ARGS=(info)
  for be in samsung treble; do
    out=$(bl "$OTHER" ONEIOS_HW_BACKEND=$be 2>&1); st=$?
    [ $st = 0 ] && printf '%s\n' "$out" | grep -q "^backend: $be$" && printf '%s\n' "$out" | grep -q 'lcd-backlight/brightness  max_brightness 2047, slider range 1..2047, One UI.s value 700' \
      && ok "hw-backend: backlight $be on another phone: lcd-backlight, 1..2047" || bad "hw-backend: backlight $be on another phone" "status $st: $(printf '%s' "$out" | tr '\n' '|')"
  done
  # ---- a vendor's own name: treble says what is missing in one line and stays off; with the fallback, samsung's form
  out=$(bl "$ODD" ONEIOS_HW_BACKEND=treble 2>&1); st=$?
  [ $st = 1 ] && [ "$(printf '%s\n' "$out" | grep -c 'treble: missing:')" = 1 ] && printf '%s\n' "$out" | grep -q 'hw: backlight: treble: missing: no backlight class device .*; the bridge stays off (hw-backend-fallback=0)' \
    && [ "$(cat "$ODD/sys/class/backlight/vendor-bl/brightness")" = 100 ] \
    && ok "hw-backend: backlight treble, no standard name: one missing line, the bridge off, nothing written" || bad "hw-backend: backlight treble, no standard name" "status $st: $(printf '%s' "$out" | tr '\n' '|')"
  out=$(bl "$ODD" ONEIOS_HW_BACKEND=treble ONEIOS_HW_FALLBACK=1 2>&1); st=$?
  [ $st = 0 ] && printf '%s\n' "$out" | grep -q 'falling back to samsung (hw-backend-fallback=1)' && printf '%s\n' "$out" | grep -q '^backend: samsung$' \
    && printf '%s\n' "$out" | grep -q 'vendor-bl/brightness  max_brightness 510, slider range 1..255' \
    && ok "hw-backend: backlight treble with hw-backend-fallback=1: samsung's form finds vendor-bl" || bad "hw-backend: backlight fallback" "status $st: $(printf '%s' "$out" | tr '\n' '|')"
  out=$(bl "$S24" ONEIOS_HW_BACKEND=pixel 2>&1); st=$?
  [ $st = 2 ] && printf '%s\n' "$out" | grep -q 'bad ONEIOS_HW_BACKEND' && ok "hw-backend: a backend that does not exist is refused" || bad "hw-backend: a bad backend" "status $st: $out"

  # ---- the HAL-based treble forms: a fake servicemanager and HALs (fake-hal) on a binderfs in a user namespace
  local HAVE_HAL=1
  if ! grep -qw binder /proc/filesystems 2>/dev/null || ! unshare -Urm true 2>/dev/null; then
    HAVE_HAL=; echo "skipped hw-backend's HAL checks (no binderfs in this kernel, or no user namespaces)"
  elif ! make -s -C "$B" build/host/fake-hal > "$W/hw-build.txt" 2>&1; then
    HAVE_HAL=; bad "hw-backend: fake-hal does not build" "$(tail -n 2 "$W/hw-build.txt" | tr '\n' '|')"
  fi
  mkdir -p "$S24/.fake/hal" "$OTHER/.fake/hal"
  # with_hal PHONE CMD...: CMD with ONEIOS_BINDER set, beside fake-hal serving PHONE/.fake/hal (gone when CMD ends)
  with_hal() {
    local p=$1; shift
    mkdir -p "$T/binderfs"
    unshare -Urm bash -c 'mount -t binder binder "$1" || exit 90; export ONEIOS_BINDER=$1/binder; hal=$2; shift 2
      "$B/build/host/fake-hal" "$hal" > "$T/fake-hal.out" 2>&1 & f=$!
      for i in $(seq 50); do grep -q ready "$T/fake-hal.out" && break; sleep 0.05; done
      "$@"; st=$?; kill -9 $f; wait $f 2>/dev/null; exit $st' sh "$T/binderfs" "$p/.fake/hal" "$@"
  }
  export B T

  # ---- battery: samsung (the nodes) on the S24, treble (the health HAL) on the S24 and on another phone
  if [ -n "$HAVE_HAL" ] && make -s -C "$B/battery" build/host/batteryctl > "$W/hw-build.txt" 2>&1; then
    local BATCTL=$B/battery/build/host/batteryctl PS=$S24/sys/class/power_supply
    mkdir -p "$PS/battery" "$PS/usb"
    echo 64 > "$PS/battery/capacity"; echo Charging > "$PS/battery/status"; echo 1 > "$PS/battery/present"; echo 301 > "$PS/battery/temp"
    echo 4100000 > "$PS/battery/voltage_now"; echo 900 > "$PS/battery/current_now"; echo 1 > "$PS/usb/online"
    printf 'level=64\nstatus=2\npresent=1\ntemp=301\nvoltage_mv=4100\ncurrent_ua=900000\nusb=1\nfull_uah=4900000\ndesign_uah=5000000\n' > "$S24/.fake/hal/health"
    printf 'level=18\nstatus=3\npresent=1\ntemp=355\nvoltage_mv=3650\ncurrent_ua=-610000\ncycles=320\nfull_uah=3700000\ndesign_uah=4500000\n' > "$OTHER/.fake/hal/health"
    bat() { local p=$1; shift; with_hal "$p" env ASAN_OPTIONS=detect_leaks=0 ONEIOS_BATTERY_DIR="$p/sys/class/power_supply" "$@" "$BATCTL"; }
    out=$(bat "$S24" 2>&1); st=$?
    [ $st = 0 ] && printf '%s\n' "$out" | grep -q '^bridges: hw: battery: backend samsung (the default)$' && printf '%s\n' "$out" | grep -q "^bridges: battery: on: $PS, status \"Charging\", 64%, plugged in" \
      && printf '%s\n' "$out" | grep -q '^sample: 64% present=1 plugged=1 charging=1 full=0 temp=301 (tenths C) voltage=4100 mV current=900 mA' \
      && ok "hw-backend: battery samsung (the default) on the S24: the power_supply nodes, 64%, charging, 900 mA" || bad "hw-backend: battery samsung on the S24" "status $st: $(printf '%s' "$out" | tr '\n' '|')"
    out=$(bat "$S24" ONEIOS_HW_BACKEND=treble 2>&1); st=$?
    [ $st = 0 ] && printf '%s\n' "$out" | grep -q '^bridges: hw: battery: backend treble (hw-backend=treble)$' \
      && printf '%s\n' "$out" | grep -q '^bridges: battery: on: the health HAL android.hardware.health.IHealth/default (version 2), status "Charging", 64%, plugged in' \
      && printf '%s\n' "$out" | grep -q '^sample: 64% present=1 plugged=1 charging=1 full=0 temp=301 (tenths C) voltage=4100 mV current=900 mA full=4900 mAh design=5000 mAh' \
      && ok "hw-backend: battery treble on the S24: the health HAL, the same 64%, charging, 900 mA" || bad "hw-backend: battery treble on the S24" "status $st: $(printf '%s' "$out" | tr '\n' '|')"
    out=$(bat "$OTHER" ONEIOS_HW_BACKEND_BATTERY=treble 2>&1); st=$?
    [ $st = 0 ] && printf '%s\n' "$out" | grep -q 'backend treble (hw-backend-battery=treble)' \
      && printf '%s\n' "$out" | grep -q '^sample: 18% present=1 plugged=0 charging=0 full=0 temp=355 (tenths C) voltage=3650 mV current=-610 mA full=3700 mAh design=4500 mAh cycles=320' \
      && ok "hw-backend: battery treble on another phone (no power_supply nodes): 18%, -610 mA, 320 cycles" || bad "hw-backend: battery treble on another phone" "status $st: $(printf '%s' "$out" | tr '\n' '|')"
    out=$(bat "$OTHER" 2>&1); st=$?
    [ $st = 1 ] && printf '%s\n' "$out" | grep -q 'battery: off: no readable' && ok "hw-backend: battery samsung on another phone with no power_supply nodes: off" || bad "hw-backend: battery samsung on another phone" "status $st: $(printf '%s' "$out" | tr '\n' '|')"
    rm "$S24/.fake/hal/health"
    out=$(bat "$S24" ONEIOS_HW_BACKEND=treble 2>&1); st=$?
    [ $st = 1 ] && [ "$(printf '%s\n' "$out" | grep -c 'treble: missing:')" = 1 ] \
      && printf '%s\n' "$out" | grep -q 'hw: battery: treble: missing: android.hardware.health.IHealth/default (no service: not registered .*); the bridge stays off (hw-backend-fallback=0)' \
      && ok "hw-backend: battery treble, no health HAL: one missing line, off" || bad "hw-backend: battery treble, no health HAL" "status $st: $(printf '%s' "$out" | tr '\n' '|')"
    out=$(bat "$S24" ONEIOS_HW_BACKEND=treble ONEIOS_HW_FALLBACK=1 2>&1); st=$?
    [ $st = 0 ] && printf '%s\n' "$out" | grep -q 'falling back to samsung (hw-backend-fallback=1)' && printf '%s\n' "$out" | grep -q '^backend: samsung$' && printf '%s\n' "$out" | grep -q '^sample: 64%' \
      && ok "hw-backend: battery treble, no health HAL, hw-backend-fallback=1: the nodes" || bad "hw-backend: battery fallback" "status $st: $(printf '%s' "$out" | tr '\n' '|')"
  elif [ -n "$HAVE_HAL" ]; then bad "hw-backend: batteryctl does not build" "$(tail -n 2 "$W/hw-build.txt" | tr '\n' '|')"; fi
  rm -rf "$T"
}
