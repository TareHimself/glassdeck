#!/bin/sh
# Started at boot in place of the stock launcher (see /usr/bin/flux-weston-autolaunch.sh). Starts
# the stock bridge exactly as stock does, so USB uploads keep working, then runs our receiver on
# KMS instead of Weston and the stock UI. Anything it can't do falls back to the stock launcher.
GLASSDECK=/home/root/glassdeck
RESCUE=$GLASSDECK/rescue/flux-weston-autolaunch.sh
ARMED=$GLASSDECK/armed
LOG=$GLASSDECK/launch.log
BIN=/usr/lib/glassdeck
# A receiver that stays up this long counts as a good boot.
CONFIRM_AFTER_S=60
RESTART_DELAY_S=2

mkdir -p "$GLASSDECK"
log() {
    echo "$(date '+%Y-%m-%d %H:%M:%S') $*" >> "$LOG"
}

run_stock() {
    log "running the stock launcher: $1"
    exec "$RESCUE"
}

[ -x "$RESCUE" ] || { log "no rescue copy of the stock launcher; nothing to fall back to"; exit 1; }
[ -x "$BIN/glassdeck-receiver" ] || run_stock "the receiver isn't installed"

# Filled in from the stock launcher in the probe report: the lines that start garfield (the
# stock bridge) and whatever it needs, without Weston or garfunkel. Until then this launcher
# runs stock unchanged, so an unfilled build is harmless.
start_stock_bridge() {
    return 1
}

start_stock_bridge || run_stock "start_stock_bridge isn't filled in yet"

# The receiver's packages and state need storage that survives a reboot.
state_root() {
    fstype=$(df -T /var/lib 2>/dev/null | awk 'NR==2{print $2}')
    probe=/var/lib/.glassdeck-write-test
    if [ "$fstype" != tmpfs ] && touch "$probe" 2>/dev/null; then
        rm -f "$probe"
        echo /var/lib/glassdeck
    else
        echo "$GLASSDECK"
    fi
}
ROOT=$(state_root)
mkdir -p "$ROOT/packages" "$ROOT/state"
log "receiver keeps packages and state in $ROOT"

# The boot counter (S04glassdeck-guard) falls back to stock after two boots that aren't
# confirmed; a receiver that stays up confirms this one.
confirm_when_up() {
    sleep "$CONFIRM_AFTER_S"
    if pidof glassdeck-receiver >/dev/null 2>&1 && [ -f "$ARMED" ]; then
        echo 0 > "$ARMED"
        sync
        log "confirmed: the receiver has been up ${CONFIRM_AFTER_S}s"
    fi
}
confirm_when_up &

while true; do
    "$BIN/glassdeck-receiver" --packages "$ROOT/packages/" --state "$ROOT/state/" >> "$GLASSDECK/receiver.log" 2>&1
    log "receiver exited with $?; restarting in ${RESTART_DELAY_S}s"
    tail -n 2000 "$GLASSDECK/receiver.log" > "$GLASSDECK/receiver.log.tmp" && mv -f "$GLASSDECK/receiver.log.tmp" "$GLASSDECK/receiver.log"
    sleep "$RESTART_DELAY_S"
done
