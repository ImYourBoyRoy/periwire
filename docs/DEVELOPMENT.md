# Development

```bash
cargo build                         # CLI, daemon, MCP (debug)
cargo test --workspace --exclude periwire-gui
cargo build --release -p periwire-gui # desktop app (see GUI.md for system packages)
python3 -m http.server 8765 --directory gui/ui   # GUI in demo mode, no daemon needed
```

Rust 1.80+. libusb is vendored (needs a C compiler); on Linux set up `LIBUDEV_NO_PKG_CONFIG=1` is already configured in
`.cargo/config.toml`.

## Layout

```
crates/usbip-proto     wire format (unit tested)
crates/periwire          daemon, exporter, importer, CLI, MCP, doctor
crates/periwire-client   control-API client (CLI, MCP and GUI use it)
gui/src-tauri          desktop app shell (tray, daemon watcher)
gui/ui                 frontend: plain HTML/CSS/JS, built-in demo mode
docs/                  documentation
tools/                 developer scripts (not shipped)
```

## Try the daemon without touching real devices

```bash
export PERIWIRE_HOME=/tmp/uf1 PERIWIRE_SOCKET=/tmp/uf1/d.sock
mkdir -p $PERIWIRE_HOME
cargo run -- daemon --listen 127.0.0.1:3299 &
cargo run -- status ; cargo run -- doctor ; cargo run -- devices
cargo run -- stop
```
Without root the daemon can list devices and the control API works; importing needs root on Linux.

## Lab end-to-end test

`tools/e2e.sh` shares a real device from this computer to a second Linux machine, mounts it **read-only**, safe-ejects it
and checks it returns. It needs `tools/lab.env` (copy `tools/lab.env.example`; git-ignored), SSH access to the second machine
and a `.env` with `sudo_password` (git-ignored; used for `sudo` on stdin only). Never run it with a device you cannot
afford to lose connectivity to, and unmount the device locally first.

Other scripts: `usb-diag.sh` (read-only USB state), `loopback-bench.sh`, `mac-build.sh` (build/test on a macOS machine
over ssh), `make-icons.py` (regenerate every icon from the artwork in `images/`).

## Screenshots

The images in `docs/img/` (WebP) are generated from the app's demo mode, so they never show real devices or machines. After a UI change run
`python3 tools/screenshots.py` (needs `pip install playwright pillow` and a Chrome or Chromium; set `CHROME=/path` if it is not on `PATH`) and commit
the result. `python3 tools/screenshots.py themes` regenerates only the images whose name contains "themes".

## UI stability check

The window gets a status update about every 2 seconds. The UI must update the numbers that changed and leave everything else alone: a rebuilt button
cannot be clicked reliably, selected text disappears and icons flicker. `python3 tools/ui-churn.py` prints how many elements each page tears down and
rebuilds while idle in demo mode, and `--check` fails if a page without live traffic rebuilds anything (it runs in CI). The tray follows the same rule:
`gui/src-tauri/src/main.rs` re-applies the menu, icon and tooltip only when they would differ from what is shown (unit-tested).

## Conventions

- Everything the UI can do must be reachable through the control API ([API](API.md)), the CLI, and an MCP tool.
- Add a unit test for new parsing/validation; use `api.rs`'s round-trip test for new API methods.
- Never `kill -9` a daemon holding devices; use SIGTERM.
- Do not commit `.env`, `tools/lab.env`, `.state/` or `private/` (they are in `.gitignore`).
