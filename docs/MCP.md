# MCP server

`periwire mcp` runs a [Model Context Protocol](https://modelcontextprotocol.io) server on stdio, so an AI assistant can
inspect and manage USB forwarding with the same capabilities as the CLI. It talks to the local daemon; start the daemon
first (read-only tools also work without one).

## Connect an assistant

Claude Code:
```bash
claude mcp add periwire -- periwire mcp
```
Any MCP client (Claude Desktop, Cursor, …): add a stdio server whose command is `periwire` with argument `mcp`:
```json
{ "mcpServers": { "periwire": { "command": "periwire", "args": ["mcp"] } } }
```
The client must run as a user that may use the daemon's control socket (see [CLI](CLI.md#periwire-daemon) `--socket-group`).

## Tools

All tools return compact one-line JSON as text (one copy, no `structuredContent` duplicate). Errors set `isError` and include a fix hint.

**Token cost.** The tool list is about 1,600 tokens and the server instructions about 330. Replies are projected down to what a model
needs to act: `periwire_status` is roughly a third of the raw snapshot (empty and false fields are left out; `ro`, `auto`, `shared`
and `mounts` appear only when true), `periwire_doctor` returns only the problems (`verbose=true` adds the passing checks), and
`periwire_list_devices` returns `busid, id, name, kind` plus flags. Pass `detail=true` on `periwire_status` or `periwire_list_devices`
for the raw objects (that is where a device's `key` for `periwire_label_device` comes from).
Annotations tell clients which are read-only (no confirmation needed) and which are destructive.

| Tool | Does | Read-only | Destructive |
|---|---|---|---|
| `periwire_status` `{detail?}` | Daemon, shares, sessions, imports, remembered connections, trust, local devices (compact; `detail` = raw) | yes | |
| `periwire_doctor` `{verbose?}` | Problems with fixes (`verbose` adds passing checks) | yes | |
| `periwire_list_devices` `{detail?}` | Local USB devices, who uses them, local mounts (compact; `detail` = raw with keys) | yes | |
| `periwire_list_remote` `{addr}` | What another computer shares with us | yes | |
| `periwire_share` `{spec, read_only?}` | Share a local device; `read_only` write-protects USB storage for every importer | | |
| `periwire_unshare` `{spec}` | Stop sharing; disconnects a current user | | yes |
| `periwire_attach` `{addr, busid, auto?, name?, read_only?}` | Import a remote device; `read_only` write-protects USB storage | | |
| `periwire_eject` `{target, force?}` | Safe eject (unmount, detach) | | yes |
| `periwire_disconnect_peer` `{target}` | Force a peer off a device we share | | yes |
| `periwire_trust_list` | Trusted computers | yes | |
| `periwire_trust_probe` `{addr}` | Read the fingerprint a host presents (does **not** trust it) | yes | |
| `periwire_trust_add` `{fingerprint, name?, confirmed_by_user}` | Trust a computer | | |
| `periwire_trust_remove` `{fingerprint}` | Stop trusting | | yes |
| `periwire_forget` `{addr, busid}` | Forget a remembered connection | | yes |
| `periwire_label_device` `{key, label}` | Name a local device (key from `periwire_list_devices`) | | |
| `periwire_reset_identity` `{confirmed_by_user}` | New fingerprint for this computer; every peer must re-trust it | | yes |
| `periwire_export` `{enabled}` | Start/stop the exporter | | yes |
| `periwire_logs` `{lines?}` | Recent daemon log | yes | |
| `periwire_driver_status` | Windows only: is the virtual USB host controller driver installed? | yes | |
| `periwire_driver_install` `{confirmed_by_user}` | Windows only: install the signed driver carried inside PeriWire (kernel driver; USB 3 hubs restart briefly) | | yes |

## Safety rules built in

- **Trust needs a human.** `periwire_trust_add` refuses unless `confirmed_by_user` is `true`, and the server instructions
  tell the model to set it only after the user compared the fingerprint with `periwire id` on the other computer.
  A wrong trust decision lets another machine take the user's devices.
- **Driver install needs a human.** `periwire_driver_install` also requires `confirmed_by_user=true`.
- **Identity reset needs a human.** `periwire_reset_identity` also requires `confirmed_by_user=true`.
- **Eject is safe by default.** `periwire_eject` fails on open files; `force` is opt-in and described as lossy.
- **No device data access.** The server only moves connections; it cannot read, write, format or partition a device.

## Typical conversations

- *"Why can't my build machine see the iPad?"* → `periwire_doctor`, `periwire_status`, `periwire_logs`.
- *"Share the flash drive with the Mac."* → `periwire_list_devices` → `periwire_share` → (human pairing) → done.
- *"Give me the camera on media-pc."* → `periwire_list_remote` → `periwire_attach` → later `periwire_eject`.

## Protocol details

stdio transport, newline-delimited JSON-RPC 2.0; negotiates protocol `2025-06-18`, `2025-03-26` or `2024-11-05`.
Implements `initialize`, `ping`, `tools/list`, `tools/call` (plus empty `resources/list` and `prompts/list`).
