#!/usr/bin/env bash
# Runs ON THE SERVER as root. Inspect or patch a COPY of the guest's OpenCore image so the test VM boots
# with SIP relaxed + AMFI entitlement checks off (needed to run an unsigned USB host-controller importer).
#   mac-oc-sip.sh inspect <oc.qcow2>     read-only: prints boot-args / csr-active-config
#   mac-oc-sip.sh apply   <oc.qcow2>     writes csr-active-config=FF0F0000 and adds amfi_get_out_of_my_way=1
# Never point this at a production image: it refuses paths outside a "periwire-mactest" directory.
set -euo pipefail
mode="$1"; img="$2"
case "$img" in *periwire-mactest*) ;; *) echo "refusing: $img is not under periwire-mactest" >&2; exit 2;; esac
modprobe nbd max_part=8
dev=/dev/nbd0; mnt=$(mktemp -d)
cleanup() { umount "$mnt" 2>/dev/null || true; qemu-nbd -d "$dev" >/dev/null 2>&1 || true; rmdir "$mnt" 2>/dev/null || true; }
trap cleanup EXIT
qemu-nbd -c "$dev" -f qcow2 "$img"; sleep 1; partprobe "$dev" 2>/dev/null || true; sleep 1
part="${dev}p1"; [ -b "$part" ] || { echo "no partition on image" >&2; exit 1; }
opts=ro; [ "$mode" = apply ] && opts=rw
mount -o "$opts" "$part" "$mnt"
python3 - "$mode" "$mnt/EFI/OC/config.plist" <<'PY'
import plistlib, sys
mode, path = sys.argv[1], sys.argv[2]
p = plistlib.load(open(path, "rb"))
G = "7C436110-AB2A-4BBB-A880-FE41995C9F82"
add = p.setdefault("NVRAM", {}).setdefault("Add", {}).setdefault(G, {})
print("boot-args:", add.get("boot-args"))
print("csr-active-config:", add.get("csr-active-config", b"").hex() or "(unset)")
print("SecureBootModel:", p.get("Misc", {}).get("Security", {}).get("SecureBootModel"), "| Vault:", p.get("Misc", {}).get("Security", {}).get("Vault"))
if mode == "apply":
    ba = add.get("boot-args", "")
    if "amfi_get_out_of_my_way=1" not in ba: ba = (ba + " amfi_get_out_of_my_way=1").strip()
    add["boot-args"] = ba; add["csr-active-config"] = bytes.fromhex("FF0F0000")
    # make sure OpenCore overwrites whatever the (copied) NVRAM already holds
    dl = p["NVRAM"].setdefault("Delete", {}).setdefault(G, [])
    for k in ("boot-args", "csr-active-config"):
        if k not in dl: dl.append(k)
    p["NVRAM"]["WriteFlash"] = True
    plistlib.dump(p, open(path, "wb"), sort_keys=False)
    print("patched ->", add["boot-args"], add["csr-active-config"].hex())
PY
sync
