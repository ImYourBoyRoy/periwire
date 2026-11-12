# Architecture

```
 computer A (shares)                                   computer B (uses)
┌──────────────────────────────┐                      ┌───────────────────────────────┐
│ USB device                   │                      │  apps ── OS USB stack         │
│   ▲ libusb (built in)        │                      │            ▲                  │
│ periwire daemon                │   USB/IP in mutual   │  Linux: kernel vhci_hcd       │
│  • exporter (TCP 3241) ◀─────┼──── TLS 1.3 ─────────┼──▶ macOS: virtual controller  │
│  • control socket            │                      │  periwire daemon                │
└──────────────────────────────┘                      │  • attachments, auto-reconnect│
   ▲ CLI · MCP server · desktop app (control API)     └───────────────────────────────┘
```

## Crates

| Crate | Role |
|---|---|
| `usbip-proto` | USB/IP wire format: op headers, 312-byte device descriptors, 48-byte URB headers (`CMD_SUBMIT`, `RET_SUBMIT`, `CMD_UNLINK`, `RET_UNLINK`). Pure, unit tested. |
| `periwire` | The program: daemon, exporter, importer, CLI, MCP server. |
| `periwire-client` | Small client for the control API (shared by the CLI, MCP server and GUI). |
| `periwire-gui` (`gui/`) | Tauri desktop app. Excluded from the default workspace build. |

## Data path

**Exporter** (`export.rs`). A shared device is opened with libusb (vendored — no system library needed), reset to a known
state, and all its interfaces claimed (the OS driver is detached and re-attached on release). Each USB/IP `CMD_SUBMIT` is
executed with synchronous libusb transfers on blocking threads; control transfers that change device state
(`SET_CONFIGURATION`, `SET_INTERFACE`, `CLEAR_HALT`, the virtual port reset) are translated into the equivalent libusb
calls. `CMD_UNLINK` cancels an in-flight transfer. A session ends on disconnect, `kick`, unshare or shutdown, and the
device returns to the local driver.

**Device info (extension).** Plain USB/IP lists only numeric ids. A PeriWire exporter also answers a private request
(`OP_REQ_DEVINFO`, 0x8f01, inside the TLS channel; a second extension, `OP_REQ_FLAGS` 0x8f02, sent before the import request, carries session options such as read-only, the storage round-trip savings and the NTFS journal fix) with JSON for each shared device: product/manufacturer strings, your label, and
a recognisable kind. That is how another computer can show "iPad" or "Storage drive" before attaching. Serial numbers are not sent.
Plain USB/IP exporters still work; kinds are then inferred from the descriptors.

**Session options (`OP_REQ_FLAGS`).** `FLAG_READ_ONLY` (1) write-protects a storage device (`guard.rs`). `FLAG_ACCEL` (4) cuts a
storage command from three network round trips to about one: the importer completes the command-block write itself while the
exporter still performs it in order, and the exporter sends the status block along with the last data reply (`accel.rs`;
`PERIWIRE_ACCEL=0` turns it off). `FLAG_NTFS_JOURNAL` (8) lets a read-only NTFS drive whose journal was emptied show Windows a
valid clean journal header so Windows can mount it write-protected (`ntfs.rs`, patched into reads by `guard.rs`; see SECURITY).
`FLAG_RESUME` (16) is sent by a connection that reconnects by itself (`--auto`): it promises a proper TLS close when it detaches on
purpose, and asks the exporter to keep the drive claimed for a grace period (`PERIWIRE_RECONNECT_GRACE` seconds, default 60,
`0` = off) if the connection ends any other way. Without it, a lost connection hands the drive back to the sharing computer's desktop,
which mounts it within a second, and the exporter then refuses every reconnect (it will not share a mounted drive). The exporter
tells the two endings apart by whether the first read after the last message returns end-of-stream (a TLS `close_notify`: the importer
left on purpose, drive returned at once) or fails (reset, timeout, no `close_notify`: lost). A lost session's `Dev` is parked
(`resume.rs`, a small generic store with unit tests) and handed back only to the same certificate fingerprint asking for the same
read-only and NTFS options; anyone else is refused until the time runs out, then the drive goes back to the sharing computer.
If the same importer reconnects while the exporter has not noticed the old connection die (a silent outage), the old session is
ended as a takeover and its drive parked and resumed in one step. Before resuming, the exporter runs Bulk-Only reset recovery, because
the connection may have died in the middle of a command. All three importers (Linux, Windows, macOS) ask for it when the connection is `--auto`; the macOS importer closes the TLS connection only after a deliberate detach (`UrbClient::leave_cleanly`).
The exporter replies with the bits it accepts; an older exporter ignores bits it does not know, so mixed versions still work.

**Linux importer** (`attach.rs`, `import.rs::vhci`). After TLS and the USB/IP import handshake, the daemon connects a
loopback TCP socket pair, hands one end's file descriptor to `/sys/devices/platform/vhci_hcd.0/attach` and relays bytes
between the other end and the TLS stream. The kernel's USB/IP client then drives the device natively. The relay counts
bytes for live statistics.

**macOS importer** (`mac.rs`, `hci.rs`, `urb.rs`). macOS has no vhci, so the daemon implements a virtual USB host
controller with `IOUSBHostControllerInterface`: it decodes the controller's command and transfer rings, turns transfers
into USB/IP URBs (`urb.rs`) and feeds the results back. Requires an Apple entitlement; see [MACOS](MACOS.md).

## Daemon

`daemon.rs` owns the configuration (`config.json`), the share set, exporter sessions (`export::Shared`) and attachments
(`attach::Registry`). Each attachment is a task with a small state machine — connecting → attached → (reconnecting) →
detaching → ended — emitting events as it changes. `auto` attachments retry with exponential back-off up to 30 s.
Safe eject (`import::vhci::unmount`) syncs, then unmounts every filesystem on the attached device before cancelling
the relay; a busy mount aborts the eject unless forced.

`api.rs` is the single entry point (`dispatch`) for all front ends; `doctor.rs` implements the health checks;
`events.rs` is the log ring buffer and event bus; `backend.rs` lets the CLI answer read-only commands locally when no
daemon runs.

## Security

Mutual TLS 1.3 with fingerprint pinning (`tls.rs`, rustls with custom verifiers; no CA). See [SECURITY](SECURITY.md).

## Why not X?

- *Hypervisor USB passthrough* (libvirt/QEMU `usb-host`): needs the device on the hypervisor and was unreliable with a
  network-forwarded device. periwire runs inside the guest instead.
- *Kernel `usbip` tools*: Linux-only, unauthenticated and unencrypted, no safe-eject or management. periwire keeps the
  kernel's `vhci_hcd` for the Linux import path but replaces everything around it.
- *Per-device isochronous support* (webcams, audio) is not implemented yet.
