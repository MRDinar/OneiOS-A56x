#!/bin/bash
# Pack the built kernel for a release, and (with --upload) publish it as the release kernel-<build id> of this
# repository, from where the release workflow takes it (docs/releases.md, "The kernel in a release").
#   kernel/publish.sh                    pack out/kernel/current (the VM kernel, kernel/build-linux.sh)
#   kernel/publish.sh --hosted NAME      pack out/kernel/current-hosted-NAME (kernel/build-hosted.sh HOSTED_OUT=NAME;
#                                        NAME lead: out/kernel/current-hosted). The kernel xnuhost runs, and the one
#                                        the OneOS app prelinks on the phone; only ONEIOS_CRYPTO=original is releasable
#   KERNEL_DIR=DIR kernel/publish.sh     pack DIR (a folder with xnu.macho, xnu.macho.build.json and series.txt)
#   ... --upload                         the same, then create the GitHub release and upload the archive (asks
#                                        nothing: only run it when the build is the one to release)
# Output: out/kernel/publish/<build id>.tar.zst (xnu-12377...-<hex>.tar.zst), its SHA-256 and the line for kernel/release-kernel.txt.
# The archive holds xnu.macho, xnu.macho.build.json and series.txt, and nothing else. Before packing, the same
# check as the runtime package runs (`oneios-tool package check`, tools/oneios-tool/crates/oneios-package): a kernel
# that is prelinked, has kernelcache kexts, links a non-redistributable source (the record's restricted_sources),
# has no record of this builder or a record of other bytes is refused.
set -euo pipefail
ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
UPLOAD=0
SRC=${KERNEL_DIR:-}
while [ $# -gt 0 ]; do
  case $1 in
    --upload) UPLOAD=1 ;;
    --hosted)
      [ -n "${2:-}" ] || { echo "publish: --hosted NAME (the HOSTED_OUT of kernel/build-hosted.sh; lead: the default tree)" >&2; exit 2; }
      [ -z "${KERNEL_DIR:-}" ] || { echo "publish: KERNEL_DIR and --hosted name two kernels" >&2; exit 2; }
      if [ "$2" = lead ]; then SRC=$ROOT/out/kernel/current-hosted; else SRC=$ROOT/out/kernel/current-hosted-$2; fi
      shift ;;
    *) echo "publish: unknown argument $1 (--hosted NAME, --upload)" >&2; exit 2 ;;
  esac
  shift
done
SRC=${SRC:-$ROOT/out/kernel/current}
[ -f "$SRC/xnu.macho" ] && [ -f "$SRC/xnu.macho.build.json" ] || {
  echo "publish: no kernel with a build record at $SRC (kernel/build-linux.sh, or kernel/build-hosted.sh for --hosted)" >&2; exit 1; }
. "$ROOT/scripts/lib/oneios-tool.sh"
oneios_tool "$ROOT"
ID=$(sed -n 's/^  "build_id": *"\([^"]*\)".*/\1/p' "$SRC/xnu.macho.build.json" | head -1)
[ -n "$ID" ] || { echo "publish: $SRC/xnu.macho.build.json has no build_id" >&2; exit 1; }
OUT=$ROOT/out/kernel/publish
STAGE=$OUT/.stage
rm -rf "$STAGE"; mkdir -p "$STAGE"
trap 'rm -rf "$STAGE"' EXIT
cp "$SRC/xnu.macho" "$SRC/xnu.macho.build.json" "$STAGE/"
[ -f "$SRC/series.txt" ] && cp "$SRC/series.txt" "$STAGE/"
chmod 0644 "$STAGE"/*
"$ONEIOS_TOOL" package check "$STAGE"
EPOCH=$(git -C "$ROOT" log -1 --format=%ct)
ARCHIVE=$OUT/$ID.tar.zst
( cd "$STAGE" && ls | LC_ALL=C sort > "$OUT/.members" )
if tar --version 2>/dev/null | grep -q 'GNU tar'; then
  tar -C "$STAGE" --format=ustar --owner=0 --group=0 --numeric-owner --mtime="@$EPOCH" -T "$OUT/.members" -cf - | zstd -q -19 -f -o "$ARCHIVE"
else
  # macOS's bsdtar has no --mtime: the same ustar layout from oneios-tool (runtime/package.sh does the same)
  "$ONEIOS_TOOL" package archive "$STAGE" "$OUT/.members" "$EPOCH" "$OUT/.archive.tar"
  zstd -q -19 -f "$OUT/.archive.tar" -o "$ARCHIVE"; rm -f "$OUT/.archive.tar"
fi
rm -f "$OUT/.members"
SHA=$( { sha256sum "$ARCHIVE" 2>/dev/null || shasum -a 256 "$ARCHIVE"; } | cut -d" " -f1)
echo "publish: $SRC -> $ARCHIVE"
echo "publish: line for kernel/release-kernel.txt:"
echo "kernel-$ID $SHA"
if [ "$UPLOAD" = 1 ]; then
  gh release create "kernel-$ID" --repo akshaynexus/OneiOS --prerelease --title "XNU $ID" \
    --notes "Source-built XNU for the OneiOS runtime package (APSL 2.0; kernel/NOTICE.kernel). Not Apple's kernelcache: no Apple kernel extension inside. Build record in the archive." \
    "$ARCHIVE"
fi
