# Testing

What is tested automatically, how to run the hardware tests on your own machines, what has been verified on real hardware, and what has not.
The hardware tools in `tools/` take their settings from arguments and the git-ignored `tools/lab.env` (copy `tools/lab.env.example`); nothing
about a particular lab is built into them. Detailed run logs of a particular lab belong in `private/` (git-ignored), not here.

## Automated tests (no hardware)

`cargo test --release` runs the unit tests of every crate in seconds (guard, accelerator, NTFS journal substitution, reconnect store, protocol,
help-text accuracy against the real CLI). A run that takes minutes means a test is hung: run test binaries with `timeout`.
`tools/build-windows.sh` cross-builds the Windows executable; `tools/mac-build.sh test` builds and tests on a Mac over ssh.

## Hardware tests (your own machines)

Pick a **spare** device and say which with `vid:pid` (`periwire devices` or `lsusb` lists them). The scripts have no default device on purpose,
and never write to it: use read-only attaches, and `sg_raw` with a zero-length WRITE(10) as the safe write probe.
Either pass the device as the argument or set `PERIWIRE_TEST_DEVICE=vid:pid`.

| Test | Command |
|------|---------|
| Linux export and import, read-only mount, safe eject | `tools/e2e.sh up <vid:pid>`, then `attach`, `eject`, `down` |
| Read-only importer | `tools/e2e.sh cli server attach <exporter>:3241 <busid> --read-only`, then `blockdev --getro /dev/sdX` prints 1 |
| Owner-side read-only | `periwire share <vid:pid> --read-only`, attach from another machine without a flag: read-only there |
| Windows importer | `tools/win-e2e.sh up <vid:pid>`, `attach <vid:pid>`, `tools/win-soak.sh <cycles> <vid:pid>` |
| macOS importer | on a Mac with SIP relaxed (never a machine you care about): `tools/mac-build.sh release`, sign with `crates/periwire/entitlements.plist`, `tools/mac-e2e.sh up <vid:pid>` and `attach` |
| iPhone/iPad into a Mac | as above with the device's `vid:pid`; keep the device **unlocked**; healthy means `xcrun devicectl list devices` says `available (paired)` and the details show `tunnelState: connected` and `ddiServicesAvailable: true` |
| Throughput | `tools/loopback-bench.sh <vid:pid> <busid>` (loopback), or time a large read over your real link |

Away from the LAN, set `PERIWIRE_REMOTE_HOST` to the lab server's address; for the Windows VM set `WINRUN_HOST` and `WINRUN_USER`. Trace every URB with
`PERIWIRE_TRACE=1` (the exporter also prints each SCSI command). A Windows trace needs a `trace.flag` file next to the executable (`tools/win-daemon.ps1`).
The desktop on the exporting machine auto-mounts a returned drive and the exporter refuses a mounted one, so the tools unmount just before attaching.

## Verified on real hardware

| Area | Result |
|------|--------|
| Linux importer and exporter | share, attach, read-only and read/write mounts, safe eject, the device returns to its owner; names, kinds and labels reach the importer |
| Trust and pairing | fingerprint pairing and identity reset on Linux, Windows and macOS |
| Read-only mode | the exporter's guard: the kernel marks the disk read-only, write-class commands return "write protected", reads work, unknown vendor commands are refused; the default stays writable; owner-side read-only works |
| Windows importer | embedded driver install/uninstall, disk appears, NTFS volume mounts (also read-only), hashes identical to a direct read, repeated attach/detach cycles |
| macOS importer | storage appears as a native external disk (read-only too), hashes identical; the iPad appears as a native device, configures, and `devicectl` reports it available with the tunnel connected on iPadOS 27.0.1 with Xcode 26.6; an iPad and a USB drive work at the same time on one daemon |
| Round-trip accelerator (`FLAG_ACCEL`) | storage reads about 1.75-1.9x faster over Wi-Fi at about 15 ms ping (Linux 6.49 vs 3.39 MB/s, Windows 2.74 vs 1.57 MB/s, accelerator on vs `PERIWIRE_ACCEL=0`, cold cache), data bit-identical to a direct read for sequential and for 600 random reads of 512 B to 1 MiB, on Linux and Windows (macOS reads also match) |
| NTFS journal substitution (`FLAG_NTFS_JOURNAL`) | an NTFS drive whose journal was emptied (the Linux `ntfs3` driver does that on every read-write mount) mounts on Windows read-only, `chkdsk` reports no problems, a write is refused, and the drive is byte-for-byte unchanged afterwards (checked before and after) |
| Reconnect grace period (`FLAG_RESUME`) | after a lost connection the exporter holds the drive for 60 s and only the same importer with the same options gets it back ("resumed ... came back after 0 s"); a deliberate detach releases at once; an importer that never returns is released at expiry; verified with Linux, Windows and macOS importers, including killing the importer, rebooting the Mac guest while attached, and a physical cable replug |
| Unplug and re-enumeration | a device that vanishes ends the session cleanly and an `--auto` importer re-attaches the device that comes back in about 2-3 s (software unplug, port disable and a real cable replug; the iPad also re-enumerates by itself) |
| GUI (browser/demo mode) | six themes, responsive layout, driver banner, read-only switches, help drawer, first-run guide |
| MCP | compact tool list and replies; safety gates unchanged |
| iPad throughput through the macOS importer | NCM write-behind and read-ahead (`mac.rs`): 200 MB to the iPad in 39 s over a 7 ms link (was 7+ min), hash-identical after a round trip; read-ahead alone about 1.25x; iPad to Mac 2.6 MB/s (unchanged) |
| In-app help | the `help_matches_cli` test checks every command and flag in `gui/ui/help.js` against the real CLI |

## Not verified yet

- An actual Xcode build and run to the iPad through the importer (signing identities do not belong in a throwaway guest), `xctrace record`
  against iPadOS 27 (Instruments in Xcode 26.6 does not support it; `devicectl` is fine), iPad sleep and wake.
- The GUI "Read-only for others" switch against a real importer, and the GUI inside the real Tauri window (including the one-click driver install on Windows).
- Isochronous devices (cameras, audio), UAS drives on macOS, internet relay, a macOS exporter.
- Write-behind on other Ethernet-over-USB devices (only the iPad's NCM link was measured), and prefetching for the iPad-to-Mac direction.
- The accelerator on writes and on a physical unplug mid-transfer (unit tests cover writes and failures; no hardware run), and a macOS speed measurement.
- Production macOS use with SIP on: the importer needs Apple's host-controller entitlement (or SIP relaxed) until that is granted.
- Performance over a wired LAN or Wi-Fi 6.

## Lessons from real hardware

- **`devicectl device copy to ... --remove-existing-content true` empties the app's whole data container** (Documents, Library, tmp), not just the destination file. Never use it when timing transfers into an installed app; each such run reset the app's saves and preferences. Copy to a fresh path without the flag.
- **Apple devices re-enumerate on a USB reset**, which invalidates the exporter's handle, and some also drop off the bus by themselves. The exporter resets only USB
  *mass storage*, treats libusb `NoDevice` as a clean disconnect, and the importer (`--auto`, always use it for iPhones and iPads) attaches the device that came back.
- **An iPhone or iPad must be unlocked.** A locked one enumerates but refuses the configuration change macOS asks for (`SET_CONFIGURATION` fails with a protocol
  error, over and over) and stays "unavailable". The exporter logs a hint once.
- **Cheap flash controllers are fragile.** After another USB-sharing server had held a stick, it answered nothing once its kernel driver was detached, and a later reset
  dropped it into its controller's ROM fallback mode (0 bytes) until it was replugged. Nothing was ever written. The exporter leaves a stick that answers alone and resets
  only one that is silent. Stop any other USB-sharing server before testing; some leave sticks unconfigured and unresponsive.
- **The same model of stick can be silent on one port and fine on another.** If a stick goes silent right after being claimed, even in a bare libusb test, try another port before suspecting the software.
- **Desktops auto-mount a released drive** and a file manager window can keep the mount busy. PeriWire refuses to share a drive that is mounted locally ("still mounted locally ... unmount it first"), which is correct.
- **Windows asks for the status block in a 512-byte read** where Linux asks for exactly 13 bytes. Any read of at least 13 bytes after a complete data stage is the status stage and is answered with the 13 bytes the device would send.
- **Windows queues the status read right behind the command block** (and behind a clear-halt): never assume the host waits for a reply before its next transfer. The exporter also answers a repeated status read from the block it already sent.
- **Control transfers must run in order with the storage transfers.** After a short data stage a stick may STALL the status read; Windows then sends CLEAR_FEATURE(HALT) and the retry about 40 microseconds apart. On a separate thread the retry could overtake the clear and stall again, ending in a 20 s timeout.
- **A plain packet drop on an idle connection does not break TCP**, so it is not a useful network-loss test; make the importer's own connection die with a read in flight, or kill the importer.
- **Compare data against a fresh reference.** A desktop that mounts the drive read-write after every return changes its contents (access times and the like): take the reference hash right before the comparison and do not read files through the desktop's mount in between.
- **A hang in a background test is invisible if output is only printed at the end.**
- **VM disks on a slow SSD:** a macOS guest on emulated SATA froze when the host SSD stalled QEMU's main loop during a large guest write burst; `cache=none,aio=native` on the guest disk (or an NVMe-emulated disk) fixed it. This affects how you host a Mac test guest, not PeriWire itself.
