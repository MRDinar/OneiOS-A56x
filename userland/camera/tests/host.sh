#!/bin/bash
# Host tests of the capture plug-in (CI runs every executable */tests/host.sh): the reader, run against stand-ins
# for /dev/oneios-camera; the stream formats against CMCapture 24A446's resolver rule and the scalers
# (tests/test_formats.c); and on a Mac the device itself against macOS's CMCapture (tests/host-mac.sh). The iOS
# build is build.sh (Xcode on the Mac).
set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."
out=$(mktemp)
trap 'rm -f "$out"' EXIT
${CC:-cc} -std=c11 -D_DEFAULT_SOURCE -Wall -Wextra -Werror -fsanitize=address,undefined -I. -I../../drivers/camera \
  oneios_camera_reader.c tests/test_reader.c -o "$out"
"$out"
# the stream formats CMCapture matches (its 24A446 rule) and the scalers: plain C
${CC:-cc} -std=c11 -D_DEFAULT_SOURCE -Wall -Wextra -Werror -fsanitize=address,undefined -I. -I../../drivers/camera \
  oneios_camera_formats.c oneios_camera_scale.c tests/test_formats.c -o "$out"
"$out"
tests/host-mac.sh 2>/dev/null | tail -1
