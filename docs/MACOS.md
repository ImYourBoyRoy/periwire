# macOS importer

`periwire attach <host:port> <busid>` on macOS (via the daemon) creates a **virtual USB host controller** with Apple's
`IOUSBHostControllerInterface` and plugs the remote device into it, so macOS sees an ordinary
locally connected device (same route VirtualHere's Mac client uses).

## How it works

```
exporter (any OS) ──mutual TLS──▶ urb.rs (USB/IP client) ──▶ mac.rs (virtual controller) ──▶ macOS USB stack
```

- `hci.rs` — portable: ring-message decoding, address pool, endpoint descriptors, errno mapping (unit tested everywhere).
- `urb.rs` — portable USB/IP client: sequence-numbered URBs, unlink, disconnect handling (tested against the real exporter).
- `mac.rs` — macOS only. One serial dispatch queue owns every Apple state machine. Commands (power, reset, device/endpoint
  create/destroy/pause/reset) are answered through `IOUSBHostCI*StateMachine`; doorbells make us walk the endpoint's transfer
  ring, completing Setup/Link messages locally and turning Data/Status messages into URBs. Results hop back onto the queue,
  and are dropped if the ring head moved (paused, reset), so a reclaimed buffer is never written.
- Control transfers: `Setup` is completed immediately, the `Data` stage carries the whole request, and `Status` carries the
  request when there was no data stage.
- The exporter honours vhci's port-reset pseudo-request (SET_FEATURE PORT_RESET) by resetting the real device.

## Permission requirements (measured on the build guest, SIP enabled)

| Attempt | Result |
|---|---|
| plain binary | refused: `Failed to create IOUSBHostControllerInterface (0xe00002cd)` |
| ad-hoc signed with the entitlement | killed on launch (exit 137, AMFI) |

`periwire mac-probe` reports whether the current process may create a controller. Ways to get permission:

1. **Production:** sign an app bundle with `com.apple.developer.usb.host-controller-interface` plus a provisioning profile that
   grants it (Apple must enable the capability for the team; VirtualHere's client is built this way).
2. **Development:** disable SIP and run as root, or disable SIP + `amfi_get_out_of_my_way=1` and ad-hoc sign with
   `crates/periwire/entitlements.plist`.

## Test plan (needs a SIP-relaxed guest — never the production `macOS` guest)

1. Create a copy-on-write overlay on the shut-off `macOS-q35-5.2-rollback` clone's disk (base stays untouched), plus copies of
   its OpenCore image and NVRAM; define a separate test domain. Production `macOS` must be shut down while it runs.
2. Disable SIP on the test guest only (OpenCore `csr-active-config`, or Recovery `csrutil disable`).
3. `tools/mac-build.sh release`, then `periwire mac-probe` as root → expect `OK`.
4. Forward a spare USB stick: laptop `serve` → guest `sudo periwire attach`; check `diskutil list`, mount read-only, compare hashes.
5. Forward the iPad (VirtualHere stopped): check Xcode/CoreDevice sees it.

## Known limitations

- Isochronous transfers (cameras, audio) are failed cleanly; macOS's controller API does not handle them well either.
- UAS drives are not downgraded to bulk-only yet (macOS does not fall back by itself); non-UAS drives and the iPad are unaffected.
- **Bulk read-ahead (new, not yet measured on the iPad).** Until now one URB was in flight per endpoint, so every transfer cost a
  network round trip (the iPad's USB network link moved about 0.5 MB/s that way). The importer now submits the transfers the driver
  has already queued on a bulk endpoint without waiting for the previous one (up to 8 and 8 MiB per endpoint; `PERIWIRE_PIPELINE=1`
  turns it off, a higher number deepens it, up to 32). Results still complete in ring order, and an error, pause or reset unlinks
  what was read ahead. After a failed transfer the data already read ahead for an IN endpoint is dropped, as with any unlink. The
  exporter runs each endpoint's transfers in the order they arrived, so queued transfers cannot overtake each other.
- Several devices at once work through the daemon (each attachment gets its own virtual controller): verified 2026-11-12 with the iPad and a read-only USB stick on one daemon, both healthy at the same time. The old one-device-per-`attach`-process limit applied only to the early standalone command.
- **iPad re-enumeration (found and fixed 2026-11-12):** an iPad can drop off the USB bus and come back under a new device number, for instance right after macOS's vendor handshake (`40 40 80 03 8c 01`) when it is still in its first mode. The exporter used to keep the stale handle and fail every later transfer with -19, so the iPad never configured on the Mac. Now a libusb `NoDevice` ends the session as a clean disconnect (the importer detaches the device from macOS) and an importer attached with `--auto` re-attaches the device that came back, about 2 s later. Always attach the iPad with `--auto` (`tools/mac-e2e.sh attach` does). Verified: `devicectl` `available (paired)`, `ddiServicesAvailable: true`, `tunnelState: connected` on iPadOS 27.0.1 with Xcode 26.6, and again after a software unplug/replug (`echo 0 > /sys/bus/usb/devices/1-2/authorized; sleep 2; echo 1 > …`) in the middle of a session.

## Test results (2026-11-08, throwaway SIP-relaxed guest, macOS 26.5.2)

Run on a copy-on-write overlay of the rollback guest's disk with a patched copy of OpenCore (`tools/mac-oc-sip.sh`:
`csr-active-config=FF0F0000` and the `amfi_get_out_of_my_way=1` boot-arg). Driven by `tools/mac-e2e.sh`.

- `periwire mac-probe` → `OK` (ad-hoc signed with the host-controller entitlement; refused before SIP/AMFI were relaxed).
- A USB stick shared from a Linux laptop appeared in macOS as an ordinary external USB disk
  (`STORE N GO` under `AppleUSBUserHCI`, `diskutil list external` showed its GPT, NTFS mounted read-only).
- The first 64 MiB read through the importer hashed identically to a direct read on the laptop.
- Raw read speed was about 8 MB/s over Wi-Fi (round-trip bound, same as the Linux importer).
- Safe eject returned the device to the laptop.

Known issue found: the very first attach after the exporter opens a device can time out string-descriptor reads
(`-110`) because the exporter resets the device on open and the importer starts talking immediately; a second attach
worked. The exporter should wait for the device to re-enumerate (or retry control reads) after its reset.
The iPad has not been tried yet (it was not plugged in).

## Test results (2026-11-09/10, macOS 26.5.2 and 26.7.3)

The importer was rebuilt and re-tested on the SIP-off guest before and after its update to macOS 26.7.3 (Xcode 26.6, Rust 1.99.0): `mac-probe` OK (ad-hoc
signature with `entitlements.plist` is needed after every rebuild: `codesign --force --sign - --entitlements crates/periwire/entitlements.plist target/release/periwire`),
read-only attach with `--auto` (`Media Read-Only: Yes`), 128 MiB of sample reads identical to a raw read on the sharing laptop, daemon killed and restarted (the
exporter held the drive and the importer resumed by itself), deliberate detach returning the drive at once. See TESTING.md for the details.
