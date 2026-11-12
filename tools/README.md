# tools/

Developer helpers, not part of the product. Configure the lab in `lab.env` (copy `lab.env.example`; git-ignored).

- `lib.sh` — shared helpers: `.env` parsing (never printed), `run_sudo`, `remote`, `remote_sudo`, `guest`.
- `e2e.sh` — laptop → server end-to-end test with real daemons (`run`, `up`, `attach`, `eject`, `down`, `cli`, `logs`).
- `deploy.sh` — build release and copy the binary to the lab server.
- `usb-diag.sh [vid:pid]` — read-only USB state, filtered kernel log, VirtualHere status.
- `usb-configure.sh vid:pid` — select USB config 1 for a device the kernel left unconfigured (no data access).
- `loopback-bench.sh` — throughput over loopback.
- `mac-build.sh` — sync, test and build on a macOS machine over ssh (`PERIWIRE_GUEST_HOST` overrides the guest address).
- `mac-e2e.sh`, `mac-oc-sip.sh` — end-to-end test on a throwaway SIP-relaxed Mac guest, and the OpenCore patch helper it uses.
- `win-e2e.sh`, `win-soak.sh`, `win-daemon.ps1`, `winrun.py` — the same for the Windows lab VM (attach/detach cycles, trace flag).
- `make-icons.py` — render the app and tray icons.
- `screenshots.py` — regenerate the WebP screenshots in `docs/img/` from the UI's demo mode (see docs/DEVELOPMENT.md).

The hardware tests take the device to test with as `vid:pid` (argument, or `PERIWIRE_TEST_DEVICE`); there is no default, use a spare device. Nothing
about a particular lab is built in: addresses, users and hosts come from `lab.env` (see `lab.env.example`) or the environment.

Note: `dmesg` can be flooded by audit spam on some systems; use `journalctl -k`.

## Windows VM helper

`tools/winrun.py` runs PowerShell on the lab Windows VM through the builder (SSH tunnel + WinRM) and copies files to it
via a short-lived HTTP server on the builder's libvirt bridge. Setup: `python3 -m venv .venv && .venv/bin/pip install pywinrm requests-ntlm`.
Credentials come from `.env` (`windows_user`/`windows_password`, falling back to `username`/`sudo_password`) and are never printed.
For the Windows VM over Tailscale set `WINRUN_HOST=<its Tailscale address>` (direct, no tunnel). Away from the LAN set `PERIWIRE_REMOTE_HOST` to the builder's Tailscale address (it also overrides `REMOTE_HOST` in the other tools).
