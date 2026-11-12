#!/usr/bin/env bash
# Repeat attach / check / detach N times against the Windows lab VM and report the exporter's error statuses.
#   WINRUN_HOST=<vm ip> WINRUN_USER=Administrator PERIWIRE_TRACE=1 tools/win-soak.sh [N] <vid:pid> <busid>
#   (the busid is the device's port id on the exporting machine, from `periwire devices`; or set PERIWIRE_TEST_DEVICE / PERIWIRE_TEST_BUSID)
# Needs `tools/win-e2e.sh up` to have been run with PERIWIRE_TRACE=1 so the exporter log has URB detail.
set -u
. "$(dirname "$0")/lib.sh"
N="${1:-3}"; need_spec "${2:-}"; BUSID="${3:-${PERIWIRE_TEST_BUSID:-}}"; E="$TOOLS_DIR/win-e2e.sh"
[ -n "$BUSID" ] || { echo "ABORT: give the exporter-side busid as the third argument (or PERIWIRE_TEST_BUSID)" >&2; exit 1; }
LAPTOP_IP="${PERIWIRE_LAPTOP_IP:-$(tailscale ip -4 | head -1)}"; PY="$PROJECT_DIR/.venv/bin/python"; WR="$TOOLS_DIR/winrun.py"
for i in $(seq 1 "$N"); do
  hr "cycle $i/$N"
  vp="${SPEC%%:*}"
  for d in /sys/block/sd*; do [ -e "$d/device" ] || continue; n="$(basename "$d")"; udevadm info -q property -n "/dev/$n" | grep -q "ID_VENDOR_ID=$vp" || continue
    for p in $(lsblk -lno NAME "/dev/$n" | tail -n +2); do mountpoint -q "$(lsblk -no MOUNTPOINT "/dev/$p" | head -1)" 2>/dev/null && udisksctl unmount -b "/dev/$p" >/dev/null; done; done
  before="$(run_sudo grep -c 'status=-110\|status=-75' "$PROJECT_DIR/.state/laptop/daemon.log")"
  "$E" cli windows attach "$LAPTOP_IP:3241" "$BUSID" 2>&1 | tail -1
  sleep 30
  "$PY" "$WR" run 'Get-Disk | Where-Object BusType -eq "USB" | ForEach-Object { "disk $($_.Number) $($_.FriendlyName) $([math]::Round($_.Size/1GB,2)) GB $($_.OperationalStatus)" }' 2>&1 | grep -v '^#\|^$'
  after="$(run_sudo grep -c 'status=-110\|status=-75' "$PROJECT_DIR/.state/laptop/daemon.log")"
  echo "exporter timeouts/overflows during this cycle: $((after - before))"
  "$E" cli windows detach "$BUSID" 2>&1 | tail -1
  sleep 8
done
run_sudo sh -c "grep -n 'BOT reset recovery' '$PROJECT_DIR/.state/laptop/daemon.log' | tail -5"
