#!/usr/bin/env python3
"""Run PowerShell on the lab Windows VM through the builder (SSH tunnel + WinRM), and copy files to it.

  .venv/bin/python tools/winrun.py run  'Get-ComputerInfo | Select OsName,OsVersion'
  .venv/bin/python tools/winrun.py file script.ps1                 # run a local .ps1 remotely
  .venv/bin/python tools/winrun.py get  'C:\\remote\\file.bin' local.bin
  .venv/bin/python tools/winrun.py put  local.exe 'C:\\periwire\\periwire.exe'

Credentials: .env `windows_user` / `windows_password`, falling back to `username` / `sudo_password`
(the lab uses one account everywhere). Values are read key-by-key, sent only to WinRM, and never printed.
Direct mode: WINRUN_HOST=<tailscale ip of the VM> talks to it directly (no SSH tunnel; files are served from this
machine's Tailscale address). Otherwise the builder tunnel is used.
Lab settings: tools/lab.env (REMOTE_HOST, WIN_HOST, WIN_FILE_SERVER_BIND; see lab.env.example). PERIWIRE_REMOTE_HOST overrides
REMOTE_HOST (e.g. the builder's Tailscale address when away from the LAN).
Setup once:  python3 -m venv .venv && .venv/bin/pip install pywinrm requests-ntlm
"""
from __future__ import annotations

import os
import re
import socket
import subprocess
import sys
import time
from pathlib import Path

import winrm

ROOT = Path(__file__).resolve().parents[1]
PORT = 15985
DIRECT = os.environ.get("WINRUN_HOST", "")


def read_kv(path: Path) -> dict[str, str]:
    out: dict[str, str] = {}
    if path.is_file():
        for line in path.read_text().splitlines():
            m = re.match(r"\s*([A-Za-z_][A-Za-z0-9_]*)\s*=\s*(.*)$", line)
            if m and not line.lstrip().startswith("#"):
                out[m.group(1)] = m.group(2).strip().strip("\"'")
    return out


ENV = read_kv(ROOT / ".env")
LAB = read_kv(ROOT / "tools" / "lab.env")
SSH_HOST = os.environ.get("PERIWIRE_REMOTE_HOST") or LAB.get("REMOTE_HOST", "")
SSH_USER = LAB.get("REMOTE_USER") or ENV.get("username", "")
WIN_HOST = LAB.get("WIN_HOST", "")                      # the Windows VM's address as the builder sees it (not needed with WINRUN_HOST)
FILE_SERVER_BIND = LAB.get("WIN_FILE_SERVER_BIND", "")  # builder-side address the VM can reach, for `put` (e.g. the libvirt bridge address)
USER = os.environ.get("WINRUN_USER") or ENV.get("windows_user") or ENV.get("username", "")
PW = ENV.get("windows_password") or ENV.get("sudo_password", "")


def redact(text: str) -> str:
    return text.replace(PW, "***") if PW and len(PW) >= 4 else text


class Tunnel:
    def __enter__(self):
        if DIRECT:
            return self
        if not SSH_HOST:
            sys.exit("REMOTE_HOST not set (tools/lab.env or PERIWIRE_REMOTE_HOST)")
        if not WIN_HOST:
            sys.exit("WIN_HOST not set (tools/lab.env), or use WINRUN_HOST for a direct connection to the Windows VM")
        self.p = subprocess.Popen(
            ["ssh", "-N", "-o", "BatchMode=yes", "-o", "ExitOnForwardFailure=yes", "-o", "ConnectTimeout=15",
             "-i", str(Path.home() / ".ssh/id_ed25519"), "-L", f"127.0.0.1:{PORT}:{WIN_HOST}:5985", f"{SSH_USER}@{SSH_HOST}"])
        for _ in range(40):
            try:
                socket.create_connection(("127.0.0.1", PORT), 0.5).close()
                return self
            except OSError:
                time.sleep(0.25)
        self.p.terminate()
        sys.exit("could not open the SSH tunnel to the Windows VM")

    def __exit__(self, *a):
        if not DIRECT:
            self.p.terminate()


def session() -> winrm.Session:
    if not (USER and PW):
        sys.exit("no credentials: set windows_user/windows_password (or username/sudo_password) in .env")
    ep = f"http://{DIRECT}:5985/wsman" if DIRECT else f"http://127.0.0.1:{PORT}/wsman"
    return winrm.Session(ep, auth=(USER, PW), transport="ntlm",
                         read_timeout_sec=240, operation_timeout_sec=200)


def run_ps(s: winrm.Session, script: str) -> int:
    r = s.run_ps(script)
    sys.stdout.write(redact(r.std_out.decode(errors="replace")))
    if r.std_err:
        # pywinrm returns CLIXML for PowerShell errors; strip the envelope noise
        err = redact(r.std_err.decode(errors="replace"))
        err = re.sub(r"<[^>]+>", "", err.replace("_x000D__x000A_", "\n")).strip()
        if err:
            sys.stderr.write(err + "\n")
    return r.status_code


def put(s: winrm.Session, local: Path, remote: str) -> int:
    """Copy via a short-lived HTTP server bound to the builder's libvirt bridge (the VM is NATed behind it)."""
    port = 8099
    if DIRECT:
        me = subprocess.run(["tailscale", "ip", "-4"], capture_output=True, text=True).stdout.split()[0]
        srv = subprocess.Popen([sys.executable, "-m", "http.server", str(port), "--bind", me, "--directory", str(local.parent)],
                               stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        try:
            time.sleep(1.5)
            return run_ps(s, f"New-Item -ItemType Directory -Force -Path (Split-Path '{remote}') | Out-Null; "
                             f"Invoke-WebRequest -UseBasicParsing -Uri 'http://{me}:{port}/{local.name}' -OutFile '{remote}'; "
                             f"(Get-Item '{remote}').Length; (Get-FileHash '{remote}' -Algorithm SHA256).Hash")
        finally:
            srv.terminate()
    if not FILE_SERVER_BIND:
        sys.exit("set WIN_FILE_SERVER_BIND in tools/lab.env to the builder-side address the Windows VM can reach (e.g. its libvirt bridge address)")
    remote_tmp = f"/tmp/periwire-put-{os.getpid()}"
    ssh = ["ssh", "-o", "BatchMode=yes", "-i", str(Path.home() / ".ssh/id_ed25519"), f"{SSH_USER}@{SSH_HOST}"]
    subprocess.run(ssh + [f"mkdir -p {remote_tmp}"], check=True)
    subprocess.run(["scp", "-q", "-i", str(Path.home() / ".ssh/id_ed25519"), str(local), f"{SSH_USER}@{SSH_HOST}:{remote_tmp}/"], check=True)
    srv = subprocess.Popen(ssh + [f"cd {remote_tmp} && exec python3 -m http.server {port} --bind {FILE_SERVER_BIND} >/dev/null 2>&1"])
    try:
        time.sleep(2)
        name = local.name
        rc = run_ps(s, f"New-Item -ItemType Directory -Force -Path (Split-Path '{remote}') | Out-Null; "
                       f"Invoke-WebRequest -UseBasicParsing -Uri 'http://{FILE_SERVER_BIND}:{port}/{name}' -OutFile '{remote}'; "
                       f"(Get-Item '{remote}').Length; (Get-FileHash '{remote}' -Algorithm SHA256).Hash")
    finally:
        srv.terminate()
        subprocess.run(ssh + [f"pkill -f 'http.server {port}' ; rm -rf {remote_tmp}"], check=False)
    return rc


def get(s: winrm.Session, remote: str, local: Path, chunk: int = 600_000) -> int:
    """Copy a (modest-size) file from the VM by reading it in base64 chunks over WinRM."""
    import base64
    r = s.run_ps(f"(Get-Item '{remote}').Length")
    size = int(r.std_out.decode().strip().splitlines()[-1])
    with open(local, "wb") as f:
        off = 0
        while off < size:
            ps = (f"$fs=[IO.File]::OpenRead('{remote}'); $fs.Seek({off},0)|Out-Null; $b=New-Object byte[] {chunk}; "
                  f"$n=$fs.Read($b,0,{chunk}); $fs.Close(); [Convert]::ToBase64String($b,0,$n)")
            out = s.run_ps(ps).std_out.decode().strip().splitlines()[-1]
            data = base64.b64decode(out)
            f.write(data)
            off += len(data)
    print(f"{local} ({size} bytes)")
    return 0


def main() -> int:
    if len(sys.argv) < 3:
        print(__doc__)
        return 2
    cmd = sys.argv[1]
    with Tunnel():
        s = session()
        if cmd == "run":
            return run_ps(s, sys.argv[2])
        if cmd == "file":
            return run_ps(s, Path(sys.argv[2]).read_text())
        if cmd == "get" and len(sys.argv) == 4:
            return get(s, sys.argv[2], Path(sys.argv[3]))
        if cmd == "put" and len(sys.argv) == 4:
            return put(s, Path(sys.argv[2]), sys.argv[3])
    print(__doc__)
    return 2


if __name__ == "__main__":
    sys.exit(main())
