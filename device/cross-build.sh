#!/bin/sh
# Builds a crate for the keyboard (aarch64) in WSL. Run from Windows: wsl -d Ubuntu -- sh device/cross-build.sh [crate]
# Needs gcc/g++-aarch64-linux-gnu, the aarch64-unknown-linux-gnu Rust target, and Rockchip MPP copied
# from the board into $GLASSDECK_SYSROOT (usr/local/lib/librockchip_mpp.so*, its pkgconfig and headers).
set -eu
crate=${1:-glassdeck-receiver}
sysroot=${GLASSDECK_SYSROOT:-$HOME/glassdeck-sysroot}
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$HOME/glassdeck-target}"
export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc
export CC_aarch64_unknown_linux_gnu=aarch64-linux-gnu-gcc
export CXX_aarch64_unknown_linux_gnu=aarch64-linux-gnu-g++
export PKG_CONFIG_ALLOW_CROSS=1
export PKG_CONFIG_SYSROOT_DIR="$sysroot"
export PKG_CONFIG_LIBDIR="$sysroot/usr/local/lib/pkgconfig"
cd "$(dirname "$0")/.."
cargo build --release --target aarch64-unknown-linux-gnu -p "$crate"
echo "$CARGO_TARGET_DIR/aarch64-unknown-linux-gnu/release/$crate"
