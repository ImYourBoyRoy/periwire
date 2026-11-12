#!/usr/bin/env bash
# End-to-end test with the lab Windows VM: this machine exports a USB device, the Windows VM lists / imports it.
# Both ends talk over Tailscale. Needs .venv (pywinrm), .env credentials, WINRUN_HOST + WINRUN_USER (see tools/winrun.py).
#   WINRUN_HOST=<vm tailscale ip> WINRUN_USER=Administrator tools/win-e2e.sh up <vid:pid>   daemons on both sides, paired (a spare device; or set PERIWIRE_TEST_DEVICE)
#   ... list      what the Windows VM sees of this machine's shared devices
#   ... attach <vid:pid>   import the first shared device on Windows (needs usbip-win2 installed there)
#   ... status    both sides
#   ... down      stop both daemons (the device returns to this machine)
set -u
. "$(dirname "$0")/lib.sh"
: "${WINRUN_HOST:?set WINRUN_HOST to the Windows VM Tailscale address}"
PY="$PROJECT_DIR/.venv/bin/python"; WR="$TOOLS_DIR/winrun.py"
BIN="$PROJECT_DIR/target/release/periwire"; STATE="$PROJECT_DIR/.state/laptop"; PORT=3241
EXE='C:\periwire\periwire.exe'
LAPTOP_IP="${PERIWIRE_LAPTOP_IP:-$(tailscale ip -4 | head -1)}"   # PERIWIRE_LAPTOP_IP: use the LAN address when at home
die() { echo "ABORT: $*" >&2; exit 1; }
lap() { "$BIN" "$@"; }
win() { "$PY" "$WR" run "& $EXE $* 2>&1 | Out-String" 2>&1 | grep -v '^#\|^$'; }
wps() { "$PY" "$WR" run "$1" 2>&1 | grep -v '^#\|^$'; }

unmount_spec() {   # the exporter refuses mounted devices; the desktop may re-mount after a daemon restart, so do this right before use
  local SPEC="$1" d name p
  for d in /sys/block/sd*; do
    [ -e "$d/device" ] || continue; name="$(basename "$d")"
    udevadm info -q property -n "/dev/$name" | grep -q "ID_VENDOR_ID=${SPEC%%:*}" || continue
    for p in $(lsblk -lno NAME "/dev/$name" | tail -n +2); do mountpoint -q "$(lsblk -no MOUNTPOINT "/dev/$p" | head -1)" 2>/dev/null && { echo "unmounting /dev/$p"; udisksctl unmount -b "/dev/$p" || die "could not unmount /dev/$p"; }; done
  done
}

up() {
  need_spec "${1:-}"
  [ -x "$BIN" ] || die "build first: cargo build --release"
  unmount_spec "$SPEC"
  hr "laptop daemon (root)"
  run_sudo mkdir -p "$STATE"
  run_sudo sh -c "pkill -TERM -x periwire; sleep 1; PERIWIRE_TRACE='${PERIWIRE_TRACE:-}' PERIWIRE_HOME='$STATE' nohup '$BIN' daemon --listen 0.0.0.0:$PORT --share '$SPEC' >'$STATE/daemon.log' 2>&1 &"
  sleep 1.5; lap status | head -8
  hr "windows daemon (detached)"
  "$PY" "$WR" file "$TOOLS_DIR/win-daemon.ps1" 2>&1 | grep -v '^#\|^$' | head -8
  hr "pair"
  win trust host "$LAPTOP_IP:$PORT" --yes
  lap trust add "$(win id | tr -d '\r\n ')" --name windows-test
}
list() { hr "windows sees"; win remote "$LAPTOP_IP:$PORT"; }
attach() {
  need_spec "${1:-}"; unmount_spec "$SPEC"
  BUSID="$(win --json remote "$LAPTOP_IP:$PORT" | sed -n 's/.*"busid": "\([^"]*\)".*/\1/p' | head -1 | tr -d '\r')"
  [ -n "$BUSID" ] || die "nothing shared"
  hr "attach $BUSID"; win attach "$LAPTOP_IP:$PORT" "$BUSID" || die "attach failed"
  sleep 6; hr "what Windows sees"
  wps 'Get-Disk | Where-Object BusType -eq "USB" | Format-Table Number,FriendlyName,Size,BusType -AutoSize | Out-String; Get-PnpDevice -Class USB -Status OK | Select-Object FriendlyName | Out-String'
  hr "status"; win status
}
case "${1:-}" in
  up) up "${2:-}" ;;
  list) list ;;
  attach) attach "${2:-}" ;;
  status) echo "--- laptop"; lap status | sed -n 1,10p; echo "--- windows"; win status ;;
  cli) side="$2"; shift 2; if [ "$side" = laptop ]; then lap "$@"; else win "$@"; fi ;;
  logs) echo "--- laptop"; run_sudo tail -30 "$STATE/daemon.log"; echo "--- windows"; wps 'Get-Content C:\periwire\daemon.log -Tail 30' ;;
  down) hr "stop daemons"; win stop; run_sudo pkill -TERM -x periwire; sleep 3; echo stopped; lsblk -o NAME,SIZE,TRAN | grep -E " usb|NAME" ;;
  *) sed -n 2,9p "$0" ;;
esac
