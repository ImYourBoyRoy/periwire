# Platform status

| | Share (export) | Use (import) | Control socket / CLI / GUI | Service |
|---|---|---|---|---|
| **Linux** | ✅ tested | ✅ tested (needs root + `vhci-hcd`) | ✅ | systemd (`periwire service install`) |
| **macOS** | ⚙️ implemented via libusb, not yet tested | ⚙️ implemented, **needs Apple entitlement**, see [MACOS](MACOS.md) | ✅ | launchd plist (`service print`) |
| **Windows** | ⚙️ libusb builds; devices must use the WinUSB driver (Zadig); untested on real hardware | ⚙️ via the signed usbip-win2 driver that `periwire.exe` carries and installs itself (`periwire driver install`), relayed over TLS; driver install, attach and read-only mount verified on Windows Server 2025 (one-click from the GUI too) | ✅ daemon, CLI, named pipe and TLS pairing verified on Windows Server 2025; GUI untested | scheduled task (`service install`) |

✅ works and has been exercised end to end · ⚙️ code exists, not verified · ❌ not available.

## Tested combinations

Linux → Linux over Ethernet and Wi-Fi with a USB 3 flash drive (mount, read, safe eject, give back). macOS importer
unit-tested; end-to-end requires the entitlement.

## Limitations

- **Isochronous transfers** (webcam video, USB audio, some capture devices) are not supported: those URBs fail. Bulk,
  interrupt and control devices (storage, HID, security keys, phones/tablets via their data interfaces, serial adapters,
  printers) work.
- **One importer per device** at a time.
- **Throughput** is round-trip bound; mass storage on Wi-Fi gets roughly 7–8 MB/s, wired much more.
- **Internet**: needs a reachable TCP port or VPN; no relay yet.
- **Windows**: importing needs a kernel driver (Windows has no built-in virtual USB host controller). `periwire.exe` carries the unmodified, Microsoft-signed [usbip-win2](https://github.com/vadimgrn/usbip-win2) driver packages (BSD-2, see `THIRD-PARTY-NOTICES.md`) and installs them with `periwire driver install` (one Administrator prompt; `--elevate` asks for it). `periwire driver status | uninstall | extract DIR` manage it, and the GUI has an Install driver button. Verified 2026-11-08 on Windows Server 2025: install, attach over Tailscale, a read-only disk with byte-identical data, detach, uninstall, reinstall. Speed is round-trip bound (about 0.4 MB/s at 78 ms RTT, a few MB/s on a LAN). The driver's command-line tools need the Visual C++ 2015-2022 x64 runtime (most machines have it; `driver status` checks). Sharing from Windows only works for devices bound to WinUSB, because Windows' own storage/HID drivers keep libusb out.
