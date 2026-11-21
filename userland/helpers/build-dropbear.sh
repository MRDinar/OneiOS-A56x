#!/bin/bash
# Build arm64e dropbear for the guest (pubkey-only, port 2222 via oneios-init). Not part of build.sh because
# dropbear is vendored from ownervisor and pinned separately.
#
#   userland/helpers/build-dropbear.sh [OUT_DIR]
#
# Writes OUT_DIR/dropbear (signed). Also copies OUT_DIR/zsh if userland/helpers/build-zsh.sh was run first.
set -euo pipefail
HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
REPO=$(cd "$HERE/../.." && pwd)
OUT=${1:-$REPO/out/userland/helpers}
TAG=DROPBEAR_2022.83
WORK=$REPO/out/userland/dropbear-armv8e/work
URL="https://github.com/mkj/dropbear/archive/refs/tags/${TAG}.tar.gz"
SHA256=e02c5c36eb53bfcd3f417c6e40703a50ec790a1a772269ea156a2ccef14998d2
AUTHKEYS=/private/var/root/.ssh/authorized_keys
SHELL=/private/var/oneios/bin/zsh

. "$REPO/scripts/ios/env.sh"
. "$REPO/scripts/lib/oneios-tool.sh"
oneios_tool "$REPO"
mkdir -p "$OUT" "$WORK"
tarball=$WORK/${TAG}.tar.gz
if [ ! -f "$tarball" ] || [ "$(openssl dgst -sha256 "$tarball" | awk '{print $2}')" != "$SHA256" ]; then
  curl -fsSL -o "$tarball" "$URL"
fi
if [ "$(openssl dgst -sha256 "$tarball" | awk '{print $2}')" != "$SHA256" ]; then
  echo "build-dropbear.sh: tarball sha256 mismatch" >&2; exit 1
fi
src=$WORK/dropbear-$TAG
rm -rf "$src"
tar -xzf "$tarball" -C "$WORK"
# The login shell and the one authorized_keys (AUTHKEYS, SHELL above): dropbear-oneios.patch, made for this pin
# (fuzz 0: a moved pin fails here)
patch -s -F 0 -p1 -d "$src" < "$HERE/dropbear-oneios.patch"

cat >"$src/localoptions.h" <<'H'
#define DROPBEAR_SVR_PASSWORD_AUTH 0
#define DROPBEAR_SVR_PAM_AUTH 0
#define DROPBEAR_SVR_PUBKEY_AUTH 1
#define DROPBEAR_SVR_MULTIUSER 1
#define DROPBEAR_UTMP 0
#define DROPBEAR_WTMP 0
#define DROPBEAR_LASTLOG 0
#define DROPBEAR_PUTUTLINE 0
#define DO_MOTD 0
#define DEFAULT_PATH "/private/var/oneios/bin:/usr/bin:/bin:/usr/sbin:/sbin"
#define ED25519_PRIV_FILENAME "/private/var/oneios/ssh/dropbear_ed25519_host_key"
#define RSA_PRIV_FILENAME "/private/var/oneios/ssh/dropbear_rsa_host_key"
#define ECDSA_PRIV_FILENAME "/private/var/oneios/ssh/dropbear_ecdsa_host_key"
#define DSS_PRIV_FILENAME "/private/var/oneios/ssh/dropbear_dss_host_key"
H

export CC="$IOS_DIR/ios-clang"
(cd "$src" && ./configure --host=arm-apple-darwin CC="$CC" --disable-zlib --disable-lastlog --disable-utmp --disable-wtmp >/dev/null)
make -C "$src" PROGRAMS=dropbear dropbear -j"$(sysctl -n hw.ncpu)" >/dev/null

ENT=$OUT/dropbear-entitlements.plist
cat >"$ENT" <<'P'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict><key>com.apple.private.security.no-container</key><true/></dict></plist>
P
cp "$src/dropbear" "$OUT/dropbear"
"$ONEIOS_TOOL" ios sign --identifier com.oneios.dropbear --entitlements "$ENT" "$OUT/dropbear" >/dev/null
rm -f "$ENT"
"$ONEIOS_TOOL" image tweaks cdhash "$OUT/dropbear" | tee /tmp/dropbear-cdhash.txt
grep -q 'arm64e pages=4096' /tmp/dropbear-cdhash.txt || {
  echo "build-dropbear.sh: dropbear is not arm64e with 4 KiB pages (line above)" >&2; exit 1
}
rm -f /tmp/dropbear-cdhash.txt
echo "build-dropbear.sh: $OUT/dropbear"
