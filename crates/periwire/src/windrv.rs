//! The Windows virtual USB host controller, carried inside this program.
//!
//! Windows has no built-in way to present a remote USB device as a local one, so importing needs a signed kernel
//! driver. Rather than make the user download and run an installer, PeriWire embeds the unmodified, Microsoft-signed
//! usbip-win2 driver packages (BSD-2-Clause, see `assets/usbip-win2/SOURCE.md`) and installs them itself with
//! `periwire driver install`, the same model VirtualHere uses. The kernel driver still has to be installed once
//! with Administrator rights; that is a Windows rule, not something a program can skip.
#![cfg(windows)]
use anyhow::{anyhow, bail, Context, Result};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::process::Command;

pub const VERSION: &str = "0.9.8.1";
/// Hardware id of the root-enumerated host controller device the UDE driver binds to.
pub const HWID: &str = r"ROOT\USBIP_WIN2\UDE";

macro_rules! asset {
    ($n:literal) => {
        ($n, include_bytes!(concat!("../assets/usbip-win2/", $n)) as &[u8])
    };
}
const FILES: &[(&str, &[u8])] = &[
    asset!("usbip2_filter.inf"),
    asset!("usbip2_filter.sys"),
    asset!("usbip2_filter.cat"),
    asset!("usbip2_ude.inf"),
    asset!("usbip2_ude.sys"),
    asset!("usbip2_ude.cat"),
    asset!("devnode.exe"),
    asset!("usbip.exe"),
    asset!("libusbip.dll"),
    asset!("resources.dll"),
    asset!("LICENSE.txt"),
];

#[derive(Serialize, Clone, Debug)]
pub struct Pkg {
    pub published: String,
    pub original: String,
    pub version: String,
}

#[derive(Serialize, Debug)]
pub struct Status {
    pub embedded_version: String,
    pub admin: bool,
    pub vc_runtime: bool,
    pub packages: Vec<Pkg>,
    pub device: Option<String>,
    pub tools: Option<String>,
    pub ready: bool,
}

pub fn install_dir() -> PathBuf {
    let base = std::env::var_os("ProgramData").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\ProgramData"));
    base.join("PeriWire").join("usbip-win2")
}

/// Write `bytes` to `p` in one step: a crash or a full disk must not leave a half-written program that is trusted later.
fn write_atomic(p: &Path, bytes: &[u8]) -> Result<()> {
    let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let tmp = p.with_file_name(format!("{name}.part"));
    std::fs::write(&tmp, bytes).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, p).with_context(|| format!("replacing {}", p.display()))?;
    Ok(())
}

/// Write the embedded files into `dir` (only the ones that differ).
pub fn extract(dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    for (name, bytes) in FILES {
        let p = dir.join(name);
        if std::fs::read(&p).map(|b| b == *bytes).unwrap_or(false) {
            continue;
        }
        write_atomic(&p, bytes)?;
    }
    Ok(())
}

/// Take `dir` away from whoever created it and leave it changeable only by SYSTEM and Administrators (everyone else may read and run).
/// The tools in it are started by an elevated process, so a folder that an ordinary user pre-created, or can write to, would let that user
/// plant a program that runs as Administrator. Well-known SIDs are used so this works in every Windows language.
fn lock_down(dir: &Path) -> Result<()> {
    let icacls = sysdir().join("icacls.exe");
    let d = dir.display().to_string();
    let steps: [Vec<&str>; 3] = [
        vec![&d, "/setowner", "*S-1-5-32-544", "/T", "/C", "/Q"],
        vec![&d, "/reset", "/T", "/C", "/Q"],
        vec![&d, "/inheritance:r", "/grant:r", "*S-1-5-18:(OI)(CI)F", "*S-1-5-32-544:(OI)(CI)F", "*S-1-5-32-545:(OI)(CI)RX", "/T", "/C", "/Q"],
    ];
    for args in steps {
        let (c, t) = run(&icacls, &args)?;
        if c != 0 {
            bail!("could not restrict {} to Administrators (icacls exit {c}): {}", dir.display(), tail(&t));
        }
    }
    Ok(())
}

/// The tools the importer starts, by name: every file of `FILES` that is a program or library (not driver packages or the licence).
const TOOLS: &[&str] = &["usbip.exe", "libusbip.dll", "resources.dll", "devnode.exe"];

/// Are the tools in `dir` exactly the ones carried inside this program?
fn tools_match(dir: &Path) -> bool {
    TOOLS.iter().all(|t| FILES.iter().find(|(n, _)| n == t).is_some_and(|(_, bytes)| std::fs::read(dir.join(t)).map(|b| b == *bytes).unwrap_or(false)))
}

/// Unpack into the install directory, locking the directory down first when this process is elevated, and replacing anything that differs.
fn extract_install(dir: &Path) -> Result<()> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    if is_admin() {
        lock_down(dir)?;
    }
    extract(dir)
}

/// The extracted `usbip.exe`, if present.
pub fn usbip_exe() -> Option<PathBuf> {
    let p = install_dir().join("usbip.exe");
    p.exists().then_some(p)
}

/// The `usbip.exe` to run: the one carried inside this program, in a folder only administrators can write to. The daemon runs elevated, so it
/// refreshes the folder and its contents first; a copy that is not byte-for-byte the embedded one is never returned.
pub fn usbip_exe_or_extract() -> Option<PathBuf> {
    let dir = install_dir();
    if is_admin() {
        if let Err(e) = extract_install(&dir) {
            crate::logln!("could not prepare the USB/IP tools: {e:#}");
            return None;
        }
    }
    tools_match(&dir).then(|| dir.join("usbip.exe"))
}

fn sysdir() -> PathBuf {
    std::env::var_os("SystemRoot").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\Windows")).join("System32")
}

fn run(exe: &Path, args: &[&str]) -> Result<(i32, String)> {
    let out = Command::new(exe).args(args).output().with_context(|| format!("running {}", exe.display()))?;
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    Ok((out.status.code().unwrap_or(-1), text))
}

fn pnputil(args: &[&str]) -> Result<(i32, String)> {
    run(&sysdir().join("pnputil.exe"), args)
}

/// High-integrity (elevated) token?
pub fn is_admin() -> bool {
    run(&sysdir().join("whoami.exe"), &["/groups"]).map(|(_, t)| t.contains("S-1-16-12288")).unwrap_or(false)
}

/// The Microsoft Visual C++ runtime that usbip.exe / devnode.exe link against.
pub fn vc_runtime_ok() -> bool {
    ["vcruntime140.dll", "vcruntime140_1.dll", "msvcp140.dll"].iter().all(|f| sysdir().join(f).exists())
}

fn values(block: &str) -> Vec<String> {
    block.lines().filter_map(|l| l.split_once(':').map(|(_, v)| v.trim().to_string())).collect()
}
fn is_oem_inf(v: &str) -> bool {
    let l = v.to_lowercase();
    l.starts_with("oem") && l.ends_with(".inf") && l[3..l.len() - 4].chars().all(|c| c.is_ascii_digit())
}
fn is_usbip_inf(v: &str) -> bool {
    matches!(v.to_lowercase().as_str(), "usbip2_ude.inf" | "usbip2_filter.inf")
}

/// Installed usbip-win2 driver packages, from `pnputil /enum-drivers` (language-independent: parsed by shape).
pub fn packages() -> Vec<Pkg> {
    let text = match pnputil(&["/enum-drivers"]) {
        Ok((_, t)) => t.replace("\r\n", "\n"),
        Err(_) => return vec![],
    };
    text.split("\n\n")
        .filter_map(|b| {
            let v = values(b);
            let original = v.iter().find(|x| is_usbip_inf(x))?.clone();
            let published = v.iter().find(|x| is_oem_inf(x))?.clone();
            let version = v.iter().find(|x| x.contains('/') && x.contains('.') && x.contains(' ')).cloned().unwrap_or_default();
            Some(Pkg { published, original, version })
        })
        .collect()
}

/// Instance id of the host controller device, if it exists.
pub fn device() -> Option<String> {
    let (_, text) = pnputil(&["/enum-devices", "/deviceid", HWID]).ok()?;
    text.lines().filter_map(|l| l.split_once(':').map(|(_, v)| v.trim().to_string())).find(|v| v.to_uppercase().starts_with("ROOT\\"))
}

pub fn status() -> Status {
    let packages = packages();
    let device = device();
    let tools = usbip_exe().map(|p| p.display().to_string());
    let ready = device.is_some() && packages.iter().any(|p| p.original.eq_ignore_ascii_case("usbip2_ude.inf"));
    Status { embedded_version: VERSION.into(), admin: is_admin(), vc_runtime: vc_runtime_ok(), packages, device, tools, ready }
}

fn ok_code(c: i32) -> bool {
    c == 0 || c == 3010 // 3010 = success, restart required
}

/// Install both driver packages and create the host controller device. Returns true when Windows asks for a restart.
pub fn install() -> Result<bool> {
    if !is_admin() {
        bail!("installing a driver needs Administrator rights: run this from an elevated prompt, or add --elevate");
    }
    let dir = install_dir();
    extract_install(&dir)?; // also refreshes the tools if this program is newer, and locks the folder down
    if status().ready {
        return Ok(false); // already installed: running devnode again could create a second host controller device
    }
    let filter = dir.join("usbip2_filter.inf");
    let ude = dir.join("usbip2_ude.inf");
    let mut reboot = false;

    let (c, t) = pnputil(&["/add-driver", &filter.display().to_string(), "/install"])?;
    if !ok_code(c) {
        bail!("pnputil could not install the filter driver (exit {c}): {}", tail(&t));
    }
    reboot |= c == 3010;

    // Same call the upstream installer makes: devnode installs the UDE driver and creates the device node.
    let devnode = dir.join("devnode.exe");
    let first = if vc_runtime_ok() { run(&devnode, &["install", &ude.display().to_string(), HWID]).ok() } else { None };
    let mut devnode_note = String::new();
    match first {
        Some((c, _)) if ok_code(c) => reboot |= c == 3010,
        other => {
            devnode_note = other.map(|(c, t)| format!("devnode exit {c}: {}", tail(&t))).unwrap_or_else(|| "devnode.exe not runnable (Visual C++ runtime missing?)".into());
            // Fallback with Windows' own tools (Windows 11 / Server 2022+ have `pnputil /add-device`).
            let (c, t) = pnputil(&["/add-driver", &ude.display().to_string(), "/install"])?;
            if !ok_code(c) {
                bail!("could not install the UDE driver ({devnode_note}); pnputil exit {c}: {}", tail(&t));
            }
            let (c, t) = pnputil(&["/add-device", HWID, "/install"])?;
            if !ok_code(c) {
                bail!("could not create the virtual host controller ({devnode_note}); pnputil exit {c}: {}", tail(&t));
            }
            reboot |= c == 3010;
        }
    }
    if device().is_none() {
        bail!("the drivers were installed but the virtual host controller device did not appear {}", devnode_note);
    }
    Ok(reboot)
}

/// Remove the host controller device, both driver packages and the extracted tools.
pub fn uninstall() -> Result<()> {
    if !is_admin() {
        bail!("removing a driver needs Administrator rights: run this from an elevated prompt, or add --elevate");
    }
    let dir = install_dir();
    if let Some(inst) = device() {
        let devnode = dir.join("devnode.exe");
        let done = devnode.exists() && vc_runtime_ok() && run(&devnode, &["remove", HWID, "root"]).map(|(c, _)| ok_code(c)).unwrap_or(false);
        if !done {
            let (c, t) = pnputil(&["/remove-device", &inst, "/subtree"])?;
            if !ok_code(c) {
                bail!("could not remove the virtual host controller (exit {c}): {}", tail(&t));
            }
        }
    }
    for p in packages() {
        let (c, t) = pnputil(&["/delete-driver", &p.published, "/uninstall", "/force"])?;
        if !ok_code(c) {
            bail!("could not delete driver package {} (exit {c}): {}", p.published, tail(&t));
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

/// Re-run this program elevated (UAC prompt) with `args` and wait for it to finish.
pub fn elevate_and_wait(args: &[&str]) -> Result<i32> {
    let exe = std::env::current_exe()?;
    let arglist = args.join(" ");
    let ps = format!("$p = Start-Process -FilePath '{}' -ArgumentList '{}' -Verb RunAs -Wait -PassThru; exit $p.ExitCode", exe.display().to_string().replace('\'', "''"), arglist.replace('\'', "''"));
    let st = Command::new(sysdir().join(r"WindowsPowerShell\v1.0\powershell.exe"))
        .args(["-NoProfile", "-NonInteractive", "-Command", &ps])
        .status()
        .map_err(|e| anyhow!("could not start PowerShell to ask for elevation: {e}"))?;
    Ok(st.code().unwrap_or(1))
}

fn tail(t: &str) -> String {
    let s: Vec<&str> = t.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    s[s.len().saturating_sub(3)..].join(" | ")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn shapes() {
        assert!(is_oem_inf("oem42.inf") && is_oem_inf("OEM7.INF") && !is_oem_inf("usbip2_ude.inf") && !is_oem_inf("oemx.inf"));
        assert!(is_usbip_inf("USBIP2_UDE.INF") && !is_usbip_inf("other.inf"));
        assert_eq!(values("Published Name:     oem1.inf\nOriginal Name:      usbip2_ude.inf"), vec!["oem1.inf", "usbip2_ude.inf"]);
    }
}
