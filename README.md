<p align="center"><img src="gui/ui/logo.png" width="96" alt="periwire"></p>

# PeriWire

**Plug a USB device into one computer, use it on another.** periwire forwards USB devices over your network so a
flash drive, iPad, webcam, security key or dongle that is physically attached to *computer A* shows up on *computer B*
as if it were plugged in there — natively, with its own drivers.

- **One program, both roles.** The same daemon shares your local devices *and* uses other computers' devices.
- **Safe by default.** Computers pair by certificate fingerprint (mutual TLS, no passwords, no accounts, no cloud).
  Nothing is shared until you share it. Ejecting unmounts first; "force" is a deliberate choice.
- **Self-contained.** One binary per OS. libusb is built in; nothing else to install on the computer that shares.
- **Three equal front ends:** a command line, an [MCP server](docs/MCP.md) for AI assistants, and a desktop app with a
  tray icon. All three drive the same local [control API](docs/API.md).

> Status: early (0.1). Verified end to end on Linux, macOS (import works with the host-controller entitlement or SIP
> relaxed) and Windows Server 2025 (the signed driver is carried inside `periwire.exe` and installed with one click).
> See [platform status](docs/PLATFORMS.md) and [what is tested](docs/TESTING.md).

## Quick start (two computers, 3 minutes)

Install the `periwire` binary on both (see [Install](#install)). Below, **A** has the device, **B** wants to use it.

```bash
# on A — start the daemon, find the device, share it
periwire daemon &                    # or: sudo periwire service install
periwire devices                     # note the device's busid, e.g. 2-2
periwire share 2-2
periwire id                          # A's fingerprint — you'll compare it on B

# on B — start the daemon (as root on Linux: it needs to attach devices) and pair with A
sudo periwire daemon &
periwire trust host A.local          # shows A's fingerprint; compare with A's `periwire id`, answer y
periwire id                          # B's fingerprint…

# back on A — trust B (paste B's fingerprint)
periwire trust add <fingerprint-of-B> --name B

# on B — use it
periwire remote A.local              # what A shares
periwire attach A.local 2-2          # the device now appears on B
periwire status                      # live overview
periwire detach 2-2                  # safe eject: unmounts, then hands it back
```

Something not working? `periwire doctor` checks everything and tells you how to fix it.

Prefer clicking? Start the desktop app (`periwire-gui`), it does all of the above: switch devices on to share them,
browse another computer, attach with one click, and see live throughput. Try it risk-free in your browser with the
built-in demo mode: open `gui/ui/index.html` through any static file server.

## Install

Prebuilt binaries are not published yet; build from source (Rust 1.80+):

```bash
cargo build --release                      # -> target/release/periwire   (CLI, daemon, MCP server)
cargo build --release -p periwire-gui        # -> target/release/periwire-gui (desktop app; needs the system webview, see docs/GUI.md)
```

Run the daemon as a service so it starts at boot: `sudo periwire service install` (Linux/systemd), or
`periwire service print` for the launchd/systemd definition.

## What it can do

| | |
|---|---|
| Share / unshare devices | `periwire share`, `unshare`, per-device switches in the app |
| Use a remote device | `periwire attach`, with `--auto` to reconnect after drops and reboots |
| Read-only mode | `attach --read-only` or the switch in the app write-protects USB storage; `share --read-only` does it for everyone who imports it ([security](docs/SECURITY.md)) |
| Windows driver | `periwire driver install` (or the Install button): the signed virtual USB driver is built into the program |
| Safe eject | `periwire detach` flushes and unmounts first; refuses if files are open unless `--force` |
| Take a device back | `periwire kick` / "Disconnect" — abrupt, the other side loses it immediately |
| Pairing | fingerprint trust (`trust host`, `trust add`), no CA, no passwords |
| Health & diagnostics | `periwire doctor`, `status`, `logs -f`, `watch` |
| Automation | `--json` on every command, exit codes, [MCP server](docs/MCP.md), [control API](docs/API.md) |

## Documentation

| Guide | |
|---|---|
| [Getting started](docs/GETTING-STARTED.md) | Step-by-step for the common situations (same LAN, across VLANs/subnets, into a VM) |
| [CLI reference](docs/CLI.md) | Every command, option, output and exit code |
| [MCP server](docs/MCP.md) | Let an AI assistant manage devices; the tool list and safety rules |
| [Desktop app](docs/GUI.md) | Running, building, demo mode, tray behaviour |
| [Control API](docs/API.md) | The JSON protocol the CLI, MCP and GUI share |
| [Architecture](docs/ARCHITECTURE.md) | How it works: USB/IP, TLS, libusb, the daemon |
| [Security model](docs/SECURITY.md) | What is protected, what is trusted, what is not |
| [Platforms](docs/PLATFORMS.md) · [macOS](docs/MACOS.md) | What works where, and the macOS importer |
| [Testing status](docs/TESTING.md) | What is verified on real machines and the checklist for the next round |
| [Troubleshooting](docs/TROUBLESHOOTING.md) | Symptoms and fixes |
| [Development](docs/DEVELOPMENT.md) | Building, testing, the lab scripts |

---

## Advanced

**How a device travels.** The sharing side opens the device with libusb (so it works on any OS), and speaks the
[USB/IP](https://docs.kernel.org/usb/usbip_protocol.html) protocol over a mutually authenticated TLS 1.3 connection
(port 3241). The using side hands that stream to the OS's virtual USB host controller: on Linux the kernel's
`vhci_hcd`, on macOS a virtual controller created through `IOUSBHostControllerInterface`. The device enumerates with
its real descriptors and the normal driver binds — storage mounts, cameras stream, HID works. Isochronous endpoints
(webcam video, audio) are not supported yet; see [PLATFORMS](docs/PLATFORMS.md#limitations).

**Network paths.** It is plain TCP, so same-subnet, routed (cross-VLAN) and VM setups work wherever TCP port 3241 can
reach the sharing computer. For VMs you do not need hypervisor USB passthrough: run the daemon *inside* the guest and
attach over the virtual network. Internet use needs a port forward or a VPN/overlay (Tailscale, WireGuard); a relay is
on the roadmap.

**Performance.** Throughput is bounded by round trips: mass-storage commands cost three sequential exchanges, so a
5 ms Wi-Fi link yields roughly 7–8 MB/s for a USB stick, while loopback reaches >100 MB/s and wired Ethernet far more.
Latency matters more than bandwidth.

**Files and environment.** Identity, trust list and config live in `~/.config/periwire` (Linux), `~/Library/Application
Support/periwire` (macOS) or the directory in `PERIWIRE_HOME`. The daemon's control socket is `/run/periwire/periwire.sock`
(root), `$XDG_RUNTIME_DIR/periwire.sock` (user) or `PERIWIRE_SOCKET`. `PERIWIRE_TRACE=1` prints every URB.

**Configuration** (`config.json`): `export.enabled`, `export.listen`, `shares[]`, `connections[]` (the remembered
imports, `auto` flag). Edit through the CLI/app, not by hand while the daemon runs.

## License

MIT — see [LICENSE](LICENSE).

## Author

Made by [ImYourBoyRoy](https://github.com/ImYourBoyRoy). Source and issues: [github.com/ImYourBoyRoy/periwire](https://github.com/ImYourBoyRoy/periwire).
