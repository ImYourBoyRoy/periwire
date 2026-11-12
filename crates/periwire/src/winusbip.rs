//! Windows importer glue: drives usbip-win2's `usbip.exe` (https://github.com/vadimgrn/usbip-win2), which PeriWire
//! carries and installs itself (see `windrv.rs`). A copy installed by the upstream installer is used too.
#![cfg(windows)]
use anyhow::{anyhow, bail, Result};
use std::path::PathBuf;
use tokio::process::Command;

pub fn find() -> Option<PathBuf> {
    let mut cands: Vec<PathBuf> = vec![crate::windrv::install_dir().join("usbip.exe")];
    for var in ["ProgramFiles", "ProgramW6432"] {
        if let Some(pf) = std::env::var_os(var) {
            cands.push(PathBuf::from(&pf).join("USBip").join("usbip.exe"));
        }
    }
    if let Some(paths) = std::env::var_os("PATH") {
        cands.extend(std::env::split_paths(&paths).map(|d| d.join("usbip.exe")));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(d) = exe.parent() {
            cands.push(d.join("usbip.exe"));
        }
    }
    cands.into_iter().find(|p| p.exists())
}

/// `usbip.exe -t <port> attach -r 127.0.0.1 -b <busid> --terse --once`: the global `-t` is the server's TCP port (after
/// `attach`, `-t` would mean --terse). `--terse` prints just the virtual port number; `--once` stops usbip-win2 from
/// retrying by itself, because the daemon owns reconnection.
pub async fn attach(usbip: PathBuf, port: u16, busid: String) -> Result<u32> {
    let out = Command::new(&usbip).args(["-t", &port.to_string(), "attach", "-r", "127.0.0.1", "-b", &busid, "--terse", "--once"]).output().await?;
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    if !out.status.success() {
        bail!("usbip.exe attach failed: {}", text.trim());
    }
    parse_port(&text).ok_or_else(|| anyhow!("usbip.exe attached the device but did not report a port: {}", text.trim()))
}

pub async fn detach(usbip: &PathBuf, port: u32) -> Result<()> {
    let out = Command::new(usbip).args(["detach", "-p", &port.to_string()]).output().await?;
    if !out.status.success() {
        bail!("usbip.exe detach failed: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(())
}

/// The port usbip reports: with --terse the whole output is the number; otherwise the first integer after "port".
fn parse_port(text: &str) -> Option<u32> {
    if let Ok(n) = text.trim().parse::<u32>() {
        return Some(n);
    }
    let l = text.to_lowercase();
    let i = l.find("port")?;
    l[i + 4..].split(|c: char| !c.is_ascii_digit()).find(|t| !t.is_empty())?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::parse_port;
    #[test]
    fn finds_port_number() {
        assert_eq!(parse_port("attach: succesfully attached to port 3"), Some(3));
        assert_eq!(parse_port("Port 12 is now in use"), Some(12));
        assert_eq!(parse_port("nothing here"), None);
        assert_eq!(parse_port("2\r\n"), Some(2));
    }
}
