# CLI reference

`periwire [--json] [--socket PATH] <command>`

| Global option | |
|---|---|
| `--json` | Print the raw result as JSON (errors as `{"error":{"message":…}}`). Stable and meant for scripts and AI tools. |
| `--socket PATH` | Talk to the daemon on this control socket (default: auto-detected; same as env `PERIWIRE_SOCKET`). |

Commands talk to the running **daemon**. Read-only commands (`status`, `devices`, `remote`, `id`, `doctor`, `trust …`,
`share`/`unshare`) still work without one, answering from local state; commands that need the daemon (`attach`,
`detach`, `kick`, `export`, `stop`, `logs`, `watch`) exit with code 3 and say how to start it.

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
| `--listen ADDR` | Exporter address (default `0.0.0.0:3241`; saved default in `config.json`). |
| `--no-export` | Do not accept importers (a pure client). |
| `--share SPEC` | Share a device for this run only (repeatable). |
| `--daemon-socket PATH` | Where to create the control socket. |
| `--socket-group GROUP` | Group allowed to use the socket (root only). By default the user who ran `sudo`/`pkexec` owns it. |
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

### `periwire attach ADDR BUSID [--auto] [--name N] [--read-only]`
Import a remote device; returns when it is attached (≤25 s). Needs the daemon with import rights (root on Linux).
`--auto` remembers the connection: it is re-attached on daemon start and after network drops (exponential back-off to
30 s). `--name` labels it in `status`.

`--read-only` write-protects the device while it is attached (default: read and write). It works for USB storage (sticks,
drives, card readers): the computer the device is plugged into reports it as write-protected, so every OS mounts it read-only
by itself, and it also refuses every write, format or other non-read command, so nothing can change the drive even from raw
access. Other kinds of device cannot be write-protected this way, so they are refused in read-only mode instead of being left
writable. The mode is fixed when the device is imported; `status` shows `read-only`. Needs PeriWire 0.1+ on both computers.

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
| `NO_COLOR` | Disable colour. |
