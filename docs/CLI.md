# CLI reference

`periwire [--json] [--socket PATH] <command>`

| Global option | |
|---|---|
| `--json` | Print the raw result as JSON (errors as `{"error":{"message":…}}`). Stable and meant for scripts and AI tools. |
| `--socket PATH` | Talk to the daemon on this control socket (default: auto-detected; same as env `PERIWIRE_SOCKET`). |

Commands talk to the running **daemon**. Some also work without one: `status`, `devices`, `remote`, `id` and `doctor` answer from
local state, and `trust …`, `share` and `unshare` read or edit the local trust list and `config.json` (a running daemon is told to
pick the change up). Commands that need the daemon (`attach`, `detach`, `kick`, `export`, `stop`, `logs`, `watch`) exit with code 3
and say how to start it.

## Exit codes

| Code | Meaning |
|---|---|
| 0 | Success |
| 1 | Error (message on stderr; with `--json`, on stdout) |
| 2 | Usage error (bad arguments) |
| 3 | Daemon not running / not reachable |
| 4 | `doctor` found at least one failing check |

## Identifying devices

- **busid** — a USB port on one computer, like `2-2` or `1-1.4`. Shown by `devices` and `remote`.
- **vid:pid** — a device model in hex, like `1234:5678`.
- **TARGET** (for `detach`/`kick`) — an attachment/session number from `status`, a busid, a vid:pid, or `busid@host`.
- **ADDR** — `host` or `host:port` (default port 3241); IPv6 as `[::1]:3241`.

## Commands

### `periwire daemon`
Run the daemon in the foreground (use a service for background). Starts the exporter (unless `--no-export`), the
control socket, and re-attaches remembered `--auto` connections.

| Option | |
|---|---|
| `--listen ADDR` | Exporter address for this run (default `0.0.0.0:3241`, from `config.json`; the option is not saved). |
| `--no-export` | Do not accept importers (a pure client). Lasts for this run; it is not saved. |
| `--no-tray` | Do not start the taskbar icon (also `PERIWIRE_NO_TRAY=1`). |
| `--share SPEC` | Share a device for this run only (repeatable). |
| `--daemon-socket PATH` | Where to create the control socket. |
| `--socket-user USER` | User allowed to use the socket (root only; a service has nobody who started it, so it is told). By default the user who ran `sudo`/`pkexec` owns it. |
| `--socket-group GROUP` | Group allowed to use the socket (root only), in addition. |
| `--socket-mode OCTAL` | Socket permissions, default `660`. |

Stops cleanly on SIGINT/SIGTERM or `periwire stop`: imports are ejected and shared devices go back to the host.
Never `kill -9` it: the kernel driver is re-attached to shared devices only on a clean exit.

### `periwire status`
Overview: daemon state, what you share (and who is using it), what you imported (state, bytes, mount points), remembered
connections and trusted computers. `--json` includes `devices` too.

### `periwire devices`
This computer's USB devices (hubs hidden) with name (your label if you set one), kind (Storage drive, Android device, iPad, Apple device in DFU mode, Security key, …), speed and state: `shared`, `in use by <peer>`,
`mounted locally`.

### `periwire share SPEC… [--read-only]` / `periwire unshare SPEC…`
Add/remove devices from the shared set (persisted). `unshare` disconnects anyone using the device. A device that is
mounted locally can be marked shared, but importers are refused until it is unmounted.
`--read-only` write-protects the device for everyone who imports it (USB storage only; same mechanism as `attach --read-only`). Running
`share` again on the same device with or without the flag switches the mode, and anyone using it is disconnected so they come back
under the new rule. `devices` shows `shared (read-only)`.

### `periwire remote ADDR`  (alias `list`)
List what another computer shares with you, with the names and kinds that computer reports (labels included). Needs mutual trust. Errors mention the fingerprint step when the TLS
handshake fails.

### `periwire linktest ADDR [BUSID] [--full]`  (alias `link`)
Measure the connection to another computer and say what it is good for. About 2 seconds; `--full` also measures throughput each way (about 6 seconds). It sends paced pings (round trip, jitter, pauses) and a stream of frames on a fixed schedule, then compares them with what
a camera or microphone needs. With `BUSID` (from `periwire remote ADDR`) the stream follows that device and the verdict is for it; without, it reports how good the link is for audio and for video in general. Prints the numbers, a verdict (good, marginal or poor) and the reasons; `--json` returns
the full report (see `link.test` in the [API](API.md)). Exit code 4 when the verdict is poor. The other computer must run a PeriWire newer than 0.1.5 (the check is a protocol extension).

### `periwire attach ADDR BUSID [--auto] [--name N] [--read-only]`
Import a remote device; returns when it is attached (≤25 s). Needs the daemon with import rights (root on Linux).
`--auto` remembers the connection: it is re-attached on daemon start and after network drops (exponential back-off to
30 s). `--name` labels it in `status`.

`--read-only` write-protects the device while it is attached (default: read and write). It works for USB storage (sticks,
drives, card readers): the computer the device is plugged into reports it as write-protected, so every OS mounts it read-only
by itself, and it also refuses every write, format or other non-read command, so nothing can change the drive even from raw
access. Other kinds of device cannot be write-protected this way, so they are refused in read-only mode instead of being left
writable. The mode is fixed when the device is imported; `status` shows `read-only`. Needs PeriWire 0.1+ on both computers.

Attaching a camera or audio device first runs a quick link check (`periwire linktest`, about 2 seconds) and prints a warning with the reasons when the link is weak for that device; it never refuses (`PERIWIRE_LINKCHECK=0` skips the check).

### `periwire detach TARGET [--force]`  (alias `eject`)
Safe eject: flush, unmount every filesystem on the device, detach it, hand it back. If a mount is busy the eject is
**refused** and nothing changes; `--force` unmounts busy mounts lazily (unsaved data may be lost). A manual detach
turns auto-attach off for that connection (it stays remembered; `forget` removes it).

### `periwire kick TARGET`
On the *sharing* side: disconnect a peer using one of your devices. Abrupt; the device returns to this computer at once.

### `periwire forget ADDR BUSID`
Remove a remembered connection.

### `periwire trust list | add FP [--name N] | remove FP | host ADDR [--yes]`
Manage trusted computers. `trust host` connects, shows the fingerprint it presents and asks you to confirm after
comparing it with `periwire id` on that computer; `--yes` skips the prompt (only when you verified out of band; required
when stdin is not a terminal).

### `periwire export on|off|status`
Start or stop the exporter at runtime (persisted). `off` releases every shared device to this computer.

### `periwire label DEVICE "Name" | --clear`
Give a local device a name you recognise ("Sam's iPad", "Photo stick"). DEVICE is a busid or vid:pid from `devices`. The
name is stored with the device (model + serial number when it has one), shows in every UI, and computers that use the
device see it too. `--clear` removes it.

### `periwire id`
Print this computer's fingerprint (64 hex chars). Script-friendly: nothing else on stdout.

### `periwire identity reset --yes`
Create a new certificate and fingerprint. Every computer that trusted this one will refuse it until you give them the
new fingerprint (`trust add`); the exporter restarts and current sessions drop. Without `--yes` it only explains.

### `periwire doctor`
Health checks with fixes: identity, USB access, permissions, importer support, daemon, exporter, trusted peers, each
shared device (including "mounted locally"), and competing software such as VirtualHere. Exit code 4 on failures.

### `periwire service install [--for-user USER]` · `service uninstall` · `service status`
Runs the daemon as a system service: a systemd unit on Linux, a launchd daemon on macOS, a scheduled task (at logon, highest privileges) on Windows. `install` and `uninstall` need administrator rights
(`sudo`, or the desktop app asks the system for them); `status` does not (`--json`: `{kind, installed, enabled, active, supported}`). The service runs with administrator rights, so it can import devices, and **only `--for-user`
(by default whoever ran sudo or pkexec) may control it**: the control socket belongs to that user, no group or new login is needed. It keeps its own identity (`/var/lib/periwire` on Linux), so other computers trust it once.
Installing over an older unit restarts it so the new settings apply.

### `periwire self-install [-y] [--dry-run] [--no-shell]` · `periwire self-uninstall [-y]`
Makes `periwire` work in every shell, and undoes it exactly. Nothing but a package puts the command on your PATH, so a copy of the app that was not installed
(an AppImage, a tarball, a build folder, a Windows or macOS app) does not have it until you run this.
- **Windows:** the folder that holds `periwire.exe` goes into your *user* PATH (registry; PowerShell and cmd both read it; the value keeps its type). No profile files.
- **Linux and macOS:** `~/.local/bin/periwire` links to the program (to a copy in `~/.local/share/periwire/bin` for an AppImage, whose mount point changes) and one
  delimited block (`# >>> periwire >>>` … `# <<< periwire <<<`) in your bash, zsh and fish profiles puts that folder on PATH and loads completions. Only profiles that
  exist (and your login shell's) are touched; `--no-shell` skips them. fish gets its own file, `conf.d/periwire.fish`.
- It asks first (or `--yes`), `--dry-run` only lists the steps, a copy installed by a package or another `periwire` on the PATH is left alone, and a file of yours at
  `~/.local/bin/periwire` is never replaced. What was done is recorded in `install-receipt.json` in the config folder; `self-uninstall` removes exactly that (the program,
  your identity, trust list and settings stay). The desktop app's *About* dialog has a button for it, and the app itself only links the command on Linux (no profile edits).

### `periwire self-update [-y] [--dry-run] [--to VERSION] [--force]`
Updates this copy of PeriWire to the latest release (after enchi's `self-update`). It downloads the release archive for this system
(`periwire-<version>-<os>-<arch>.tar.gz`, or `.zip` on Windows; the AppImage for an AppImage), checks it against the release's `<file>.sha256` (older releases: `SHA256SUMS.txt`) and refuses
to install anything that does not match, then replaces `periwire` and `periwire-gui` beside this program (on Windows the old files are moved aside and deleted at the next run; if a replacement fails the
earlier ones are put back). Restart the daemon and the app afterwards.
- A **package** (the `.deb`) or the **macOS app bundle** is not changed here: it prints how to update (package manager, or the new `.dmg`).
- `--to VERSION` installs that version instead (also to go back); `--force` reinstalls the current one.
- The checksum comes from the same release over HTTPS, so it catches a damaged or swapped download but is not a signature: `gh attestation verify <file> --repo ImYourBoyRoy/periwire` is the stronger check.
- `PERIWIRE_RELEASE_BASE_URL` points at a folder holding the archive and its `.sha256` (https, `file://`, or http on 127.0.0.1) for mirrors and tests. The app's *Install update* button runs this command.
- It is deliberately **not** an MCP tool: an assistant can tell you an update exists (`periwire_update_check`), installing software stays your decision.

### `periwire update`
Asks GitHub (one HTTPS request, nothing sent but a `periwire/<version>` User-Agent) whether a newer release exists and prints the version, the release link and
how to install it. It installs nothing and works without a daemon. `--json` prints `{current, latest, newer, url, published}`. The desktop app shows the same
result; `"update_check": false` in `config.json` (or the switch in the app's About dialog) stops the app's automatic check.

### `periwire logs [-n N] [-f]` · `periwire watch`
`logs` prints the last N daemon log lines (`-f` follows). `watch` streams events (use `--json` for one JSON object per
line).

### `periwire stop`
Ask the daemon to stop gracefully.

### `periwire mcp`
Run the [MCP server](MCP.md) on stdio.

### `periwire api [METHOD [JSON]]`
With no argument, lists the control-API methods ([API](API.md)). With a method name it calls that method on the running daemon and prints the JSON reply, e.g. `periwire api status`, `periwire api driver.status`, `periwire api share.add '{"spec":"2-2"}'` (in PowerShell 5 escape the quotes: `'{\"spec\":\"2-2\"}'`).

### `periwire service print [--user] | install | uninstall`
`print` shows the systemd unit (or launchd plist on macOS). `install`/`uninstall` (Linux, root) manage
`periwire.service` and create the `periwire` group; add your user to it to control the daemon without sudo.

### `periwire driver status | install [--elevate] | uninstall [--elevate] | extract DIR`
Windows only (other systems print a note). Importing on Windows needs a signed virtual USB host-controller driver; PeriWire
carries the unmodified usbip-win2 packages inside the executable. `install` unpacks them to `%ProgramData%\PeriWire\usbip-win2`,
runs `pnputil /add-driver` for both packages and creates the device `ROOT\USBIP_WIN2\UDE` (needs an elevated shell, or
`--elevate` for a UAC prompt). `status` reports packages, device, tools, the C++ runtime and whether importing is ready
(exit 1 when not). `uninstall` removes the device, both packages and the extracted tools. `extract DIR` only writes the
files, for manual or enterprise deployment. `doctor` reports the same state.

### `periwire completions SHELL`
`bash`, `zsh`, `fish`, `powershell` or `elvish`; e.g. `periwire completions bash | sudo tee /etc/bash_completion.d/periwire`.

## Scripting examples

```bash
periwire --json status | jq '.attachments[] | {busid, state, bytes_in}'
periwire attach media-pc 3-2 --auto --json || echo "attach failed: $?"
periwire doctor --json >/dev/null; [ $? -eq 4 ] && notify "periwire unhealthy"
```

## Environment

| Variable | |
|---|---|
| `PERIWIRE_HOME` | Directory for identity, trust list and config. |
| `PERIWIRE_SOCKET` | Control socket path (CLI, MCP, GUI and daemon). |
| `PERIWIRE_TRACE` | Set to print every USB request in the daemon log (very verbose). |
| `PERIWIRE_ISO` | `0` refuses camera and audio (isochronous) transfers for this run of the daemon; the config's `export.isochronous` is the permanent setting. |
| `PERIWIRE_LINKCHECK` | `0` skips the quick link check made before attaching a camera or audio device. |
| `NO_COLOR` | Disable colour. |
