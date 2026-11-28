# Changelog

All notable changes. The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions follow
[Semantic Versioning](https://semver.org/) (anything before 1.0 may change between releases).

## [0.1.6] — 2026-11-27

Webcams and live audio, and a way to tell whether your link is good enough for them.

### Added
- **Cameras and audio (isochronous transfers).** The sharing side now carries the live transfers a webcam, microphone or speaker uses (they used to fail, so such a device attached but did nothing). It keeps a camera's endpoint read ahead so a late request loses nothing. Verified with a webcam on Linux (raw and Motion-JPEG video, repeated starts, a 30-second run, detaching mid-stream);
  audio devices, transfers to a device, Windows and macOS were not tested, and a Mac cannot import them. `export.isochronous` (default on) and `PERIWIRE_ISO=0` turn it off. See [PLATFORMS](docs/PLATFORMS.md#cameras-and-audio-isochronous-devices) for what the link must be like: about 10 ms of round trip at most.
- **Connection check.** `periwire linktest HOST [BUSID] [--full]`, the `link.test` API method, the MCP tool `periwire_link_test` (22 tools now) and a *Check connection* button on Connect measure delay, jitter, a stream shaped like the device's traffic and (with `--full`) speed each way, and say in plain words whether the link is good enough, with the reasons.
  It is a protocol extension (`OP_REQ_PROBE`) between two PeriWire computers newer than 0.1.5. Camera and audio device cards show what they need (`iso` in the device list).
- **Attaching a camera or audio device checks the link first** and warns (CLI, app, MCP) when it is weak for that device; it never refuses. `PERIWIRE_LINKCHECK=0` skips it.
- `tools/iso-e2e.sh`: a loopback test on one computer with throw-away daemons.

### Fixed
- **Control transfers to a device reported zero bytes written.** A USB video driver checks that its probe/commit request was taken in full and refused the camera; other devices ignored it. The count is now the bytes the device took.
- **A device with a driver that binds several interfaces (a camera, a headset) did not get its driver back** after being shared: interfaces were released first to last, so the driver found its streaming interface still claimed. They are released last to first.

## [0.1.5] — 2026-11-21

The `usbmuxd` restore, now also when the daemon is stopped.

### Fixed
- Stopping the daemon while an Apple device was shared exited before `usbmuxd` was started again, so the iPad stayed missing from the desktop. The daemon now waits for its sessions to end and restores `usbmuxd` before it exits (checked with a real iPad: shared, then detached, then daemon stopped, then daemon killed and started again).

## [0.1.4] — 2026-11-21

Fixes the update check, makes the app run and control the daemon, and puts an iPad back on your computer after sharing.

### Fixed
- **Check for updates was blocked in the real window** (0.1.2 and 0.1.3 showed "the window may not call `update.check`"): the app shell's list of allowed calls missed it. A test now reads the page's code and fails if any call it makes is not allowed.
- **Sharing an iPhone or iPad no longer leaves it stranded on this computer.** On Linux a daemon with administrator rights steps `usbmuxd` aside when it claims an Apple device (it held the device, so sharing failed with "Resource busy") and starts it again when the last Apple device is released
  (session ended, unshared, daemon stopped), which makes it look at the bus again; a killed daemon puts it back at its next start. Before, nothing started it again and the device never reappeared in the file manager.

### Changed
- **The app runs the daemon for you.** Opening it starts the daemon with the app's own rights (unless you switched that off); the "daemon isn't running" popup no longer carries install instructions. The daemon's output goes to `daemon.log` in the config folder.
- **Settings page** (replaces About): *Daemon* with **Off / As me / Administrator** (the administrator choice uses the system's own prompt) and **Install service / Remove service**, then updates and the terminal command.
- **`periwire service`:** `install --for-user USER`, `uninstall` and a new `status` (`--json`) on Linux (systemd), macOS (launchd, untested) and Windows (scheduled task). The service's control socket belongs to the user it was installed for (`--socket-user`), replacing the shared `periwire` group and the
  "add yourself to a group and log in again" step; installing over an older unit restarts it.

## [0.1.3] — 2026-11-21

The About page and the in-app update, done properly.

### Changed
- **A real About page** replaces the cramped dialog: the version, an update status chip with one clear button (*Check for updates* / *Install x.y.z*), the automatic-check switch, how this copy updates, and the terminal command with *Set up* / *Undo*. The sidebar's version line shows
  "Up to date" or "Update x.y.z available" and the page has a badge.
- **Install update now finishes the job:** after `periwire self-update` replaced the files the app restarts into the new version (as does a daemon the app started), instead of leaving you on the old build. A package or the macOS disk image gets instructions and a download button.
  `periwire self-update --status` reports how a copy can be updated.

## [0.1.2] — 2026-11-21

Adds a version display, an update check and in-place updates, the `periwire` command in every shell, and one-line installers. Everyone on 0.1.1 can update with the app's *About* dialog or by downloading it.


### Added
- **Version and update check.** The desktop app shows its version at the bottom of the sidebar; clicking it opens *About* (app and daemon versions, a warning when
  they differ after an update, *Check now*, and a switch for the automatic check). A banner appears when a newer release exists and opens its release page.
  The same check is `periwire update` (CLI, `--json`), the API methods `update.check` / `update.set`, and the MCP tool `periwire_update_check` (21 tools now).
  It is one request to GitHub's latest-release endpoint, only reports and links (nothing is downloaded or installed), and `"update_check": false` in `config.json` turns the app's automatic check off.
- **The command line is found without installing it.** A copy of the app that did not come from a package (AppImage, tarball, a build in a folder) now also puts a
  `periwire` link in `~/.local/bin` the first time it starts (for an AppImage a copy of the program, since its mount point changes), unless a `periwire` is already
  on the PATH or `PERIWIRE_NO_LAUNCHER=1` is set; `periwire-gui --register-launcher` does it on demand. Before, only the `.deb` provided the `periwire` command.
- Commands that print (`status`, `doctor`, `logs` …) end quietly when their output is piped into something that closes early (`periwire status | head`), instead of panicking. The daemon and the MCP server are unaffected.
- **`periwire self-update`** (after enchi's): downloads the release archive for this system, refuses anything that does not match its SHA-256, and replaces `periwire` and `periwire-gui` beside it (Windows: the old
  files are moved aside; a failed replacement is rolled back). Packages and the macOS app bundle get instructions instead. `--to VERSION`, `--dry-run`, `--force`, and `PERIWIRE_RELEASE_BASE_URL` for mirrors.
  The app's update banner and About dialog now have *Install update*. Not an MCP tool on purpose.
- **`install.sh` / `install.ps1`** are attached to releases: one line downloads the release, checks its SHA-256, unpacks it and runs `periwire self-install`.
- **`periwire self-install` / `self-uninstall`** (after enchi's installer): make the `periwire` command work in every shell and undo it exactly. Windows: the program's folder goes into your user PATH
  (PowerShell and cmd). Linux/macOS: a link in `~/.local/bin` plus one delimited block in your bash, zsh and fish profiles (PATH and completions). It asks first, supports `--dry-run`, leaves packages and
  your own files alone, and records what it did in `install-receipt.json`. The app's *About* dialog has a button for it. (Replaces the app's earlier link-only helper, which now just runs `self-install --no-shell`.)
- The Windows installer now runs `periwire self-install` after installing (so `periwire` works in new PowerShell and cmd windows) and `self-uninstall` before removing the app. What `self-install`, `self-uninstall`
  and `self-update` do is also appended to `install.log` in the config folder.
- `periwire doctor` also notices an app launcher entry or a `~/.local/bin/periwire` link whose program has been moved or deleted (Linux).
- **`periwire doctor` explains two more ways to get stuck:** the `periwire` command not being on your PATH (with the fix for your OS), and, on Linux, `usbmuxd` masked or stopped while an iPhone/iPad is
  attached (the device stays in its basic USB mode and never shows up; unplugging does nothing).
- Every release download now has a `<file>.sha256` checksum file next to it (alongside `SHA256SUMS.txt`), and the Release workflow fails if a name that
  imyourboyroy.com matches on is missing or ambiguous. The naming contract is in [docs/RELEASING.md](docs/RELEASING.md).

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

[0.1.5]: https://github.com/ImYourBoyRoy/periwire/releases/tag/v0.1.5
[0.1.4]: https://github.com/ImYourBoyRoy/periwire/releases/tag/v0.1.4
[0.1.3]: https://github.com/ImYourBoyRoy/periwire/releases/tag/v0.1.3
[0.1.2]: https://github.com/ImYourBoyRoy/periwire/releases/tag/v0.1.2
[0.1.1]: https://github.com/ImYourBoyRoy/periwire/releases/tag/v0.1.1
[0.1.0]: https://github.com/ImYourBoyRoy/periwire/releases/tag/v0.1.0
