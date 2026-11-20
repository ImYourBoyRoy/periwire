# PeriWire for AI assistants and coding agents

PeriWire shares USB devices between computers over the network. This file is the shortest path to using it, or to working
on its code.

## Use it (you are an assistant helping a user)

Install the program from the [releases page](https://github.com/ImYourBoyRoy/periwire/releases), then connect:

```bash
claude mcp add periwire -- periwire mcp        # Claude Code
# any MCP client: { "mcpServers": { "periwire": { "command": "periwire", "args": ["mcp"] } } }
```

The MCP server talks to the local daemon (`periwire daemon`, or `sudo periwire service install`). Start with these tools:
`periwire_status` (what is going on), `periwire_doctor` (problems and how to fix them), `periwire_list_devices`,
`periwire_list_remote {addr}`, then `periwire_share` / `periwire_attach` / `periwire_eject`. Every CLI command takes
`--json`; `periwire api` lists the control API methods.

**Rules that matter:**
- Never set `confirmed_by_user` (trust a computer, install the Windows driver, reset identity) unless the user has just
  confirmed it. For trust, they must compare the fingerprint with `periwire id` on the other computer.
- Eject with the default (safe) mode; use `force` only when the user accepts losing unsaved data.
- The tools move connections only; they cannot read, write or format a device.
- On Linux, attaching needs root (`sudo periwire daemon`); on macOS it needs Apple's host-controller entitlement or SIP relaxed
  ([docs/MACOS.md](docs/MACOS.md)); on Windows the driver is installed with `periwire driver install`.
- Webcams and USB audio (isochronous) are not supported yet.

Docs: [MCP](docs/MCP.md) · [CLI](docs/CLI.md) · [getting started](docs/GETTING-STARTED.md) · [troubleshooting](docs/TROUBLESHOOTING.md) ·
[API](docs/API.md) · [security model](docs/SECURITY.md).

## Work on the code

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --release -- -D warnings
cargo test --workspace --exclude periwire-gui
python3 tools/ui-churn.py --check
```

Layout: `crates/usbip-proto` (wire format and the size limits every peer-controlled length is checked against), `crates/periwire` (daemon, `export/` the sharing side, importers, CLI, MCP, doctor),
`crates/periwire-client` (control-API client), `gui/` (Tauri app; frontend is plain HTML/CSS/JS with a demo mode),
`docs/`, `tools/` (dev scripts, not shipped). Details in [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md) and
[ARCHITECTURE](docs/ARCHITECTURE.md); contribution rules in [CONTRIBUTING.md](CONTRIBUTING.md).

Conventions: everything the UI can do must also exist in the control API, the CLI and an MCP tool; never `kill -9` a daemon that holds
devices; keep files under ~500 lines and functions under ~100; validate anything a peer sends before allocating or indexing with it; never commit `.env`, `tools/lab.env`, `.state/` or `private/`; commit messages are plain text with no attribution trailers.
