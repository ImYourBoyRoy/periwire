#!/usr/bin/env bash
# End-to-end test of the macOS importer on a THROWAWAY SIP-relaxed Mac guest (see docs/MACOS.md).
# This machine exports a USB device; the Mac guest imports it. Never use the production macOS guest.
#   PERIWIRE_GUEST_HOST=<test-vm-ip> tools/mac-e2e.sh up <vid:pid>   start both daemons and pair them
#   ... attach      import the first shared device on the Mac, show it to macOS (read-only by default for NTFS)
#   ... status      both sides
#   ... down        stop both daemons gracefully (the device returns to this machine)
set -u
[ -n "${PERIWIRE_GUEST_HOST:-}" ] || { echo "set PERIWIRE_GUEST_HOST to the throwaway test VM's IP" >&2; exit 1; }
. "$(dirname "$0")/lib.sh"
need_lab
BIN="$PROJECT_DIR/target/release/periwire"; STATE="$PROJECT_DIR/.state/laptop"; PORT=3241
GBIN='$HOME/periwire/target/release/periwire'; GENV='PERIWIRE_HOME=$HOME/.periwire-test PERIWIRE_SOCKET=/tmp/periwire-test.sock'
LAPTOP_IP="$(ip -4 route get "$REMOTE_HOST" | sed -n 's/.* src \([0-9.]*\).*/\1/p')"
die() { echo "ABORT: $*" >&2; exit 1; }
lap() { "$BIN" "$@"; }
mac() { guest "export $GENV; $GBIN $(printf '%q ' "$@")"; }

up() {
  need_spec "${1:-}"
  [ -x "$BIN" ] || die "build first: cargo build --release"
  systemctl is-active vhusbd >/dev/null 2>&1 && die "vhusbd (VirtualHere) is running; keep it stopped"
  for d in /sys/block/sd*; do   # the exporter refuses mounted devices; unmount (safe, no writes) so it can take it
    [ -e "$d/device" ] || continue; name="$(basename "$d")"
    udevadm info -q property -n "/dev/$name" | grep -q "ID_VENDOR_ID=${SPEC%%:*}" || continue
    for p in $(lsblk -lno NAME "/dev/$name" | tail -n +2); do mountpoint -q "$(lsblk -no MOUNTPOINT "/dev/$p" | head -1)" 2>/dev/null && { echo "unmounting /dev/$p"; udisksctl unmount -b "/dev/$p" || die "could not unmount /dev/$p"; }; done
  done
  hr "laptop daemon (root)"
  run_sudo mkdir -p "$STATE"
  # wait for a previous daemon to be really gone (a macOS importer needs a few seconds to tear its virtual controller down): a new daemon
  # started too early refuses with "another periwire daemon is already running"
  run_sudo sh -c "pkill -TERM -x periwire; for i in \$(seq 1 30); do pgrep -x periwire >/dev/null || break; sleep 1; done; PERIWIRE_TRACE='${PERIWIRE_TRACE:-}' PERIWIRE_HOME='$STATE' nohup '$BIN' daemon --listen 0.0.0.0:$PORT --share '$SPEC' >'$STATE/daemon.log' 2>&1 &"
  sleep 1.5; lap status | head -8
  hr "mac daemon (user, no sudo)"
  guest "pkill -TERM -x periwire; for i in \$(seq 1 30); do pgrep -x periwire >/dev/null || break; sleep 1; done; mkdir -p \$HOME/.periwire-test; export $GENV PERIWIRE_TRACE='${PERIWIRE_TRACE:-}'; nohup $GBIN daemon --no-export >\$HOME/.periwire-test/daemon.log 2>&1 &"
  sleep 2; mac status | head -6
  hr "pair"
  mac trust host "$LAPTOP_IP:$PORT" --yes
  lap trust add "$(mac id)" --name mac-test
}
attach() {
  hr "mac sees"; mac remote "$LAPTOP_IP:$PORT" || die "remote list failed (laptop $LAPTOP_IP:$PORT unreachable from the Mac?)"
  BUSID="$(mac --json remote "$LAPTOP_IP:$PORT" | sed -n 's/.*"busid": "\([^"]*\)".*/\1/p' | head -1)"
  [ -n "$BUSID" ] || die "nothing shared"
  hr "attach $BUSID"; mac attach --auto "$LAPTOP_IP:$PORT" "$BUSID" || die "attach failed"   # --auto: devices such as the iPad re-enumerate by themselves and must be re-attached
  sleep 6; hr "what macOS sees"
  guest 'system_profiler SPUSBDataType 2>&1 | head -40; echo; diskutil list external 2>&1 | head -12; mount | grep -i "/Volumes" | head'
  hr "status"; mac status
}
case "${1:-}" in
  up) up "${2:-}" ;;
  attach) attach ;;
  status) echo "--- laptop"; lap status | sed -n 1,10p; echo "--- mac"; mac status ;;
  cli) side="$2"; shift 2; if [ "$side" = laptop ]; then lap "$@"; else mac "$@"; fi ;;
  logs) echo "--- laptop"; run_sudo cat "$STATE/daemon.log" | tail -40; echo "--- mac"; guest 'tail -40 $HOME/.periwire-test/daemon.log' ;;
  down)
    hr "stop daemons"; guest 'pkill -TERM -x periwire; for i in $(seq 1 30); do pgrep -x periwire >/dev/null || break; sleep 1; done; pgrep -x periwire >/dev/null && echo "mac daemon still running after 30 s" || echo mac-stopped'
    run_sudo sh -c 'pkill -TERM -x periwire; for i in $(seq 1 30); do pgrep -x periwire >/dev/null || break; sleep 1; done'; sleep 1; echo laptop-stopped; lsblk -o NAME,SIZE,TRAN | grep -E " usb|NAME" ;;
  *) sed -n 2,8p "$0" ;;
esac
