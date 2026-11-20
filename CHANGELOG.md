# Changelog

All notable changes. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions follow
[Semantic Versioning](https://semver.org/) (anything before 1.0 may change between releases).

## [Unreleased]

### Fixed
- The desktop app could say "The periwire daemon isn't running" while a daemon was running, and flicker between real and
  simulated data. The page missed the one-time "connected" announcement when the app started hidden next to the daemon, and the
  built-in demo simulator kept sending fake status updates outside demo mode. The page now treats a status update as proof of a
  daemon, and only demo mode runs the simulator.
- *Start daemon* in the app and tray no longer starts a second daemon when one already answers.

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

[0.1.0]: https://github.com/ImYourBoyRoy/periwire/releases/tag/v0.1.0
