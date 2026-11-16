# Contributing

Thanks for helping. Bug reports, device compatibility reports and pull requests are all welcome.

## Before you start

- **Bugs and ideas:** open an [issue](https://github.com/ImYourBoyRoy/periwire/issues/new/choose). Run `periwire doctor` and paste its output.
- **Security problems:** do not open an issue; follow [SECURITY.md](SECURITY.md).
- **Bigger changes:** open an issue first so we agree on the approach.

## Build and test

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --release -- -D warnings
cargo test --workspace --exclude periwire-gui
python3 tools/ui-churn.py --check          # the desktop UI must not rebuild idle elements
```

The [development guide](docs/DEVELOPMENT.md) covers layout, running the daemon without real devices, the lab scripts and
regenerating screenshots. CI runs the same checks on Linux, macOS and Windows.

## Ground rules

- Everything the UI can do must also be possible through the [control API](docs/API.md), the CLI and an MCP tool.
- Add a test for new parsing, validation and API methods.
- Never kill a daemon that holds devices with `kill -9`.
- Do not commit `.env`, `tools/lab.env`, `.state/` or `private/`. Screenshots come from the demo mode, never from real devices.
- Keep commit messages plain and describe why the change is needed. Update the docs and `CHANGELOG.md` with the change.

By contributing you agree your work is released under the [MIT license](LICENSE).
