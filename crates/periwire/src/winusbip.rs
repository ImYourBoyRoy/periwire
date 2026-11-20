//! Windows importer glue: drives usbip-win2's `usbip.exe` (https://github.com/vadimgrn/usbip-win2), which PeriWire
//! carries and installs itself (see `windrv.rs`). A copy installed by the upstream installer is used too.
#![cfg(windows)]
use anyhow::{anyhow, bail, Result};
use std::path::PathBuf;
use tokio::process::Command;

/// A `usbip.exe` installed by the upstream installer, in Program Files (only administrators can write there). The copy PeriWire carries
/// is `windrv::usbip_exe_or_extract`, which checks it against the embedded bytes. Nothing is taken from `PATH` or from the folder this
/// program runs from: the daemon is elevated, and either could be a place an ordinary user can write to.
pub fn find() -> Option<PathBuf> {
    ["ProgramFiles", "ProgramW6432"].iter().filter_map(std::env::var_os).map(|pf| PathBuf::from(pf).join("USBip").join("usbip.exe")).find(|p| p.exists())
}

/// `usbip.exe -t <port> attach -r 127.0.0.1 -b <busid> --terse --once`: the global `-t` is the server's TCP port (after
/// `attach`, `-t` would mean --terse). `--terse` prints just the virtual port number; `--once` stops usbip-win2 from
/// retrying by itself, because the daemon owns reconnection.
pub async fn attach(usbip: PathBuf, port: u16, busid: String) -> Result<u32> {
    let mut cmd = Command::new(&usbip);
    cmd.args(["-t", &port.to_string(), "attach", "-r", "127.0.0.1", "-b", &busid, "--terse", "--once"]).kill_on_drop(true); // not left running if the attach is cancelled
    let out = tokio::time::timeout(std::time::Duration::from_secs(60), cmd.output()).await.map_err(|_| anyhow!("usbip.exe attach did not finish within 60 s"))??;
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
