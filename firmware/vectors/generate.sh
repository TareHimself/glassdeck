#!/bin/sh
# Prints Keyglass's own firmware output for fixed inputs, which the Rust ports are tested against.
# sh firmware/vectors/generate.sh <keyglass checkout>
# The checkout must be at KEYGLASS_COMMIT, and Docker must have its firmware build image
# (docker build -t openflux-firmware-build -f keyboard/firmware/Dockerfile.build keyboard/firmware).
set -eu
KEYGLASS_COMMIT=91774a1
keyglass=${1:?usage: generate.sh <keyglass checkout>}
here=$(cd "$(dirname "$0")" && pwd)
head=$(git -C "$keyglass" rev-parse --short=7 HEAD)
[ "$head" = "$KEYGLASS_COMMIT" ] || { echo "keyglass is at $head, not $KEYGLASS_COMMIT" >&2; exit 1; }

export MSYS_NO_PATHCONV=1
mount_kg=$(cd "$keyglass" && (pwd -W 2>/dev/null || pwd))
mount_here=$(cd "$here" && (pwd -W 2>/dev/null || pwd))
for program in serial_link usb_commands; do
  docker run --rm -v "$mount_kg:/kg:ro" -v "$mount_here:/vectors:ro" openflux-firmware-build \
    sh /vectors/run-in-container.sh "$program" > "$here/$program.txt"
  echo "$here/$program.txt"
done
