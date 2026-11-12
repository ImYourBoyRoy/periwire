# Troubleshooting

Start with `periwire doctor` on both computers; it checks most of what is below and prints a fix.

| Symptom | Likely cause | Fix |
|---|---|---|
| `the periwire daemon is not running` (exit 3) | no daemon, or it uses another socket | `periwire daemon`; check `--socket`/`PERIWIRE_SOCKET` |
| `permission denied on …periwire.sock` | the daemon runs as root and your user may not use its socket | start it with `sudo` (the invoking user gets access), or `--socket-group`; for the service: `sudo usermod -aG periwire $USER` and log in again |
| `TLS handshake failed (is that machine's fingerprint trusted?)` | the two computers don't trust each other yet | on each side `periwire trust host <other>` / `periwire trust add <fp>`; both directions are needed |
| `importing needs root` | the Linux daemon is not root | run it with `sudo` or as a service |
| `vhci_hcd module is not available` | kernel module missing | `sudo modprobe vhci-hcd`; on Ubuntu install `linux-modules-extra-$(uname -r)` |
| `refused to export …` on attach | device not shared, already used by another computer, still mounted on the sharing side, or could not be claimed | `periwire devices` on the sharing side; eject/unmount it there; read its `periwire logs` |
| `refusing to share …: still mounted locally` | the sharing computer auto-mounted the drive | eject it there (file manager or `udisksctl unmount -b /dev/sdX1`) |
| After a network drop an `--auto` connection keeps saying `reconnecting` and the exporter log says `still mounted locally` | The drive was handed back to the sharing computer and its desktop mounted it. With current versions the exporter keeps the drive for 60 s after a lost connection (`connection lost: keeping … so the same computer can reconnect`), so a reconnect within that time resumes silently (`resumed …`). Outages longer than that, importers without `--auto`, and older exporters still hit this | Unmount the drive on the sharing computer and the connection resumes by itself; raise `PERIWIRE_RECONNECT_GRACE` (seconds) on the sharing computer for longer outages |
| `… is being held for another computer that lost its connection` | Another computer's connection to this drive dropped a moment ago and the exporter is keeping it for them | Wait for the grace period (60 s by default) or disconnect it from the sharing computer's `devices` list |
| device attaches but nothing appears | the OS needs a moment (SCSI scan takes a few seconds), or a missing driver | wait ~5 s; check `dmesg`/`journalctl -k` on the using computer |
| `…is busy (files are open on it)` on eject | a program uses the mount | close it, or `periwire detach … --force` (may lose data) |
| device didn't return after eject | kernel re-probe takes a few seconds; or the daemon was killed with `-9` | wait; if it stays missing, replug it; always stop the daemon with SIGTERM/`periwire stop` |
| Windows, **read-only**: the drive letter appears but Explorer says the drive is unreadable or write-protected; `Get-Volume` shows an unknown file system of size 0; the log has `Ntfs` event 55 ("scanned online") | An NTFS drive whose journal was emptied (not cleanly left by Windows) needs one *write* when Windows mounts it, and a write-protected disk refuses it. Linux's kernel `ntfs3` driver empties the journal on every read-write mount, so a desktop that auto-mounts the drive before you share it causes this. Nothing is wrong with the data (`chkdsk` without `/f` reports no problems) | Mount drives read-only on the sharing computer (or turn off auto-mount for it), then attach read-only. A drive already in this state mounts normally once with a read-write attach (Windows rewrites its journal; this writes to the drive, so use a copy or a drive you can alter), after which read-only works again. **From this version** the Windows importer asks the sharing computer to show a valid journal in read-only sessions, so this case mounts by itself (the exporter's log says `…showing a valid clean journal header…`); a journal with real unfinished changes still needs an eject from Windows |
| slow transfers | latency-bound mass-storage protocol over Wi-Fi | use wired Ethernet; keep the path short |
| webcam/audio device attaches but doesn't work | isochronous endpoints unsupported | not supported yet ([PLATFORMS](PLATFORMS.md#limitations)) |
| `address already in use` for the exporter | port 3241 taken | `periwire daemon --listen 0.0.0.0:3242` |
| another tool (VirtualHere, usbipd) grabs devices | competing software | stop it; `doctor` warns when it sees it |
| macOS: `Failed to create IOUSBHostControllerInterface` | missing entitlement | see [MACOS](MACOS.md) |

Logs: `periwire logs -f` (or the Activity page). For USB-level detail run the daemon with `PERIWIRE_TRACE=1`.
