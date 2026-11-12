#!/usr/bin/env bash
# Read-only USB diagnostics: why is a device not showing up as a disk / not forwardable?
# Usage: tools/usb-diag.sh [vid:pid]      (no args = all non-hub devices)
# Changes nothing and never touches device contents.
. "$(dirname "$0")/lib.sh"
FILTER="${1:-}"

hr "lsusb"
lsusb | grep -v "root hub" | { [ -n "$FILTER" ] && grep -i "$FILTER" || cat; }

hr "per-device state (config=empty means the kernel never configured it)"
for d in /sys/bus/usb/devices/[0-9]*-[0-9]*; do
  [ -f "$d/idVendor" ] || continue
  case "$d" in *:*) continue;; esac
  id="$(cat $d/idVendor):$(cat $d/idProduct)"
  [ -n "$FILTER" ] && [[ "$id" != *"$FILTER"* ]] && continue
  printf '%-8s %s  %s %s  speed=%sM config=[%s] authorized=%s ifaces=%s\n' \
    "$(basename $d)" "$id" "$(cat $d/manufacturer 2>/dev/null)" "$(cat $d/product 2>/dev/null)" \
    "$(cat $d/speed)" "$(cat $d/bConfigurationValue 2>/dev/null)" "$(cat $d/authorized)" \
    "$(ls -d $d:* 2>/dev/null | wc -l)"
done

hr "block devices (non-loop)"
lsblk -o NAME,SIZE,TRAN,MODEL,FSTYPE,LABEL,MOUNTPOINT | grep -v loop

hr "kernel log, USB-related only (journal; dmesg is flooded by audit noise)"
journalctl -k --no-pager --since "-30min" 2>/dev/null | grep -v -E "audit:|kauditd|exe=" | grep -i -E "usb|uas|scsi|xhci|sd[a-z]:" | tail -40 | cut -c1-200

hr "VirtualHere server (vhusbd)"
systemctl is-active vhusbd 2>&1
journalctl -u vhusbd --no-pager -n 8 2>&1 | cut -c1-200
