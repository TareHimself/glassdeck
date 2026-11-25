#!/bin/sh
# Builds keyboard programs for the stock keyboard's glibc 2.36, in Docker. Run from the repo root (Git
# Bash or WSL): sh device/keyboard-build.sh [crate ...]. Binaries land in target/keyboard/.
#
# Linking needs Rockchip MPP's headers and library in target/keyboard-sysroot/usr/local, taken from any
# board with MPP installed: sh device/keyboard-build.sh --fetch-mpp root@10.0.0.186
# libstdc++ is linked statically, because the keyboard doesn't ship it. MPP links against our copy with
# --allow-shlib-undefined; at run time the keyboard's own librockchip_mpp.so.1 is used.
set -eu
image=glassdeck-keyboard-build
sysroot=target/keyboard-sysroot
export MSYS_NO_PATHCONV=1

if [ "${1:-}" = "--fetch-mpp" ]; then
  board=${2:?usage: --fetch-mpp user@board}
  mkdir -p "$sysroot/usr/local"
  ssh "$board" 'tar -C /usr/local -ch include/rockchip lib/pkgconfig/rockchip_mpp.pc $(cd /usr/local && ls lib/librockchip_mpp.so*)' \
    | tar -x -C "$sysroot/usr/local"
  # A board with glibc 2.38+ builds MPP against libmvec, which glibc 2.36 doesn't have on aarch64.
  sed -i 's/ -lmvec//' "$sysroot/usr/local/lib/pkgconfig/rockchip_mpp.pc"
  echo "MPP from $board is in $sysroot"
  exit 0
fi

[ -f "$sysroot/usr/local/lib/pkgconfig/rockchip_mpp.pc" ] \
  || { echo "no MPP in $sysroot; run: sh device/keyboard-build.sh --fetch-mpp user@board" >&2; exit 1; }
[ $# -gt 0 ] || set -- glassdeck-receiver glassdeck-scene

docker build -q -t "$image" -f device/keyboard.Dockerfile device >/dev/null
repo=$(pwd -W 2>/dev/null || pwd)
packages=""
copies=""
for crate in "$@"; do
  packages="$packages -p $crate"
  copies="$copies /target/aarch64-unknown-linux-gnu/release/$crate"
done
mkdir -p target/keyboard
docker run --rm \
  -v "$repo:/src" -v "$repo/$sysroot:/sysroot:ro" \
  -v glassdeck-keyboard-cargo:/usr/local/cargo/registry -v glassdeck-keyboard-target:/target \
  -w /src "$image" \
  sh -c "cargo build --release --target aarch64-unknown-linux-gnu $packages && cp $copies /src/target/keyboard/"
for crate in "$@"; do echo "target/keyboard/$crate"; done
