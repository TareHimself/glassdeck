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
    pgrep -x weston >/dev/null 2>&1 || /etc/init.d/S49weston start >> "$LOG" 2>&1
    exec "$RESCUE"
}

[ -x "$RESCUE" ] || { log "no rescue copy of the stock launcher; nothing to fall back to"; exit 1; }
[ -x "$BIN/glassdeck-receiver" ] || run_stock "the receiver isn't installed"

# Runs Flux's own launcher without its last line, which starts garfunkel (the stock UI) in the
# foreground. Everything before it stays stock: the GPU clock cap, NTP, the IPK monitor and
# garfield, each under its own watchdog in the background. A launcher that doesn't look like
# the one we know (v1.0.13) isn't run this way.
STOCK_BRIDGE=/tmp/glassdeck-stock-bridge.sh
GARFIELD_WAIT_S=30
start_stock_bridge() {
    grep -qx 'start_garfield_with_watchdog &' "$RESCUE" || { log "the stock launcher doesn't start garfield the way we know"; return 1; }
    grep -qx 'start_garfunkel_with_watchdog' "$RESCUE" || { log "the stock launcher doesn't end with garfunkel the way we know"; return 1; }
    grep -vx 'start_garfunkel_with_watchdog' "$RESCUE" > "$STOCK_BRIDGE" || return 1
    bash "$STOCK_BRIDGE" >> "$GLASSDECK/stock-bridge.log" 2>&1 &
    waited=0
    until pgrep -f "garfield /dev/ffs-garfield" >/dev/null 2>&1; do
        [ "$waited" -ge "$GARFIELD_WAIT_S" ] && { log "garfield didn't start within ${GARFIELD_WAIT_S}s"; return 1; }
        sleep 1
        waited=$((waited + 1))
    done
    log "garfield is running under the stock watchdog"
}

# Weston (started by init before this launcher) holds the display; the receiver needs KMS.
stop_weston() {
    /etc/init.d/S49weston stop >> "$LOG" 2>&1
    waited=0
    while pgrep -x weston >/dev/null 2>&1; do
        [ "$waited" -ge 10 ] && { log "weston didn't stop"; return 1; }
        sleep 1
        waited=$((waited + 1))
    done
}

# The panel is mounted the way Flux's Weston config says; the receiver turns it the same way.
WESTON_INI=/etc/xdg/weston/weston.ini
# What each key does, as Flux keeps it (Polymath edits it); the labels follow it.
FLUX_KEYMAP=/home/root/flux/config/keymap.json
# The key scanner's serial link; garfunkel read it, and with garfunkel not started the receiver does.
SCANNER_UART=/dev/ttyS3
panel_rotation() {
    transform=$(sed -n 's/^[[:space:]]*transform=[[:space:]]*//p' "$WESTON_INI" 2>/dev/null | head -1)
    case "$transform" in
        "" | normal) echo 0 ;;
        rotate-180) echo 180 ;;
        *) return 1 ;;
    esac
}
ROTATION=$(panel_rotation) || run_stock "the panel is mounted in a way the receiver can't turn ($WESTON_INI)"

start_stock_bridge || run_stock "garfield couldn't be started the stock way"
stop_weston || run_stock "weston kept the display"

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

# The receiver's own USB function, added beside garfield's in garfield's gadget only while the
# receiver runs. Garfield's function is never touched; while ours is in, the device is composite and
# gets its own bcdDevice, so Windows reads the descriptors that pick WinUSB again.
GADGET=/sys/kernel/config/usb_gadget/g1
FUNCTION=$GADGET/functions/ffs.glassdeck
LINK=$GADGET/configs/c.1/ffs.glassdeck
FFS=/dev/ffs-glassdeck
COMPOSITE_BCD=0x4744
SAVED_BCD=/tmp/glassdeck-usb-bcd
SAVED_UDC=/tmp/glassdeck-usb-udc
EP1_WAIT_S=10

usb_udc() {
    if [ -s "$SAVED_UDC" ]; then cat "$SAVED_UDC"; else ls /sys/class/udc | head -1; fi
}

# Unbinds the controller and adds our function; the receiver then writes its descriptors.
usb_prepare() {
    [ -d "$GADGET" ] || { log "no USB gadget at $GADGET"; return 1; }
    udc=$(cat "$GADGET/UDC" 2>/dev/null)
    [ -n "$udc" ] || { log "garfield's USB gadget isn't bound"; return 1; }
    echo "$udc" > "$SAVED_UDC"
    cat "$GADGET/bcdDevice" > "$SAVED_BCD" || return 1
    echo "" > "$GADGET/UDC" || return 1
    echo "$COMPOSITE_BCD" > "$GADGET/bcdDevice" || return 1
    mkdir -p "$FUNCTION" || return 1
    [ -e "$LINK" ] || ln -s "$FUNCTION" "$LINK" || return 1
    mkdir -p "$FFS" || return 1
    mountpoint -q "$FFS" || mount -t functionfs glassdeck "$FFS" || return 1
}

# FunctionFS makes ep1 once the receiver's descriptors are in; the controller binds only then.
usb_bind_when_ready() {
    waited=0
    until [ -e "$FFS/ep1" ]; do
        [ "$waited" -ge "$EP1_WAIT_S" ] && { log "the receiver didn't set up its USB function within ${EP1_WAIT_S}s"; return 1; }
        sleep 1
        waited=$((waited + 1))
    done
    usb_udc > "$GADGET/UDC"
}

# Takes our function out and binds garfield's gadget as it was. Safe to run at any stage.
usb_down() {
    echo "" > "$GADGET/UDC" 2>/dev/null
    rm -f "$LINK"
    umount -l "$FFS" 2>/dev/null
    rmdir "$FUNCTION" 2>/dev/null
    [ -s "$SAVED_BCD" ] && cat "$SAVED_BCD" > "$GADGET/bcdDevice"
    usb_udc > "$GADGET/UDC" || log "couldn't bind the USB controller again"
}

run_receiver() {
    "$BIN/glassdeck-receiver" --panel-rotation "$ROTATION" --keymap "$FLUX_KEYMAP" --scanner-uart "$SCANNER_UART" --packages "$ROOT/packages/" --state "$ROOT/state/" "$@" >> "$GLASSDECK/receiver.log" 2>&1
}

while true; do
    if usb_prepare; then
        run_receiver --usb-function "$FFS" &
        receiver=$!
        if usb_bind_when_ready; then
            log "USB link up"
        else
            usb_down
        fi
        wait "$receiver"
        status=$?
        usb_down
    else
        log "couldn't add the USB function; running the receiver without it"
        usb_down
        run_receiver
        status=$?
    fi
    log "receiver exited with $status; restarting in ${RESTART_DELAY_S}s"
    tail -n 2000 "$GLASSDECK/receiver.log" > "$GLASSDECK/receiver.log.tmp" && mv -f "$GLASSDECK/receiver.log.tmp" "$GLASSDECK/receiver.log"
    sleep "$RESTART_DELAY_S"
done
