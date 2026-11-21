//! `periwire self-install` / `self-uninstall`: make the command line reachable from every shell, reversibly (after enchi's installer).
//!
//! * Windows: the folder holding `periwire.exe` goes into the user's persistent PATH (PowerShell and cmd both read it).
//! * Linux and macOS: `~/.local/bin/periwire` links to the program (for an AppImage, to a copy, since its mount point changes at every
//!   start) and one delimited block per shell profile (bash, zsh, fish) puts that folder on PATH and loads completions.
//! * A package that already provides `periwire` on the PATH is left alone.
//!
//! What was done is written to `install-receipt.json` in the config folder, and `self-uninstall` removes exactly that.
mod profile;
mod user_path;

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};

pub use profile::Shell;

/// Where things are, passed in so that the plan can be tested without touching the machine.
pub struct Env {
    pub exe: PathBuf,
    pub home: PathBuf,
    pub data_dir: PathBuf,
    pub login_shell: Option<Shell>,
    pub macos: bool,
    pub windows: bool,
    pub appimage: bool,
    /// Another `periwire` already on PATH (not ours): a package, or a copy somebody installed.
    pub found_elsewhere: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Plan {
    /// The folder that has to be on PATH.
    pub command_dir: Option<PathBuf>,
    /// `~/.local/bin/periwire` -> target; `copy_to` is where an AppImage's program is copied first.
    pub link: Option<(PathBuf, PathBuf)>,
    pub copy_to: Option<PathBuf>,
    pub profiles: Vec<profile::Target>,
    pub windows_path: bool,
    /// Why there is nothing to do, when there is not.
    pub nothing: Option<String>,
}

pub fn is_packaged(exe: &Path) -> bool {
    ["/usr/", "/opt/", "/snap/", "/nix/store/", "/var/lib/flatpak/", "/app/"].iter().any(|p| exe.starts_with(p))
}

pub fn plan(e: &Env, with_shells: bool) -> Plan {
    let empty = |why: &str| Plan { command_dir: None, link: None, copy_to: None, profiles: vec![], windows_path: false, nothing: Some(why.to_string()) };
    if e.windows {
        let dir = e.exe.parent().map(Path::to_path_buf);
        return Plan { command_dir: dir, link: None, copy_to: None, profiles: vec![], windows_path: true, nothing: None };
    }
    if is_packaged(&e.exe) && !e.appimage {
        return empty("this copy belongs to a package, which already puts `periwire` on your PATH");
    }
    if e.found_elsewhere {
        return empty("another `periwire` is already on your PATH; not shadowing it");
    }
    let bin = e.home.join(".local/bin");
    let (target, copy_to) = if e.appimage {
        let copy = e.data_dir.join("bin/periwire");
        (copy.clone(), Some(copy))
    } else {
        (e.exe.clone(), None)
    };
    let profiles = if with_shells { profile::targets(&e.home, e.login_shell, e.macos) } else { vec![] };
    Plan { command_dir: Some(bin.clone()), link: Some((bin.join("periwire"), target)), copy_to, profiles, windows_path: false, nothing: None }
}

/// What a run changed, so that `self-uninstall` can undo exactly that.
#[derive(Serialize, Deserialize, Default, Debug)]
pub struct Receipt {
    pub version: String,
    pub link: Option<String>,
    pub link_target: Option<String>,
    pub copy: Option<String>,
    pub profiles: Vec<String>,
    pub windows_path: Option<String>,
}

fn receipt_path() -> PathBuf {
    crate::identity::home().join("install-receipt.json")
}

fn read_receipt() -> Receipt {
    std::fs::read_to_string(receipt_path()).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

fn write_if_different(path: &Path, bytes: &[u8]) -> std::io::Result<bool> {
    if std::fs::read(path).map(|old| old == bytes).unwrap_or(false) {
        return Ok(false);
    }
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    let tmp = path.with_extension("part");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)?;
    Ok(true)
}

/// Make the link (and the copy for an AppImage). Returns a line for the report.
fn make_link(link: &Path, target: &Path, exe: &Path, copy_to: &Option<PathBuf>) -> Result<String> {
    if let Some(copy) = copy_to {
        let bytes = std::fs::read(exe).with_context(|| format!("reading {}", exe.display()))?;
        if write_if_different(copy, &bytes)? {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(copy, std::fs::Permissions::from_mode(0o755))?;
            }
        }
    }
    #[cfg(unix)]
    {
        match std::fs::symlink_metadata(link) {
            Ok(m) if m.file_type().is_symlink() => {
                if std::fs::read_link(link)? == target {
                    return Ok(format!("{} already points at {}", link.display(), target.display()));
                }
                std::fs::remove_file(link)?; // ours or stale: follow the program to where it is now
            }
            Ok(_) => bail!("{} is a file that is not a link (yours?); left alone", link.display()),
            Err(_) => std::fs::create_dir_all(link.parent().unwrap_or(Path::new(".")))?,
        }
        std::os::unix::fs::symlink(target, link)?;
        Ok(format!("linked {} -> {}", link.display(), target.display()))
    }
    #[cfg(not(unix))]
    {
        let _ = (link, target);
        Ok(String::new())
    }
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

fn describe(p: &Plan) -> Vec<String> {
    let mut v = vec![];
    if let Some((l, t)) = &p.link {
        v.push(format!("link {} -> {}", l.display(), t.display()));
    }
    if let Some(c) = &p.copy_to {
        v.push(format!("copy the program to {}", c.display()));
    }
    for t in &p.profiles {
        v.push(format!("add a PATH and completions block to {} ({})", t.path.display(), t.shell.name()));
    }
    if p.windows_path {
        if let Some(d) = &p.command_dir {
            v.push(format!("add {} to your user PATH", d.display()));
        }
    }
    v
}

fn detect_env() -> Result<Env> {
    let exe = std::env::current_exe().context("cannot tell where this program is")?;
    let home = dirs::home_dir().context("cannot find your home folder")?;
    let data_dir = dirs::data_dir().map(|d| d.join("periwire")).unwrap_or_else(|| home.join(".local/share/periwire"));
    let bin = home.join(".local/bin");
    let found_elsewhere = std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).filter(|d| *d != bin).any(|d| d.join("periwire").is_file() && d.join("periwire") != exe));
    Ok(Env {
        exe,
        home,
        data_dir,
        login_shell: std::env::var("SHELL").ok().and_then(|s| Shell::from_path(&s)),
        macos: cfg!(target_os = "macos"),
        windows: cfg!(windows),
        appimage: std::env::var_os("APPIMAGE").is_some(),
        found_elsewhere,
    })
}

/// `periwire self-install [--yes] [--dry-run] [--no-shell]`
pub fn self_install(yes: bool, dry_run: bool, no_shell: bool, quiet: bool, json_out: bool) -> Result<i32> {
    let env = detect_env()?;
    let plan = plan(&env, !no_shell);
    if let Some(why) = &plan.nothing {
        if json_out {
            println!("{}", json!({"changed": false, "reason": why}));
        } else if !quiet {
            println!("nothing to do: {why}");
        }
        return Ok(0);
    }
    let steps = describe(&plan);
    if dry_run || !yes {
        if !quiet && !json_out {
            println!("self-install would:");
            steps.iter().for_each(|s| println!("  - {s}"));
        }
        if dry_run {
            if json_out {
                println!("{}", json!({"dry_run": true, "steps": steps}));
            }
            return Ok(0);
        }
        if !ask("Go ahead?")? {
            bail!("cancelled; nothing was changed");
        }
    }
    let mut receipt = read_receipt();
    receipt.version = env!("CARGO_PKG_VERSION").to_string();
    let mut report = vec![];
    if let Some((link, target)) = &plan.link {
        match make_link(link, target, &env.exe, &plan.copy_to) {
            Ok(line) => {
                report.push(line);
                receipt.link = Some(link.display().to_string());
                receipt.link_target = Some(target.display().to_string());
                receipt.copy = plan.copy_to.as_ref().map(|c| c.display().to_string());
            }
            Err(e) => report.push(format!("could not link: {e:#}")),
        }
    }
    if let Some(dir) = &plan.command_dir {
        for t in &plan.profiles {
            match profile::apply(t, dir) {
                Ok(profile::Change::Unchanged) => report.push(format!("{} already set up", t.path.display())),
                Ok(_) => {
                    report.push(format!("updated {}", t.path.display()));
                    let p = t.path.display().to_string();
                    if !receipt.profiles.contains(&p) {
                        receipt.profiles.push(p);
                    }
                }
                Err(e) => report.push(format!("could not edit {}: {e}", t.path.display())),
            }
        }
        if plan.windows_path {
            match user_path::add(dir) {
                Ok(changed) => {
                    report.push(if changed { format!("added {} to your user PATH", dir.display()) } else { format!("{} is already on your user PATH", dir.display()) });
                    receipt.windows_path = Some(dir.display().to_string());
                }
                Err(e) => report.push(format!("could not change your PATH: {e}")),
            }
        }
    }
    if let Some(d) = receipt_path().parent() {
        std::fs::create_dir_all(d)?;
    }
    write_if_different(&receipt_path(), serde_json::to_string_pretty(&receipt)?.as_bytes())?;
    if json_out {
        println!("{}", json!({"changed": true, "report": report, "receipt": receipt}));
    } else if !quiet {
        report.iter().for_each(|l| println!("{l}"));
        println!(
            "{} Undo with `periwire self-uninstall`.",
            if cfg!(windows) { "Open a new PowerShell or cmd window to use `periwire` there." } else { "Open a new terminal to use `periwire` there (or run `hash -r`)." }
        );
    }
    Ok(0)
}

/// `periwire self-uninstall [--yes]`: undo what the receipt lists. The program itself and your identity, trust list and config stay.
pub fn self_uninstall(yes: bool, json_out: bool) -> Result<i32> {
    let r = read_receipt();
    let mut steps = vec![];
    if let Some(l) = &r.link {
        steps.push(format!("remove the link {l}"));
    }
    if let Some(c) = &r.copy {
        steps.push(format!("remove the copy {c}"));
    }
    r.profiles.iter().for_each(|p| steps.push(format!("remove the periwire block from {p}")));
    if let Some(d) = &r.windows_path {
        steps.push(format!("remove {d} from your user PATH"));
    }
    if steps.is_empty() {
        println!("nothing to undo: self-install has not changed anything on this computer.");
        return Ok(0);
    }
    if !json_out {
        println!("self-uninstall would:");
        steps.iter().for_each(|s| println!("  - {s}"));
    }
    if !yes && !ask("Go ahead?")? {
        bail!("cancelled; nothing was changed");
    }
    let mut report = vec![];
    if let Some(l) = &r.link {
        let link = Path::new(l);
        let ours = std::fs::read_link(link).ok().is_some_and(|t| r.link_target.as_deref() == Some(&t.display().to_string()));
        if ours {
            report.push(std::fs::remove_file(link).map(|_| format!("removed {l}")).unwrap_or_else(|e| format!("could not remove {l}: {e}")));
        } else if link.exists() || std::fs::symlink_metadata(link).is_ok() {
            report.push(format!("{l} no longer points where self-install put it; left alone"));
        }
    }
    if let Some(c) = &r.copy {
        let _ = std::fs::remove_file(c);
        report.push(format!("removed {c}"));
    }
    for p in &r.profiles {
        report.push(match profile::strip(Path::new(p)) {
            Ok(true) => format!("removed the block from {p}"),
            Ok(false) => format!("{p} had no periwire block"),
            Err(e) => format!("could not edit {p}: {e}"),
        });
    }
    if let Some(d) = &r.windows_path {
        report.push(match user_path::remove(Path::new(d)) {
            Ok(true) => format!("removed {d} from your user PATH"),
            Ok(false) => format!("{d} was not on your user PATH"),
            Err(e) => format!("could not change your PATH: {e}"),
        });
    }
    let _ = std::fs::remove_file(receipt_path());
    if json_out {
        println!("{}", json!({"report": report}));
    } else {
        report.iter().for_each(|l| println!("{l}"));
        println!("The program itself, your identity, trust list and settings are untouched. Remove the program with your package manager or by deleting it.");
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(exe: &str) -> Env {
        Env {
            exe: exe.into(),
            home: "/home/x".into(),
            data_dir: "/home/x/.local/share/periwire".into(),
            login_shell: Some(Shell::Bash),
            macos: false,
            windows: false,
            appimage: false,
            found_elsewhere: false,
        }
    }

    #[test]
    fn a_copy_that_was_not_installed_gets_a_link_in_local_bin_and_shell_blocks() {
        let p = plan(&env("/home/x/pw/periwire"), true);
        assert_eq!(p.command_dir.as_deref(), Some(Path::new("/home/x/.local/bin")));
        assert_eq!(p.link, Some(("/home/x/.local/bin/periwire".into(), "/home/x/pw/periwire".into())));
        assert!(p.copy_to.is_none() && !p.windows_path && p.nothing.is_none());
        assert!(plan(&env("/home/x/pw/periwire"), false).profiles.is_empty(), "--no-shell leaves the profiles alone");
    }

    #[test]
    fn an_appimage_is_copied_out_because_its_mount_point_changes() {
        let mut e = env("/tmp/.mount_PeriWiXYZ/usr/bin/periwire");
        e.appimage = true;
        let p = plan(&e, false);
        let copy = PathBuf::from("/home/x/.local/share/periwire/bin/periwire");
        assert_eq!(p.copy_to, Some(copy.clone()));
        assert_eq!(p.link.unwrap().1, copy, "the link points at the copy, not the temporary mount");
    }

    #[test]
    fn packages_and_other_copies_are_left_alone() {
        assert!(plan(&env("/usr/bin/periwire"), true).nothing.is_some());
        let mut e = env("/home/x/pw/periwire");
        e.found_elsewhere = true;
        assert!(plan(&e, true).nothing.unwrap().contains("not shadowing"));
    }

    #[test]
    fn windows_uses_the_user_path_and_no_profile() {
        let mut e = env(r"C:\Users\x\AppData\Local\PeriWire\periwire.exe");
        e.windows = true;
        e.exe = PathBuf::from("C:/Users/x/AppData/Local/PeriWire/periwire.exe");
        let p = plan(&e, true);
        assert!(p.windows_path && p.link.is_none() && p.profiles.is_empty());
        assert_eq!(p.command_dir.as_deref(), Some(Path::new("C:/Users/x/AppData/Local/PeriWire")));
    }

    #[test]
    #[cfg(unix)]
    fn linking_follows_a_moved_program_and_never_replaces_somebody_elses_file() {
        let d = std::env::temp_dir().join(format!("pw-selfinstall-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("a")).unwrap();
        std::fs::create_dir_all(d.join("b")).unwrap();
        let (a, b, link) = (d.join("a/periwire"), d.join("b/periwire"), d.join("bin/periwire"));
        std::fs::write(&a, b"v1").unwrap();
        std::fs::write(&b, b"v1").unwrap();
        assert!(make_link(&link, &a, &a, &None).unwrap().starts_with("linked"));
        assert!(make_link(&link, &a, &a, &None).unwrap().contains("already points"));
        assert!(make_link(&link, &b, &b, &None).unwrap().starts_with("linked"), "a moved program is followed");
        assert_eq!(std::fs::read_link(&link).unwrap(), b);
        std::fs::remove_file(&link).unwrap();
        std::fs::write(&link, b"mine").unwrap();
        assert!(make_link(&link, &a, &a, &None).is_err());
        assert_eq!(std::fs::read(&link).unwrap(), b"mine");
        let copy = d.join("data/bin/periwire");
        std::fs::remove_file(&link).unwrap();
        make_link(&link, &copy, &a, &Some(copy.clone())).unwrap();
        assert_eq!(std::fs::read(&copy).unwrap(), b"v1");
        let _ = std::fs::remove_dir_all(&d);
    }
}
