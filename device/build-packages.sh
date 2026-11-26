#!/bin/sh
# Builds the keyboard's .ipk packages into target/packages/. Run from the repo root:
# sh device/build-packages.sh
# The bootstrap also needs target/keyboard/ (sh device/keyboard-build.sh glassdeck-receiver
# glassdeck-gpu) and is skipped until its launcher knows how stock starts the bridge.
set -eu
python=${PYTHON:-python3}
out=target/packages
mkdir -p "$out"

for pkg in glassdeck-probe glassdeck-backup glassdeck-restore-stock; do
  "$python" device/build-ipk.py "device/packages/$pkg" -o "$out/$pkg.ipk"
done

launcher=device/packages/glassdeck-bootstrap/data/usr/lib/glassdeck/glassdeck-launch.sh
if tr -d '\r' < "$launcher" | grep -A1 '^start_stock_bridge() {' | grep -q 'return 1'; then
  echo "glassdeck-bootstrap not built: start_stock_bridge in $launcher isn't filled in from the probe report yet" >&2
  exit 0
fi
for bin in glassdeck-receiver glassdeck-gpu; do
  [ -f "target/keyboard/$bin" ] || { echo "target/keyboard/$bin is missing; run sh device/keyboard-build.sh glassdeck-receiver glassdeck-gpu" >&2; exit 1; }
done
stage=$out/stage/glassdeck-bootstrap
rm -rf "$stage"
mkdir -p "$stage"
cp -r device/packages/glassdeck-bootstrap/. "$stage/"
cp target/keyboard/glassdeck-receiver target/keyboard/glassdeck-gpu "$stage/data/usr/lib/glassdeck/"
"$python" device/build-ipk.py "$stage" -o "$out/glassdeck-bootstrap.ipk"
