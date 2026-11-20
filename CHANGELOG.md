# Changelog

All notable changes. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions follow
[Semantic Versioning](https://semver.org/) (anything before 1.0 may change between releases).

## [0.1.1] — 2026-11-20

A bugfix release after a review of the whole code base and tests on Linux, Windows and macOS hardware. Everyone on 0.1.0 should update.

### Fixed
- The desktop app could say "The periwire daemon isn't running" while a daemon was running, and flicker between real and
  simulated data. The page missed the one-time "connected" announcement when the app started hidden next to the daemon, and the
  built-in demo simulator kept sending fake status updates outside demo mode. The page now treats a status update as proof of a
  daemon, and only demo mode runs the simulator. No false offline screen while the app is still connecting.
- *Start daemon* in the app and tray no longer starts a second daemon when one already answers.
- Sharing: every finished session waited 5 s and could never park the device for a reconnect (a handle on the device was kept open);
  removing a share or changing its read-only flag while someone used it locked up the call; a single failed `accept()` (for example no
  free file descriptors) stopped sharing for good; a slow flash drive pausing mid-transfer could return a short read.
- Safety limits: URB length, packet count, endpoint number and reply length are validated before anything is allocated for them (a hostile
  or corrupt peer could make the daemon allocate gigabytes). A trusted computer's name can no longer carry a line break and add a second
  fingerprint to the trust list; trust lines are parsed strictly and written atomically.
- Control socket: a root daemon started with `sudo`/`pkexec` gives the socket to that user only, not to their primary group (on systems where
  every user shares one, every local user could control the daemon). Only a group named with `--socket-group` gets in. Request lines are
  capped, and a path that is not a socket is never replaced.
- `--listen` and `--no-export` last for that run; they are no longer saved by an unrelated change such as sharing a device.
- Safe eject: handles mount points with spaces, encrypted (LUKS/LVM) drives and nested mounts, flushes just the drive instead of the whole
  system, and gives up with a clear message instead of hanging. Shutdown waits for imports with one shared deadline.
- App: a slow answer for an earlier computer can no longer replace the list for the one you chose (and attach the wrong device); a remembered
  read-only attachment stays read-only; two identical devices are shared one at a time (by port); the window may only call the daemon methods
  its own buttons use, and its content security policy is tighter.
- `logs` and `watch` exit with code 3 when no daemon runs (as documented); MCP confirmation refusals have their own error code and hint.
- Windows: the elevated daemon runs the USB/IP tools only from a folder that only Administrators can change (ownership and permissions are
  reset each time), only if they match the copies carried inside the program byte for byte, and no longer searches `PATH`. The attach
  command has a time limit and is not left running when cancelled.

### Changed
- Quitting the app now has an owner-aware meaning. A daemon the app started goes down with it (*Quit and stop the daemon*, with the
  number of devices in use it will disconnect); a daemon started anywhere else keeps running, and the tray offers *Quit and stop the
  daemon* next to *Quit the app*.
- Internals: `export.rs`, the CLI entry point and the app shell are split into modules; docs corrected against the code.

### Known
- An iPad on a USB 3 (10 Gbps) port dropped off the bus during 100-200 MB pushes in testing; on a USB 2 port the same push passed hash-identical.
  Use a USB 2 port or hub for the iPad ([testing notes](docs/TESTING.md)).

## [0.1.0] — 2026-11

First public release.

### Added
- **Share and use USB devices over the network.** One daemon does both: it shares local devices and attaches other computers'
  devices so they appear as if plugged in locally, with their own drivers. USB/IP inside mutually authenticated TLS 1.3.
- **Pairing by certificate fingerprint**: no passwords, accounts or cloud. Nothing is shared until you share it; read-only
  mode for USB storage; safe eject that flushes and unmounts first; `auto` reconnect after drops and reboots.
- **Three front ends over one local control API**: the `periwire` command line (`--json` everywhere), an MCP server
  (`periwire mcp`, 20 tools with built-in human-confirmation rules) and a desktop app with six themes and a demo mode.
- **Linux** import through `vhci_hcd`; **Windows** import through a signed virtual USB host controller driver carried inside
  `periwire.exe` and installed with one click; **macOS** import through `IOUSBHostControllerInterface` (needs Apple's
  host-controller entitlement or a Mac with SIP relaxed), tested against an iPad.
- **Taskbar icon whenever a daemon runs** (terminal, service, MCP) with the desktop app closing to it; the app registers
  itself in the application launcher on Linux, and installers add Start-menu and Applications entries on Windows and macOS.
- **macOS transfer speed**: several bulk transfers kept in flight per endpoint, and write-behind for Ethernet-over-USB
  (CDC-NCM) devices such as an iPad. Pushing 200 MB to an iPad takes about 40 seconds instead of more than 7 minutes.
- Downloads for Linux (`.deb`, AppImage, tar.gz), macOS (Apple silicon and Intel dmg, tar.gz) and Windows (installer, zip),
  with `SHA256SUMS.txt`, a CycloneDX dependency list and signed build attestations.

### Known limits
- Isochronous transfers (webcams, USB audio) are not supported yet.
- Copying from an iPad to a Mac runs at about 2.6 MB/s; only the push direction was sped up.
- macOS and Windows programs are not signed with a paid developer certificate (macOS: right-click > Open the first time).
- See [platform status](docs/PLATFORMS.md) and [what has been tested](docs/TESTING.md).

[0.1.1]: https://github.com/ImYourBoyRoy/periwire/releases/tag/v0.1.1
[0.1.0]: https://github.com/ImYourBoyRoy/periwire/releases/tag/v0.1.0
