#!/bin/sh
# Installs the receiver as a systemd service that starts at boot and restarts when it exits, on a
# board with the receiver at /opt/glassdeck/local (the Orange Pi stand-in).
# sh device/install-receiver-service.sh root@10.0.0.186
set -eu
board=${1:?usage: install-receiver-service.sh user@board}
here=$(cd "$(dirname "$0")" && pwd)
scp "$here/systemd/glassdeck-receiver.service" "$board:/etc/systemd/system/glassdeck-receiver.service"
ssh "$board" '
  set -e
  pkill -f "^./target/release/glassdeck-receiver" || true
  systemctl daemon-reload
  systemctl enable --now glassdeck-receiver
  sleep 2
  systemctl --no-pager --lines=3 status glassdeck-receiver
'
