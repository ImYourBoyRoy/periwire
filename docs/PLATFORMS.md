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

- **Cameras and audio** (isochronous devices) work where listed under [Cameras and audio](#cameras-and-audio-isochronous-devices) below, and need a quick link. Bulk,
  interrupt and control devices (storage, HID, security keys, phones/tablets via their data interfaces, serial adapters,
  printers) work everywhere.
- **One importer per device** at a time.
- **Throughput** is round-trip bound; mass storage on Wi-Fi gets roughly 7–8 MB/s, wired much more.
- **Internet**: needs a reachable TCP port or VPN; no relay yet.
- **Windows**: importing needs a kernel driver (Windows has no built-in virtual USB host controller). `periwire.exe` carries the unmodified, Microsoft-signed [usbip-win2](https://github.com/vadimgrn/usbip-win2) driver packages (BSD-2, see `THIRD-PARTY-NOTICES.md`) and installs them with `periwire driver install` (one Administrator prompt; `--elevate` asks for it). `periwire driver status | uninstall | extract DIR` manage it, and the GUI has an Install driver button. Verified 2026-11-08 on Windows Server 2025: install, attach over Tailscale, a read-only disk with byte-identical data, detach, uninstall, reinstall. Speed is round-trip bound (about 0.4 MB/s at 78 ms RTT, a few MB/s on a LAN). The driver's command-line tools need the Visual C++ 2015-2022 x64 runtime (most machines have it; `driver status` checks). Sharing from Windows only works for devices bound to WinUSB, because Windows' own storage/HID drivers keep libusb out.

## Cameras and audio (isochronous devices)

Webcams, microphones, speakers and capture cards move their data as *isochronous* transfers: a fixed amount every millisecond or so, and what is late is lost. PeriWire carries them
(the sharing side queues the transfers on the device with libusb; `export.isochronous` in the config, default on, or `PERIWIRE_ISO=0` for one run, turns it off).

| | Status |
|---|---|
| Sharing from Linux | **verified** with a webcam (640x480 raw and 1280x720 Motion-JPEG at the camera's full frame rate, repeated starts and stops, a 30-second run, detaching in the middle of a stream; the camera's own driver returns afterwards) |
| Using on Linux (the kernel's `vhci_hcd`) | **verified**, as above |
| Microphones, speakers and other audio | **not verified**: no USB audio device was available to test with. Transfers *to* a device (playback) are written and unit-tested but have never run on hardware |
| Using on Windows | not verified (the driver and PeriWire's relay carry the same messages; nothing was run) |
| Using on macOS | **not supported**: the controller interface macOS provides is not used for these transfers, which are failed |
| Sharing from macOS or Windows | not verified (libusb's isochronous support on those systems has limits of its own) |

**The link decides.** The computer using the device keeps only a few transfers in flight (a webcam driver queues about five of 4 ms: 20 ms of video) and each replacement needs a full round trip
to be requested. The sharing side reads the camera ahead, so a request that arrives late finds its data waiting (up to a quarter of a second is kept) instead of losing it, but a link slower than the
queue can ask for cannot keep the stream flowing. Measured with a webcam over a shaped loopback link (the same stream, 640x480 raw video at 15 frames a second):

| Round trip | Result | `periwire linktest` says |
|---|---|---|
| under 1 ms (loopback) | all frames | good |
| about 5 ms | all frames | good |
| about 10 ms, 3 ms of jitter | all frames | marginal |
| about 30 ms | no frames | too weak |

Before the sharing side read ahead, the 10 ms case delivered 17 frames in 10 seconds instead of 150. Wired Ethernet and a good Wi-Fi link are well inside the limit; a VPN or a link across the internet is not.
`periwire linktest HOST BUSID` measures yours; attaching a camera or audio device runs a quick check first and warns when the link is weak (it never refuses; `PERIWIRE_LINKCHECK=0` skips it).
The thresholds for audio are estimates based on how audio drivers queue; they have not been checked against a real audio device.

**Tried it?** Audio and the other untested rows above are the most useful reports to send: open a [device report](https://github.com/ImYourBoyRoy/periwire/issues/new?template=device_report.yml) (works, partly, or not) and include the output of `periwire linktest HOST BUSID`.

Compression of the stream was measured and rejected: raw webcam frames compress only about 1.5x with fast compression, at 12 to 22 ms of one CPU core per frame, and bandwidth was never what failed; delay was. Many cameras send Motion-JPEG, which does not compress further.
