#!/usr/bin/env bash
# Select USB configuration 1 for an unconfigured device (touches USB setup only, never data).
# Usage: tools/usb-configure.sh vid:pid
. "$(dirname "$0")/lib.sh"
id="${1:?usage: usb-configure.sh vid:pid}"
for d in /sys/bus/usb/devices/[0-9]*-[0-9]*; do
  case "$d" in *:*) continue;; esac
  [ -f "$d/idVendor" ] || continue
  [ "$(cat $d/idVendor):$(cat $d/idProduct)" = "$id" ] || continue
  echo "device $(basename $d): config before=[$(cat $d/bConfigurationValue)]"
  run_sudo sh -c "echo 1 > '$d/bConfigurationValue'" || echo "write failed"
  sleep 2
  echo "config after=[$(cat $d/bConfigurationValue)] ifaces=$(ls -d $d:* 2>/dev/null | wc -l)"
done
