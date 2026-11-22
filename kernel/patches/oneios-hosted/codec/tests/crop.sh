#!/bin/sh
# crop.sh TEST DIR: OneiOSCodecPicture.h's SPS crop on real encoders' streams. ffmpeg codes a 96x96 test picture
# (testsrc2) with each HEVC encoder it has of libx265 and hevc_videotoolbox (a Mac's), TEST (test_picture) crops every
# SPS of the stream to 84x84 (chronod's widget tile), and ffmpeg decodes the result: it must be 84x84, decode with no
# complaint and be the top-left 84x84 of the 96x96 picture (ffmpeg's own crop of the uncropped stream). The same for
# 128x96 to 120x90. Exit 0 all the same (or skipped: no ffmpeg, no HEVC encoder), 1 a difference, 2 usage.
[ $# -eq 2 ] && [ -x "$1" ] && [ -d "$2" ] || { sed -n '2,6p' "$0" >&2; exit 2; }
test=$1; dir=$2
command -v ffmpeg >/dev/null 2>&1 && command -v ffprobe >/dev/null 2>&1 || { echo "crop: no ffmpeg here: skipped"; exit 0; }
fail=0; done=0
for encoder in libx265 hevc_videotoolbox; do
  for size in 96x96:84x84 128x96:120x90; do
    coded=${size%%:*}; crop=${size##*:}; cw=${crop%x*}; ch=${crop#*x}; name=$dir/$encoder-$coded
    ffmpeg -nostdin -v error -f lavfi -i "testsrc2=size=$coded:rate=1" -frames:v 1 -pix_fmt yuv420p -c:v "$encoder" \
      -f hevc -y "$name.hevc" > "$name.encode.log" 2>&1 || { echo "crop: $encoder $coded: no stream (skipped): $(head -1 "$name.encode.log")"; continue; }
    "$test" stream "$name.hevc" "$cw" "$ch" "$name.cropped.hevc" > "$name.crop.log" 2>&1 || { echo "crop: $encoder $coded: $(cat "$name.crop.log")"; fail=1; continue; }
    got=$(ffprobe -v error -select_streams v -show_entries stream=width,height -of csv=p=0 "$name.cropped.hevc")
    [ "$got" = "$cw,$ch" ] || { echo "crop: $encoder $coded: the cropped stream is $got, not $cw,$ch"; fail=1; continue; }
    if ! ffmpeg -nostdin -v warning -xerror -err_detect explode -f hevc -i "$name.cropped.hevc" -f rawvideo -pix_fmt yuv420p \
         -y "$name.cropped.yuv" > "$name.decode.log" 2>&1 || [ -s "$name.decode.log" ]; then
      echo "crop: $encoder $coded: the decoder complained:"; sed 's/^/    /' "$name.decode.log" | head -5; fail=1; continue
    fi
    ffmpeg -nostdin -v error -f hevc -i "$name.hevc" -vf "crop=$cw:$ch:0:0" -f rawvideo -pix_fmt yuv420p -y "$name.expected.yuv" || { fail=1; continue; }
    if cmp -s "$name.cropped.yuv" "$name.expected.yuv"; then done=$((done + 1)); else echo "crop: $encoder $coded: the picture differs"; fail=1; fi
  done
done
[ "$done" -gt 0 ] || [ "$fail" = 1 ] || { echo "crop: no HEVC encoder in this ffmpeg: skipped"; exit 0; }
echo "crop: $done stream(s) cropped and decoded by $(ffmpeg -version | head -1 | cut -d' ' -f1-3) to the picture expected"
exit $fail
