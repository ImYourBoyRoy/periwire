#!/usr/bin/env bash
# End-to-end lab test: share a device from THIS machine, import it on the lab server, mount READ-ONLY,
# eject it safely, and confirm it comes back. Never writes to the device. Needs tools/lab.env.
#   tools/e2e.sh run <vid:pid>     everything below, in order (vid:pid of a spare device, or set PERIWIRE_TEST_DEVICE)
#   tools/e2e.sh up <vid:pid>      start root daemons on both machines and pair them
#   tools/e2e.sh attach            import on the server, wait for the disk, mount read-only, list files
#   tools/e2e.sh eject             safe-eject on the server (unmounts first) and verify the device is back here
#   tools/e2e.sh down              stop both daemons gracefully (devices return to their owner)
#   tools/e2e.sh cli laptop|server ARGS...   run periwire on either side
#   tools/e2e.sh logs              both daemon logs
set -u
. "$(dirname "$0")/lib.sh"
need_lab
BIN="$PROJECT_DIR/target/release/periwire"; STATE="$PROJECT_DIR/.state/laptop"
RBIN="$REMOTE_DIR/periwire"; RID="$REMOTE_DIR/id"; PORT=3241
LAPTOP_IP="$(ip -4 route get "$REMOTE_HOST" | sed -n 's/.* src \([0-9.]*\).*/\1/p')"
die() { echo "ABORT: $*" >&2; exit 1; }
lap() { "$BIN" "$@"; }                                   # laptop CLI (talks to the root daemon's socket)
srv() { remote "$RBIN $(printf '%q ' "$@")"; }            # server CLI
ssh_srv_root() { remote_sudo "$1"; }

up() {
  need_spec "${1:-}"
  [ -x "$BIN" ] || die "build first: cargo build --release"
  systemctl is-active vhusbd >/dev/null 2>&1 && die "vhusbd (VirtualHere) is running; keep it stopped"
  # Refuse to take a device that is mounted here (the exporter would refuse too).
  for d in /sys/block/sd*; do
    [ -e "$d/device" ] || continue
    name="$(basename "$d")"
    if udevadm info -q property -n "/dev/$name" | grep -q "ID_VENDOR_ID=${SPEC%%:*}"; then
      mp="$(lsblk -no MOUNTPOINT "/dev/$name" | grep -v '^$' | head -1)"
      [ -z "$mp" ] || die "/dev/$name is mounted at $mp; unmount it first (udisksctl unmount -b /dev/${name}1)"
    fi
  done
  hr "deploy"; "$TOOLS_DIR/deploy.sh" || die "deploy failed"
  hr "laptop daemon (root)"
  run_sudo mkdir -p "$STATE"
  run_sudo sh -c "pkill -TERM -x periwire; sleep 1; PERIWIRE_TRACE='${PERIWIRE_TRACE:-}' PERIWIRE_HOME='$STATE' nohup '$BIN' daemon --listen 0.0.0.0:$PORT --share '$SPEC' >'$STATE/daemon.log' 2>&1 &"
  sleep 1.5; lap status | head -8
  hr "server daemon (root)"
  remote_sudo "pkill -TERM -x periwire; sleep 1; mkdir -p '$RID'; PERIWIRE_HOME='$RID' nohup '$RBIN' daemon --no-export >'$REMOTE_DIR/daemon.log' 2>&1 &"
  sleep 1.5; srv status | head -6
  hr "pair"
  srv trust host "$LAPTOP_IP:$PORT" --yes
  lap trust add "$(srv id)" --name server
}

attach() {
  hr "server sees"; srv remote "$LAPTOP_IP:$PORT" || die "remote list failed"
  BUSID="$(srv --json remote "$LAPTOP_IP:$PORT" | sed -n 's/.*"busid": "\([^"]*\)".*/\1/p' | head -1)"
  [ -n "$BUSID" ] || die "nothing shared"
  hr "attach $BUSID"; srv attach "$LAPTOP_IP:$PORT" "$BUSID" || die "attach failed"
  remote_sudo 'for i in $(seq 1 30); do lsblk -rno NAME,TRAN | awk "\$2==\"usb\"{f=1} END{exit !f}" && break; sleep 1; done; sleep 1; lsblk -o NAME,SIZE,TRAN,MODEL,FSTYPE,LABEL | grep -E "usb|NAME|part"'
  hr "read-only mount"
  remote_sudo 'for i in $(seq 1 20); do D=$(lsblk -rno NAME,TRAN | awk "\$2==\"usb\"{print \$1; exit}"); P=$(lsblk -rno NAME,TYPE | awk -v d="$D" "\$2==\"part\" && index(\$1,d)==1 {print \$1; exit}"); [ -n "$P" ] && break; sleep 1; done; [ -n "$P" ] || { echo "no partition"; exit 0; }; mkdir -p /mnt/periwire-ro; mount -o ro,noexec,nosuid /dev/$P /mnt/periwire-ro && ls /mnt/periwire-ro | head -20'
  hr "status"; srv status; echo "--- laptop"; lap status | sed -n 1,10p
}

eject() {
  hr "safe eject on server (mount is still open: it must be unmounted for us)"
  BUSID="$(srv --json status | sed -n 's/.*"busid": "\([^"]*\)".*/\1/p' | head -1)"
  srv detach "$BUSID" || die "detach failed"
  sleep 3; echo "--- laptop sees the device again:"; lsblk -o NAME,SIZE,TRAN,MODEL | grep -E "usb|NAME"
  lap status | sed -n 1,10p
}

down() {
  hr "stop daemons"
  remote_sudo "pkill -TERM -x periwire; sleep 3; echo server-stopped"
  run_sudo pkill -TERM -x periwire; sleep 3; echo laptop-stopped
  lsblk -o NAME,SIZE,TRAN | grep -E " usb|NAME"
}

case "${1:-}" in
  run) up "${2:-}"; attach; eject; down ;;
  up) up "${2:-}" ;;
  attach) attach ;;
  eject) eject ;;
  down) down ;;
  cli) side="$2"; shift 2; if [ "$side" = laptop ]; then lap "$@"; else srv "$@"; fi ;;
  logs) echo "--- laptop"; run_sudo cat "$STATE/daemon.log"; echo "--- server"; remote_sudo "cat '$REMOTE_DIR/daemon.log'" ;;
  *) sed -n 2,12p "$0" ;;
esac
