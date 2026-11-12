#!/usr/bin/env bash
# Software-ceiling benchmark: export the stick and import it back on THIS machine over loopback (network ~free).
# Read-only: only `dd` reads the block device. A temporary udev rule stops the desktop auto-mounting the copy.
. "$(dirname "$0")/lib.sh"
BIN="$PROJECT_DIR/target/release/periwire"; EXP="$PROJECT_DIR/.state/laptop"; IMP="$PROJECT_DIR/.state/laptop-imp"
need_spec "${1:-}"; BUSID="${2:-${PERIWIRE_TEST_BUSID:-}}"; [ -n "$BUSID" ] || { echo "ABORT: give the exporter-side busid as the second argument (or PERIWIRE_TEST_BUSID)" >&2; exit 1; }; RULE=/run/udev/rules.d/99-periwire-bench.rules
cleanup() {
  run_sudo pkill -TERM -x periwire 2>/dev/null; sleep 3
  run_sudo rm -f "$RULE"; run_sudo udevadm control --reload
}
trap cleanup EXIT
grep -q "$BUSID" <(lsblk -no MOUNTPOINT 2>/dev/null) ; lsblk -no MOUNTPOINT /dev/sdb* 2>/dev/null | grep -q . && { echo "ABORT: stick is mounted; unmount first"; exit 1; }
run_sudo mkdir -p /run/udev/rules.d "$EXP" "$IMP"
run_sudo sh -c "echo 'SUBSYSTEM==\"block\", ENV{ID_USB_DRIVER}==\"usb-storage\", ENV{UDISKS_IGNORE}=\"1\"' > $RULE"; run_sudo udevadm control --reload
run_sudo sh -c "PERIWIRE_HOME='$EXP' nohup '$BIN' serve --listen 127.0.0.1:3250 --share '$SPEC' >'$EXP/bench.log' 2>&1 &"; sleep 1
run_sudo env PERIWIRE_HOME="$IMP" "$BIN" trust-host 127.0.0.1:3250 >/dev/null
run_sudo env PERIWIRE_HOME="$EXP" "$BIN" trust "$(run_sudo env PERIWIRE_HOME="$IMP" "$BIN" id)" --name bench-importer >/dev/null
run_sudo sh -c "PERIWIRE_HOME='$IMP' nohup '$BIN' attach 127.0.0.1:3250 $BUSID >'$IMP/attach.log' 2>&1 &"
for i in $(seq 1 30); do lsblk -rno NAME,TRAN | grep -q " usb$" && break; sleep 1; done; sleep 2
D="$(lsblk -rno NAME,TRAN | grep " usb$" | cut -d' ' -f1 | head -1)"; echo "forwarded copy appeared as /dev/$D"
[ -n "$D" ] || { run_sudo cat "$EXP/bench.log" "$IMP/attach.log"; exit 1; }
run_sudo sh -c "echo 3 > /proc/sys/vm/drop_caches; dd if=/dev/${D}1 bs=4M count=${COUNT:-64} of=/dev/null 2>&1 | tail -1"
