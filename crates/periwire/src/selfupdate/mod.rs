//! `periwire self-update`: fetch the release archive, check its SHA-256, and replace the programs next to this one (after enchi's `self-update`).
//!
//! Trust: the archive and its `.sha256` come from the release over HTTPS, so the checksum catches a damaged or swapped download but is not a
//! signature; `gh attestation verify <file> --repo ImYourBoyRoy/periwire` is the stronger check and is printed. `PERIWIRE_RELEASE_BASE_URL` points at a
//! folder holding the files (https, http on loopback, or file://) for mirrors and tests.
//!
//! What can be updated: a folder install (the tarball/zip layout, or the program inside a build folder): `periwire` and `periwire-gui` beside this
//! program are replaced; an AppImage: the AppImage file is replaced. Packages (.deb, the macOS disk image) are updated their own way: this says how.
mod archive;
mod fetch;

use anyhow::{anyhow, bail, Context, Result};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::io::IsTerminal;
use std::path::{Path, PathBuf};

const RELEASES: &str = "https://github.com/ImYourBoyRoy/periwire/releases";

/// What to download for this system: the archive name, or the AppImage's.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Kind {
    Archive,
    AppImage,
}

pub fn asset_name(version: &str, os: &str, arch: &str, kind: Kind) -> Result<String> {
    Ok(match (kind, os, arch) {
        (Kind::AppImage, "linux", "x86_64") => format!("periwire_{version}_amd64.AppImage"),
        (Kind::Archive, "linux", "x86_64") => format!("periwire-{version}-linux-x86_64.tar.gz"),
        (Kind::Archive, "macos", "aarch64") => format!("periwire-{version}-macos-arm64.tar.gz"),
        (Kind::Archive, "macos", "x86_64") => format!("periwire-{version}-macos-x86_64.tar.gz"),
        (Kind::Archive, "windows", "x86_64") => format!("periwire-{version}-windows-x86_64.zip"),
        _ => bail!("there is no PeriWire download for {os} on {arch}"),
    })
}

/// The expected hash of `asset` from a `.sha256` file (`<hash>  <name>` or just `<hash>`) or from a combined `SHA256SUMS.txt`.
pub fn expected_hash(text: &str, asset: &str) -> Option<String> {
    let is_hash = |s: &str| s.len() == 64 && s.bytes().all(|c| c.is_ascii_hexdigit());
    let mut lines = text.lines().map(str::trim).filter(|l| !l.is_empty());
    let first = lines.clone().next()?;
    let mut it = first.split_whitespace();
    if let (Some(h), None) = (it.next(), it.next()) {
        return is_hash(h).then(|| h.to_lowercase()); // a bare hash
    }
    lines.find_map(|l| {
        let mut p = l.split_whitespace();
        let (h, name) = (p.next()?, p.next()?.trim_start_matches('*'));
        (is_hash(h) && name == asset).then(|| h.to_lowercase())
    })
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

/// Where the programs to replace live, and how.
#[derive(Debug, PartialEq, Eq)]
pub enum Target {
    /// Replace these files (all in one folder).
    Folder { files: Vec<PathBuf> },
    /// Replace the AppImage itself.
    AppImage(PathBuf),
    /// Cannot be done here: say why and what to do instead.
    Elsewhere(String),
}

fn exe_names() -> (&'static str, &'static str) {
    if cfg!(windows) {
        ("periwire.exe", "periwire-gui.exe")
    } else {
        ("periwire", "periwire-gui")
    }
}

pub fn target(exe: &Path, appimage: Option<PathBuf>) -> Target {
    if let Some(a) = appimage {
        return Target::AppImage(a);
    }
    let s = exe.to_string_lossy();
    if s.contains(".app/Contents/") {
        return Target::Elsewhere(format!("this copy is inside the macOS app bundle; install the new disk image (.dmg) from {RELEASES}/latest"));
    }
    if ["/usr/", "/opt/", "/snap/", "/nix/store/", "/var/lib/flatpak/", "/app/"].iter().any(|p| exe.starts_with(p)) {
        return Target::Elsewhere(format!("this copy belongs to a package: update it with your package manager, or install the new .deb from {RELEASES}/latest"));
    }
    let Some(dir) = exe.parent() else { return Target::Elsewhere("cannot tell which folder this program is in".into()) };
    let (cli, gui) = exe_names();
    let files: Vec<PathBuf> = [cli, gui].iter().map(|n| dir.join(n)).filter(|p| p.is_file()).collect();
    Target::Folder { files }
}

fn ask(question: &str) -> Result<bool> {
    if !std::io::stdin().is_terminal() {
        bail!("this needs a yes: run it again with --yes (or in a terminal to be asked)");
    }
    eprint!("{question} [y/N] ");
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    Ok(matches!(line.trim().to_lowercase().as_str(), "y" | "yes"))
}

fn arch_name() -> &'static str {
    std::env::consts::ARCH
}

/// Put `new` in place of `old`. Unix replaces a running program by renaming over it; Windows cannot, but can rename it aside first (the leftover `.old`
/// is deleted by the next run). Returns the aside copy, if any, so a failure further on can put everything back.
fn swap(new: &Path, old: &Path) -> Result<Option<PathBuf>> {
    if cfg!(windows) {
        let aside = old.with_extension("exe.old");
        let _ = std::fs::remove_file(&aside);
        std::fs::rename(old, &aside).with_context(|| format!("moving {} aside", old.display()))?;
        if let Err(e) = std::fs::rename(new, old) {
            let _ = std::fs::rename(&aside, old);
            return Err(anyhow!("could not put the new {} in place: {e}", old.display()));
        }
        Ok(Some(aside))
    } else {
        std::fs::rename(new, old).with_context(|| format!("replacing {}", old.display()))?;
        Ok(None)
    }
}

/// Delete the `.old` copies a Windows update left beside this program.
pub fn cleanup_old() {
    if !cfg!(windows) {
        return;
    }
    if let Some(dir) = std::env::current_exe().ok().and_then(|e| e.parent().map(Path::to_path_buf)) {
        for n in ["periwire.exe.old", "periwire-gui.exe.old"] {
            let _ = std::fs::remove_file(dir.join(n));
        }
    }
}

/// How this copy can be updated, as `{kind, supported, reason?, replaces}`. `kind`: folder, appimage, package or bundle.
pub fn support() -> serde_json::Value {
    let exe = std::env::current_exe().unwrap_or_default();
    let appimage = std::env::var_os("APPIMAGE").map(PathBuf::from).filter(|p| p.is_file());
    match target(&exe, appimage) {
        Target::Folder { files } => json!({"kind": "folder", "supported": true, "replaces": files.iter().map(|f| f.display().to_string()).collect::<Vec<_>>()}),
        Target::AppImage(p) => json!({"kind": "appimage", "supported": true, "replaces": [p.display().to_string()]}),
        Target::Elsewhere(why) => json!({"kind": if why.contains("app bundle") { "bundle" } else { "package" }, "supported": false, "reason": why, "replaces": []}),
    }
}

/// `periwire self-update --status`
pub fn print_status(json_out: bool) -> Result<i32> {
    let s = support();
    if json_out {
        println!("{s}");
    } else if s["supported"] == json!(true) {
        println!(
            "this copy can update itself ({}): `periwire self-update` replaces {}",
            s["kind"].as_str().unwrap_or(""),
            s["replaces"].as_array().map(|a| a.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>().join(", ")).unwrap_or_default()
        );
    } else {
        println!("this copy updates another way: {}", s["reason"].as_str().unwrap_or(""));
    }
    Ok(0)
}

pub fn self_update(yes: bool, dry_run: bool, to: Option<String>, force: bool, json_out: bool) -> Result<i32> {
    let current = env!("CARGO_PKG_VERSION");
    let exe = std::env::current_exe().context("cannot tell where this program is")?;
    let appimage = std::env::var_os("APPIMAGE").map(PathBuf::from).filter(|p| p.is_file());
    let tgt = target(&exe, appimage);
    if let Target::Elsewhere(why) = &tgt {
        bail!("{why}");
    }
    let version = match to {
        Some(v) => v.trim_start_matches(['v', 'V']).to_string(),
        None => {
            let info = crate::update::check_blocking(current)?;
            let latest = info["latest"].as_str().unwrap_or("").to_string();
            if info["newer"] != json!(true) && !force {
                if json_out {
                    println!("{}", json!({"updated": false, "current": current, "latest": latest}));
                } else {
                    println!("periwire {current} is the latest release.");
                }
                return Ok(0);
            }
            latest
        }
    };
    let kind = if matches!(tgt, Target::AppImage(_)) { Kind::AppImage } else { Kind::Archive };
    let asset = asset_name(&version, std::env::consts::OS, arch_name(), kind)?;
    let base = fetch::base_url(&version)?;
    let places: Vec<String> = match &tgt {
        Target::Folder { files } => files.iter().map(|f| f.display().to_string()).collect(),
        Target::AppImage(p) => vec![p.display().to_string()],
        Target::Elsewhere(_) => vec![],
    };
    if !json_out {
        println!("update periwire {current} -> {version}");
        println!("  download {base}/{asset} (and check it against {asset}.sha256)");
        places.iter().for_each(|p| println!("  replace {p}"));
    }
    if dry_run {
        return Ok(0);
    }
    if !yes && !ask("Go ahead?")? {
        bail!("cancelled; nothing was changed");
    }

    install_from(&base, &asset, &tgt)?;
    crate::selfinstall::log(&format!("self-update: {current} -> {version} ({asset}) replaced {}", places.join(", ")));
    if json_out {
        println!("{}", json!({"updated": true, "from": current, "to": version, "replaced": places}));
    } else {
        println!("updated periwire {current} -> {version}");
        println!("Restart the daemon and the app to use it (`periwire stop`, then start them again).");
        println!("Check where it came from:  gh attestation verify <the downloaded file> --repo ImYourBoyRoy/periwire   (the archive was checked against its SHA-256)");
    }
    Ok(0)
}

/// Download `asset` from `base`, check it against its checksum, and put its programs in place of the current ones. Nothing is changed unless the checksum matches.
pub fn install_from(base: &str, asset: &str, tgt: &Target) -> Result<()> {
    let archive = fetch::get(&format!("{base}/{asset}"), 250 << 20).with_context(|| format!("downloading {asset}"))?;
    let want = match fetch::get(&format!("{base}/{asset}.sha256"), 4096) {
        Ok(b) => expected_hash(&String::from_utf8_lossy(&b), asset),
        Err(_) => fetch::get(&format!("{base}/SHA256SUMS.txt"), 1 << 20).ok().and_then(|b| expected_hash(&String::from_utf8_lossy(&b), asset)),
    }
    .ok_or_else(|| anyhow!("the release has no checksum for {asset}; refusing to install it"))?;
    let got = sha256_hex(&archive);
    if got != want {
        bail!("the download does not match its checksum (expected {want}, got {got}); nothing was changed");
    }

    let mut swapped: Vec<(PathBuf, Option<PathBuf>)> = vec![];
    let result: Result<()> = (|| {
        match tgt {
            Target::AppImage(path) => {
                let new = path.with_extension("AppImage.new");
                std::fs::write(&new, &archive)?;
                set_executable(&new)?;
                swap(&new, path)?;
            }
            Target::Folder { files } => {
                let dir = files[0].parent().unwrap_or(Path::new(".")).to_path_buf();
                let names: Vec<String> = files.iter().filter_map(|f| f.file_name().map(|n| n.to_string_lossy().into_owned())).collect();
                let extracted = archive::extract(&archive, asset, &names)?; // name -> bytes
                for (name, bytes) in &extracted {
                    let new = dir.join(format!("{name}.new"));
                    std::fs::write(&new, bytes).with_context(|| format!("writing {}", new.display()))?;
                    set_executable(&new)?;
                }
                for (name, _) in &extracted {
                    let (new, old) = (dir.join(format!("{name}.new")), dir.join(name));
                    let aside = swap(&new, &old)?;
                    swapped.push((old, aside));
                }
            }
            Target::Elsewhere(_) => unreachable!(),
        }
        Ok(())
    })();
    if let Err(e) = result {
        for (old, aside) in swapped.into_iter().rev() {
            if let Some(a) = aside {
                let _ = std::fs::rename(&a, &old); // put the previous program back
            }
        }
        return Err(e.context("the update failed and the previous version was put back where possible"));
    }
    Ok(())
}

fn set_executable(p: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755))?;
    }
    let _ = p;
    Ok(())
}

#[cfg(test)]
mod tests;
