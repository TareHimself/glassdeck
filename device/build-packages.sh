#!/bin/sh
# Builds the keyboard's .ipk packages into target/packages/. Run from the repo root:
# sh device/build-packages.sh
# The bootstrap also needs target/keyboard/ (sh device/keyboard-build.sh glassdeck-receiver
# glassdeck-gpu).
set -eu
python=${PYTHON:-python3}
out=target/packages
mkdir -p "$out"

for pkg in glassdeck-probe glassdeck-backup glassdeck-restore-stock; do
  "$python" device/build-ipk.py "device/packages/$pkg" -o "$out/$pkg.ipk"
done

for bin in glassdeck-receiver glassdeck-gpu; do
  [ -f "target/keyboard/$bin" ] || { echo "target/keyboard/$bin is missing; run sh device/keyboard-build.sh glassdeck-receiver glassdeck-gpu" >&2; exit 1; }
done
stage=$out/stage/glassdeck-bootstrap
rm -rf "$stage"
mkdir -p "$stage"
cp -r device/packages/glassdeck-bootstrap/. "$stage/"
cp target/keyboard/glassdeck-receiver target/keyboard/glassdeck-gpu "$stage/data/usr/lib/glassdeck/"
"$python" device/build-ipk.py "$stage" -o "$out/glassdeck-bootstrap.ipk"
