#!/bin/sh
# Stage OneiOS files on the phone over adb, as the OneOS fork's app would (docs/building.md, "Test cycle on the phone").
#
#   scripts/phone/stage.sh runtime DIR    DIR: an unpacked runtime package (runtime.json at its top). Goes through the
#                                         fork's own root script (oneios_root.sh begin / receive / finish), so
#                                         runtime.sha256, runtime.ids and runtime.conf are written as an import does.
#   scripts/phone/stage.sh runtime-asis DIR   the same, but each file's size and SHA-256 are the file's own, not
#                                         runtime.json's: to put back a backed-up stage whose files were replaced by
#                                         hand (the phone's 0.1.6 of 2026-11-11). Modes still come from runtime.json.
#   scripts/phone/stage.sh image DIR      DIR: some files of an image set, at their paths in the set (kernel.kc,
#                                         kernel.kc.hashes, manifest.json, overlay/userland/xnuhost.conf ...). Each one
#                                         replaces the staged file, and its lines in image.sha256 and image.ids are
#                                         written again (the preflight checks both). The rest of image/ is untouched.
#   scripts/phone/stage.sh sums           print the sha256 of the staged kernel.kc, kernel.kc.hashes, manifest.json,
#                                         data.seed's line in image.sha256, xnuhost.conf and every runtime file
#
# Needs: adb with one phone, KernelSU root (su), the fork's stage at /data/local/tmp/oneios with bin/oneios_root.sh;
# for runtime: oneios-tool (found or built as the build scripts do, ONEIOS_TOOL=PATH for another), sha256sum or shasum.
# `oneios-tool phone stage` does the same (tools/oneios-tool/README.md "phone"); this script stays as its reference
# until that has run on the phone.
# Refuses while a session is open (session/xnuhost.pid names a live process). Changes nothing of One UI's.
set -eu
STAGE=/data/local/tmp/oneios
TMP=/data/local/tmp/oneios-stage-tmp
die() { echo "stage.sh: $*" >&2; exit 1; }
root() { adb shell "su -c 'sh -s'"; }
size() { wc -c < "$1" | tr -d ' '; }
sha() { if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1"; else shasum -a 256 "$1"; fi | cut -d' ' -f1; }
open_check() {
	# Open: the recorded pid is alive and still an xnuhost (a pid is reused after the session ends).
	r=$(printf '%s\n' "p=\$(cat $STAGE/session/xnuhost.pid 2>/dev/null); [ -n \"\$p\" ] && cat /proc/\$p/cmdline 2>/dev/null | tr '\\0' ' ' | grep -q xnuhost && echo open || echo closed" | root | tr -d '\r')
	[ "$r" = closed ] || die "a session is open: return to One UI first"
}
case "${1:-}" in
runtime|runtime-asis)
	asis=0; [ "$1" = runtime ] || asis=1
	d=${2:?DIR}; [ -f "$d/runtime.json" ] || die "$d has no runtime.json"
	ROOT=$(cd "$(dirname "$0")/../.." && pwd)
	. "$ROOT/scripts/lib/oneios-tool.sh"
	oneios_tool "$ROOT" || die "runtime.json is read with oneios-tool (Rust): cargo build --release --manifest-path tools/oneios-tool/Cargo.toml"
	open_check
	adb shell "rm -rf $TMP" && adb push "$d" "$TMP" >/dev/null
	# The root script's lines, from runtime.json (read with `oneios-tool json`: a Python block wrote the same lines
	# until 2026-11-21).
	j() { "$ONEIOS_TOOL" json "$@"; }
	r=$d/runtime.json
	tab=$(printf '\t')
	n=$(( $(j len "$r" files) + 1 ))
	case "$(j get --default false "$r" placeholder)" in false|null|0|0.0|'""'|'[]'|'{}'|'') ph=0 ;; *) ph=1 ;; esac
	{
		echo "set -e; R=\"sh $STAGE/bin/oneios_root.sh\""
		echo '$R begin runtime'
		echo "\$R receive runtime runtime.json $(size "$r") $(sha "$r") 0644 < $TMP/runtime.json"
		j lines --missing 0644 "$r" files path size sha256 mode | while IFS=$tab read -r p s h m; do
			[ "$asis" = 0 ] || { s=$(size "$d/$p"); h=$(sha "$d/$p"); }
			echo "\$R receive runtime $p $s $h $m < $TMP/$p"
		done
		echo "\$R finish runtime $n $(j get "$r" version) $(j get "$r" session_abi) $(j get "$r" xnuhost) $(j lines "$r" image_formats . | paste -sd, -) $ph $(j get --default none "$r" kernel.build_id) $(j get --default none "$r" kernel.sha256) $(j get --default none "$r" kernel.xnu)"
		echo "cat $STAGE/runtime.conf; rm -rf $TMP"
	} | root
	;;
image)
	d=${2:?DIR}; [ -d "$d" ] || die "no folder $d"
	open_check
	adb shell "rm -rf $TMP" && adb push "$d" "$TMP" >/dev/null
	list=$(cd "$d" && find . -type f | sed 's|^\./||' | sort)
	{
		echo "set -e; cd $STAGE"
		for p in $list; do
			echo "f=image/$p; [ -f \"\$f\" ] || { echo \"not staged: \$f\"; exit 1; }"
			echo "cp $TMP/$p \"\$f.part\" && chown root:root \"\$f.part\" && chmod 0644 \"\$f.part\" && mv -f \"\$f.part\" \"\$f\""
			echo "s=\$(sha256sum \"\$f\" | cut -d' ' -f1); i=\$(stat -c '%d:%i:%s:%Y' \"\$f\")"
			echo "awk -v p='$p' -v s=\"\$s\" '\$2 == p { print s \"  \" p; next } { print }' image.sha256 > image.sha256.new && mv -f image.sha256.new image.sha256"
			echo "awk -v p='$p' -v i=\"\$i\" '\$2 == p { print i \" \" p; next } { print }' image.ids > image.ids.new && mv -f image.ids.new image.ids"
			echo "echo \"staged \$s  \$f\""
		done
		echo "chmod 0600 image.sha256 image.ids; sync; rm -rf $TMP"
	} | root
	;;
sums)
	printf '%s\n' "cd $STAGE && sha256sum image/kernel.kc image/kernel.kc.hashes image/manifest.json image/overlay/userland/xnuhost.conf && grep ' data.seed\$' image.sha256 && cat runtime.conf && cat runtime.sha256" | root
	;;
*) sed -n '2,15p' "$0"; exit 2 ;;
esac
