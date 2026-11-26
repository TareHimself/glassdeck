#!/bin/sh
# Builds firmware/target/thumbv6m-none-eabi/release/glassdeck-scanner.uf2 in Docker.
set -eu
here=$(cd "$(dirname "$0")" && pwd)
export MSYS_NO_PATHCONV=1
mount=$(cd "$here/.." && (pwd -W 2>/dev/null || pwd))
docker build -q -t glassdeck-firmware-build "$mount/firmware" >/dev/null
docker run --rm -v "$mount:/work" -v glassdeck-firmware-cargo:/usr/local/cargo/registry \
  -w /work/firmware glassdeck-firmware-build sh -c '
    cargo build --release "$@" &&
    elf2uf2-rs target/thumbv6m-none-eabi/release/glassdeck-scanner target/thumbv6m-none-eabi/release/glassdeck-scanner.uf2 &&
    "$(rustc --print sysroot)"/lib/rustlib/x86_64-unknown-linux-gnu/bin/llvm-size target/thumbv6m-none-eabi/release/glassdeck-scanner' sh "$@"
