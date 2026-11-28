#!/usr/bin/env bash
# Loopback test of isochronous forwarding and the connection check on THIS computer, leaving the installed periwire alone:
# two throw-away daemons run as root with their own homes, sockets and ports. A shares a device, B imports it from A over
# 127.0.0.1 (through the kernel's vhci_hcd, like a real second computer would).
#   tools/iso-e2e.sh up <busid>      start both daemons, pair them, share <busid> on A and attach it on B
#   tools/iso-e2e.sh link [busid]    run the connection check from B against A (judged for <busid> if given)
#   tools/iso-e2e.sh camera [secs]   record from the imported webcam and count frames (the camera's own consumers must be closed)
#   tools/iso-e2e.sh status          both daemons' view
#   tools/iso-e2e.sh logs            both daemon logs
#   tools/iso-e2e.sh down            detach, stop both daemons (the device goes back to this computer)
# PERIWIRE_BIN picks the binary (default target/debug/periwire); PERIWIRE_TRACE=1 makes A print every URB.
set -u
. "$(dirname "$0")/lib.sh"
BIN="${PERIWIRE_BIN:-$PROJECT_DIR/target/debug/periwire}"
ROOT="${PERIWIRE_ISO_DIR:-/tmp/periwire-iso}"
PA=3251
die() { echo "ABORT: $*" >&2; exit 1; }

# cli A|B ARGS... : the periwire CLI against one of the two daemons (as root, so it can read that daemon's identity).
cli() { local who="$1"; shift; run_sudo env PERIWIRE_HOME="$ROOT/$who" PERIWIRE_SOCKET="$ROOT/$who.sock" "$BIN" "$@"; }

start() {  # start A|B ARGS... : a daemon as root, remembering its pid
  local who="$1"; shift
  run_sudo sh -c "mkdir -p '$ROOT/$who'; PERIWIRE_HOME='$ROOT/$who' PERIWIRE_NO_TRAY=1 PERIWIRE_TRACE='${PERIWIRE_TRACE:-}' nohup '$BIN' daemon --no-tray --daemon-socket '$ROOT/$who.sock' $* >'$ROOT/$who.log' 2>&1 & echo \$! >'$ROOT/$who.pid'"
}

stop() {
  local who="$1" pid
  pid="$(run_sudo cat "$ROOT/$who.pid" 2>/dev/null)" || return 0
  [ -n "$pid" ] || return 0
  run_sudo kill -TERM "$pid" 2>/dev/null
  for _ in $(seq 1 40); do run_sudo kill -0 "$pid" 2>/dev/null || return 0; sleep 0.25; done
  echo "daemon $who (pid $pid) did not stop within 10 s; leaving it (never kill -9 a daemon that holds devices)" >&2
}

fp() { cli "$1" id --json | python3 -c 'import json,sys; print(json.load(sys.stdin)["fingerprint"])'; }

up() {
  local busid="${1:-}"
  [ -n "$busid" ] || die "say which device to share, as a busid (see lsusb -t): e.g. 1-3"
  [ -x "$BIN" ] || die "build first: cargo build"
  run_sudo modprobe vhci_hcd || die "cannot load vhci_hcd"
  hr "daemons"
  start a "--listen 127.0.0.1:$PA --share '$busid'"
  start b "--no-export"
  sleep 2
  cli a status >/dev/null || die "daemon A did not start; see $ROOT/a.log"
  cli b status >/dev/null || die "daemon B did not start; see $ROOT/b.log"
  hr "pairing"
  cli a trust add "$(fp b)" --name B >/dev/null
  cli b trust add "$(fp a)" --name A >/dev/null
  hr "attach $busid"
  cli b attach "127.0.0.1:$PA" "$busid"
  cli b status | head -20
}

link() { cli b linktest "127.0.0.1:$PA" ${1:-} ${FULL:+--full}; }

camera() {
  local secs="${1:-5}" dev
  # The imported camera is the newest video node whose name is the real camera's (the original is gone while A holds it).
  dev="$(for d in /sys/class/video4linux/video*; do echo "$(cat "$d/index" 2>/dev/null) $d"; done | sort | head -1 | cut -d' ' -f2)"
  dev="/dev/$(basename "$dev")"
  echo "recording $secs s from $dev"
  ffmpeg -hide_banner -loglevel info -f v4l2 -i "$dev" -t "$secs" -f null - 2>&1 | grep -E "Stream #|frame=|Input #|error|Error|Invalid|failed" | tail -8
}

case "${1:-}" in
  up) up "${2:-}" ;;
  link) link "${2:-}" ;;
  camera) camera "${2:-5}" ;;
  status) hr A; cli a status | head -15; hr B; cli b status | head -15 ;;
  logs) hr A; run_sudo tail -n 40 "$ROOT/a.log"; hr B; run_sudo tail -n 40 "$ROOT/b.log" ;;
  down)
    cli b detach "$(cli b status --json | python3 -c 'import json,sys; a=json.load(sys.stdin).get("attachments",[]); print(a[0]["id"] if a else "")')" 2>/dev/null
    stop b; stop a ;;
  *) sed -n 2,12p "$0" ;;
esac
