#!/bin/bash
# Build the display daemon and the audio tap for each userland Volumio
# runs on, and copy them into bin/<arch>/ and lib/<arch>/, named as Debian
# names the userland: armhf (the 32-bit Raspberry Pi image, whatever its
# kernel), arm64 and amd64. Devices run these files; they do not run cargo.
#
# Needs the cross linkers of .cargo/config.toml (gcc-arm-linux-gnueabihf,
# gcc-aarch64-linux-gnu) and the pinned toolchain; the builder image of
# scripts/builder/Dockerfile has them all.

set -euo pipefail
ROOT=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
cd "$ROOT"
unset CARGO_TARGET_DIR

# The ALSA library of each target, for the tap's link step (its build
# script finds it under target/sysroot/<arch>): Debian bookworm's own
# package, held against the digest Debian's index gives for it.
fetch_asound() {
  local deb_arch=$1
  local multiarch=$2
  local sha=$3
  local deb="target/sysroot/asound-${deb_arch}.deb"
  local so="target/sysroot/$deb_arch/usr/lib/$multiarch/libasound.so.2"
  if [ ! -e "$so" ]; then
    mkdir -p "target/sysroot/$deb_arch"
    "$ROOT/scripts/fetch.sh" "http://deb.debian.org/debian/pool/main/a/alsa-lib/libasound2_1.2.8-1+b1_${deb_arch}.deb" "$deb" "$sha"
    dpkg-deb -x "$deb" "target/sysroot/$deb_arch"
  fi
}
fetch_asound armhf arm-linux-gnueabihf 97718ff5e552c9a7b9e77e8debbe219b0068a9ab5dcabedd0800c69dd8f0eb96
fetch_asound arm64 aarch64-linux-gnu 9fa889400fcee4b92c8f4a2fafbb7f2cd33444d9ec1665a71002ab67c06114bb
fetch_asound amd64 x86_64-linux-gnu 44c77b076a7b11ae99712439022d822245b1994c435da564ebd320bb676faf4c

ship() {
  local triple=$1
  local arch=$2
  local stripper=$3
  echo "ship: $arch ($triple)"
  cargo build --release --locked --target "$triple" -p lcd1602-vumeter -p vutap-alsa
  install -D -m 755 "target/$triple/release/lcd1602-vumeter" "bin/$arch/lcd1602-vumeter"
  install -D -m 644 "target/$triple/release/libvutap.so" "lib/$arch/libvutap.so"
  "$stripper" "bin/$arch/lcd1602-vumeter" "lib/$arch/libvutap.so"
}

ship x86_64-unknown-linux-gnu amd64 strip
ship armv7-unknown-linux-gnueabihf armhf arm-linux-gnueabihf-strip
ship aarch64-unknown-linux-gnu arm64 aarch64-linux-gnu-strip

# Volumio bookworm runs glibc 2.36 on every architecture. The cross
# toolchain's sysroot is newer, and a symbol versioned above 2.36 (even a
# weak one) makes the loader refuse the whole binary on the player.
echo "ship: glibc"
glibc_max() {
  readelf -W --dyn-syms "$1" | grep -o 'GLIBC_2\.[0-9]*' | sort -t. -k2,2n -u | tail -n1
}
for bin in bin/*/lcd1602-vumeter lib/*/libvutap.so; do
  version=$(glibc_max "$bin")
  case "$version" in
    GLIBC_2.3[7-9]|GLIBC_2.[4-9]*|GLIBC_2.[1-9][0-9][0-9])
      echo "ship: $bin needs $version, newer than Volumio's glibc 2.36" >&2
      readelf -W --dyn-syms "$bin" | grep "$version" >&2
      exit 1
      ;;
  esac
  echo "ship: $bin needs at most ${version:-no versioned glibc symbol}"
done

echo "ship: payload"
file bin/*/lcd1602-vumeter lib/*/libvutap.so
