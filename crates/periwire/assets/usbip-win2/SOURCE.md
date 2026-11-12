# usbip-win2 (vendored, unmodified)

These files are redistributed unmodified from the official release of
[vadimgrn/usbip-win2](https://github.com/vadimgrn/usbip-win2) v0.9.8.1 (`USBip-0.9.8.1-x64.exe`,
sha256 `38cad6d4432b52d5bb9409d9ad03b72fdffc4ada4cd3a48fbeca1a2752a8518a`), under the BSD 2-Clause license
(see `LICENSE.txt`). The `.sys`/`.cat` files carry the Microsoft Windows Hardware Compatibility signature; changing a
single byte breaks it, so never edit them. To update: download the new release, verify its published sha256, unpack the
files, replace these, run `sha256sum` into `SHA256SUMS.txt`, bump `VERSION`.

| File | Role |
|------|------|
| `usbip2_ude.{inf,sys,cat}` | kernel driver: the virtual USB host controller (USB Device Emulation) and USB/IP client |
| `usbip2_filter.{inf,sys,cat}` | companion upper-filter driver |
| `devnode.exe` | creates/removes the root-enumerated device `ROOT\USBIP_WIN2\UDE` |
| `usbip.exe`, `libusbip.dll`, `resources.dll` | command-line client that attaches/detaches remote devices |

The user-space tools need the Microsoft Visual C++ 2015-2022 runtime (`vcruntime140*.dll`, `msvcp140.dll`);
`periwire driver status` checks for it.
