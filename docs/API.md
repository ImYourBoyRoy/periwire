# Control API

The daemon exposes everything the CLI, MCP server and desktop app do through one local API. You can use it directly
from scripts or your own tools. `periwire api` prints the method list; the Rust client is the `periwire-client` crate.

## Transport

Newline-delimited JSON over a Unix domain socket.

| Who runs the daemon | Socket |
|---|---|
| root | `/run/periwire/periwire.sock` (`/var/run/periwire/periwire.sock` on macOS) |
| a normal user | `$XDG_RUNTIME_DIR/periwire.sock`, else `<config dir>/periwire/periwire.sock` |
| any | `PERIWIRE_SOCKET` or `--daemon-socket PATH` |

Access is controlled by file permissions (default mode 660). A root daemon started with `sudo`/`pkexec` gives the socket to that
user; their primary group is *not* given access (on some systems every user shares one), only a group named with `--socket-group` is. Anyone who can open the socket can control the daemon, including ejecting devices.
There is no network exposure of this API.

```
request : {"id": 1, "method": "status", "params": {}}
response: {"id": 1, "result": {...}}
error   : {"id": 1, "error": {"code": "not_found", "message": "no such attachment"}}
event   : {"event": "state", "time": 1700000000, "data": {}}      (only after `subscribe`)
```

Attachments and sessions carry an `info` object with the same device fields (`display`, `kind`, `kind_label`, …) so UIs can show "Studio YubiKey — Security key" instead of ids.

Error codes: `invalid` (bad/missing parameter), `not_found`, `busy` (e.g. open files on eject), `denied`,
`unsupported` (e.g. importing unavailable here), `failed`, plus client-side `not_running`.

Try it: `printf '{"id":1,"method":"ping"}\n' | socat - UNIX-CONNECT:/run/periwire/periwire.sock`

## Methods

| Method | Params | Result |
|---|---|---|
| `ping` | – | `{pong: true, version}` |
| `methods` | – | list of methods with parameter summaries |
| `identity` | – | `{fingerprint, hostname}` |
| `status` | – | snapshot (below) |
| `devices` | – | local devices: `busid, vid, pid, manufacturer, product, serial, key, label, kind, kind_label, display, speed, class, interfaces[], shared, in_use_by, mounted[], iso` (`iso` is null, or `{kind: video|audio|other, in_kbps, out_kbps}`: the most a camera or audio device can use in each direction, from its descriptors; `display` = label, else product; `kind` ∈ storage, android, android-fastboot, apple-ipad, apple-iphone, apple-dfu, apple-recovery, security-key, keyboard, mouse, hid, webcam, camera, audio, printer, bluetooth, serial, network, other) |
| `identity.reset` | `{confirm: true}` | `{fingerprint}` — new certificate; exporter restarts; peers must re-trust |
| `device.label` | `{key, label}` | `{key}` — name a device (empty clears); `key` is from `devices` |
| `share.add` / `share.remove` | `{spec, read_only?}` / `{spec}` | `{spec, read_only, disconnected}` / `{spec, disconnected}` (`read_only` write-protects USB storage for every importer; calling `share.add` again changes the mode and disconnects a current user; `status.shares[].read_only` and `devices[].shared_read_only` report it) |
| `remote.list` | `{addr}` | devices that computer shares: `busid, vid, pid, manufacturer, product, label, kind, kind_label, display, speed, class, interfaces[], iso` |
| `attach` | `{addr, busid, auto?, name?, read_only?, check_link?}` | the attachment object (`read_only: true` write-protects USB storage; default false = read and write; refused for other device kinds). For a camera or audio device a quick link check runs first (`check_link: false` skips it): its report is added as `link` and, when the link is weak for that device, the reasons as `warnings[]`. It never refuses the attach |
| `link.test` | `{addr, busid?, full?}` | measure the connection to a computer (2 s, or 6 s with `full`): `{addr, seconds, rtt: {samples, min_ms, avg_ms, p50_ms, p95_ms, max_ms, jitter_ms, stalls}, stream: {rate_kbps, interval_ms, expected, received, p50_ms, p95_ms, p99_ms, max_ms, delivered_kbps}, down_mbps?, up_mbps?, verdict: {level: good\|marginal\|poor, audio, video, headline, notes[]}}`; with `busid` the stream follows that device and the verdict is for it (`need` and `busid` are added) |
| `detach` | `{id}` or `{busid, addr?}`, `force?` | `{detached, id, unmounted[]}` |
| `kick` | `{session}` or `{busid}` | `{disconnected}` |
| `trust.list` | – | `[{fingerprint, name}]` |
| `trust.probe` | `{addr}` | `{addr, fingerprint, already_trusted}` — does **not** trust |
| `trust.add` | `{fingerprint, name?}` | `{fingerprint}` |
| `trust.remove` | `{fingerprint}` | `{removed}` |
| `connection.save` | `{addr, busid, auto?, name?}` | the connection |
| `connection.remove` | `{addr, busid}` | `{removed}` |
| `export.set` | `{enabled}` | `{enabled}` |
| `doctor` | – | `[{id, title, status: ok|warn|fail|info, detail, fix}]` |
| `driver.status` | – | `{supported, status?}` Windows virtual USB host controller driver state (`supported:false` on Linux/macOS) |
| `driver.install` | `confirm: true` | Installs the signed driver carried inside the program (Windows; the daemon must run as Administrator); idempotent; `{reboot, status}` |
| `driver.uninstall` | `confirm: true` | Removes the device, both packages and the extracted tools |
| `update.check` | – | `{current, latest, newer, url, published}`: asks GitHub whether a newer release exists (nothing is installed) |
| `update.set` | `enabled` | Turns the desktop app's automatic update check on or off (saved as `update_check`); `status` reports it |
| `logs` | `{lines?}` | `[string]` |
| `subscribe` | – | `{subscribed:true}`, then events on this connection |
| `shutdown` | – | `{stopping:true}` |

### `status` result

```json
{ "version": "0.1.0", "os": "linux", "arch": "x86_64", "hostname": "…", "pid": 1234, "daemon": true,
  "started": 1700000000, "uptime": 3600, "root": true, "fingerprint": "…64 hex…",
  "export":  {"enabled": true, "listen": "0.0.0.0:3241", "state": "listening|disabled|error", "error": null},
  "import":  {"available": true, "reason": null},
  "shares":  [{"spec": "1234:5678", "present": true}],
  "sessions": [{"id": 1, "peer": "…", "peer_name": "…", "busid": "2-2", "vid": "1234", "pid": "5678",
                "since": 1700000000, "urbs": 1200, "bytes_in": 0, "bytes_out": 0}],
  "attachments": [{"id": 1, "addr": "…:3241", "busid": "2-2", "auto": false,
                   "state": "connecting|attached|reconnecting|detaching", "error": null, "vid": "1234", "pid": "5678",
                   "speed": "super (5 Gb/s)", "port": 0, "local_busid": "3-1", "mounts": ["/mnt/x"],
                   "attached_at": 1700000000, "retries": 0, "bytes_in": 0, "bytes_out": 0}],
  "connections": [{"name": "", "addr": "…", "busid": "2-2", "auto": true}],
  "trusted": [{"fingerprint": "…", "name": "…"}] }
```

`bytes_in`/`bytes_out` on an attachment are bytes received from / sent to the sharing computer; on a session they are
bytes received from / sent to the peer.

## Events

After `subscribe`, the daemon pushes: `state` (something changed — re-read `status`), `session.opened`,
`session.closed`, `attachment.ended` (`{id, addr, busid, error}`), and `log` (`{line}`). Byte counters do not emit
events; poll `status` (the desktop app does every 2 s).
