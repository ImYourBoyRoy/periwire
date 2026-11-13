//! Puts PeriWire in the Linux app launcher when it was not installed from a package.
//!
//! The `.deb` brings its own launcher entry. An AppImage, an unpacked tarball or a copy in the home folder does not, and the person
//! should not have to run a command to find the app again. So the first time the app starts from one of those it writes a launcher
//! entry and its icon into the user's own data folder (`~/.local/share`), nothing system-wide. The entry names the program with
//! `TryExec`, so if the file is later deleted the launcher hides itself instead of lingering. Packages (programs under `/usr`, `/opt`,
//! a snap, a flatpak, the Nix store) are left alone, and an entry written earlier is removed once a package provides one.
//! Opt out with `PERIWIRE_NO_LAUNCHER=1`.
use std::io;
use std::path::{Path, PathBuf};

const ICON: &[u8] = include_bytes!("../icons/256x256.png");
const MARKER: &str = "X-PeriWire-Self-Registered=true";

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The program belongs to a package that brings its own entry (and ours, if any, was removed).
    Packaged,
    /// Somebody installed an entry system-wide already.
    SystemEntry,
    Disabled,
    Written,
    Unchanged,
}

/// What the launcher should start: the AppImage file itself (the program inside it lives in a temporary mount), else this program.
pub fn target_exe() -> Option<PathBuf> {
    std::env::var_os("APPIMAGE").map(PathBuf::from).filter(|p| p.is_file()).or_else(|| std::env::current_exe().ok())
}

pub fn is_packaged(exe: &Path) -> bool {
    ["/usr/", "/opt/", "/snap/", "/nix/store/", "/var/lib/flatpak/", "/app/"].iter().any(|p| exe.starts_with(p))
}

/// A path as the `Exec` key wants it: in double quotes, with the characters the specification reserves escaped.
fn quote(p: &str) -> String {
    let mut s = String::from("\"");
    for c in p.chars() {
        match c {
            '"' | '`' | '$' | '\\' => {
                s.push('\\');
                s.push(c);
            }
            '%' => s.push_str("%%"),
            _ => s.push(c),
        }
    }
    s.push('"');
    s
}

pub fn entry_text(exe: &Path, icon: &Path) -> String {
    let e = exe.display().to_string();
    format!(
        "[Desktop Entry]\nType=Application\nName=PeriWire\nGenericName=USB over the network\nComment=Share USB devices over the network\nKeywords=usb;network;share;remote;device;usbip;virtualhere;ipad;\nCategories=Utility;\nExec={}\nTryExec={e}\nIcon={}\nStartupWMClass=periwire-gui\nTerminal=false\nStartupNotify=false\n{MARKER}\n",
        quote(&e),
        icon.display()
    )
}

fn write_if_different(path: &Path, bytes: &[u8]) -> io::Result<bool> {
    if std::fs::read(path).map(|old| old == bytes).unwrap_or(false) {
        return Ok(false);
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, bytes)?;
    Ok(true)
}

/// The whole decision and the writing, with every location passed in so that it can be tested in a scratch folder.
pub fn register(data_home: &Path, exe: &Path, system_entry: bool, opt_out: bool) -> io::Result<Outcome> {
    let entry = data_home.join("applications/periwire.desktop");
    let icon = data_home.join("icons/hicolor/256x256/apps/periwire.png");
    let ours = std::fs::read_to_string(&entry).map(|t| t.contains(MARKER)).unwrap_or(false);
    if opt_out {
        return Ok(Outcome::Disabled);
    }
    if is_packaged(exe) || system_entry {
        if ours {
            let _ = std::fs::remove_file(&entry);
            let _ = std::fs::remove_file(&icon);
        }
        return Ok(if is_packaged(exe) { Outcome::Packaged } else { Outcome::SystemEntry });
    }
    if entry.exists() && !ours {
        return Ok(Outcome::Unchanged); // somebody made their own entry: leave it
    }
    let a = write_if_different(&icon, ICON)?;
    let b = write_if_different(&entry, entry_text(exe, &icon).as_bytes())?;
    Ok(if a || b { Outcome::Written } else { Outcome::Unchanged })
}

fn data_home() -> Option<PathBuf> {
    std::env::var_os("XDG_DATA_HOME").filter(|v| !v.is_empty()).map(PathBuf::from).or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
}

fn system_entry_exists() -> bool {
    let dirs = std::env::var("XDG_DATA_DIRS").ok().filter(|v| !v.is_empty()).unwrap_or_else(|| "/usr/local/share:/usr/share".into());
    dirs.split(':').any(|d| Path::new(d).join("applications/periwire.desktop").exists())
}

/// Called at start-up; problems are only reported, never fatal.
pub fn ensure() -> Option<Outcome> {
    let (exe, home) = (target_exe()?, data_home()?);
    let opt_out = std::env::var_os("PERIWIRE_NO_LAUNCHER").map(|v| !v.is_empty() && v != "0").unwrap_or(false);
    match register(&home, &exe, system_entry_exists(), opt_out) {
        Ok(o) => Some(o),
        Err(e) => {
            eprintln!("periwire: could not add the app to the launcher: {e}");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("pw-launcher-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn a_portable_copy_registers_once_and_follows_a_move() {
        let home = scratch("move");
        let a = Path::new("/home/x/Apps/PeriWire.AppImage");
        assert_eq!(register(&home, a, false, false).unwrap(), Outcome::Written);
        let text = std::fs::read_to_string(home.join("applications/periwire.desktop")).unwrap();
        assert!(text.contains("Exec=\"/home/x/Apps/PeriWire.AppImage\"") && text.contains("TryExec=/home/x/Apps/PeriWire.AppImage"));
        assert!(text.contains("StartupWMClass=periwire-gui") && text.contains(MARKER));
        assert!(home.join("icons/hicolor/256x256/apps/periwire.png").metadata().unwrap().len() > 1000);
        assert_eq!(register(&home, a, false, false).unwrap(), Outcome::Unchanged, "starting again changes nothing");
        let b = Path::new("/home/x/Downloads/PeriWire.AppImage");
        assert_eq!(register(&home, b, false, false).unwrap(), Outcome::Written, "a moved file is followed");
        assert!(std::fs::read_to_string(home.join("applications/periwire.desktop")).unwrap().contains("/Downloads/"));
    }

    #[test]
    fn a_package_provides_its_own_entry_and_replaces_ours() {
        let home = scratch("pkg");
        register(&home, Path::new("/home/x/pw/periwire-gui"), false, false).unwrap();
        assert!(home.join("applications/periwire.desktop").exists());
        assert_eq!(register(&home, Path::new("/usr/bin/periwire-gui"), false, false).unwrap(), Outcome::Packaged);
        assert!(!home.join("applications/periwire.desktop").exists(), "ours is removed so the menu does not list it twice");
        assert!(!home.join("icons/hicolor/256x256/apps/periwire.png").exists());
        assert_eq!(register(&home, Path::new("/home/x/pw/periwire-gui"), true, false).unwrap(), Outcome::SystemEntry);
        assert!(!home.join("applications/periwire.desktop").exists(), "nothing is written when a system entry exists");
    }

    #[test]
    fn opt_out_and_a_hand_made_entry_are_respected() {
        let home = scratch("opt");
        let exe = Path::new("/home/x/pw/periwire-gui");
        assert_eq!(register(&home, exe, false, true).unwrap(), Outcome::Disabled);
        assert!(!home.join("applications").exists());
        std::fs::create_dir_all(home.join("applications")).unwrap();
        std::fs::write(home.join("applications/periwire.desktop"), "[Desktop Entry]\nName=Mine\n").unwrap();
        assert_eq!(register(&home, exe, false, false).unwrap(), Outcome::Unchanged);
        assert!(std::fs::read_to_string(home.join("applications/periwire.desktop")).unwrap().contains("Name=Mine"));
    }

    #[test]
    fn exec_quoting_follows_the_specification() {
        assert_eq!(quote("/a b/c"), "\"/a b/c\"");
        assert_eq!(quote("/a\"b$c`d\\e%f"), "\"/a\\\"b\\$c\\`d\\\\e%%f\"");
    }

    #[test]
    fn what_counts_as_packaged() {
        for p in ["/usr/bin/periwire-gui", "/opt/PeriWire/periwire-gui", "/snap/periwire/1/periwire-gui", "/nix/store/x/bin/periwire-gui"] {
            assert!(is_packaged(Path::new(p)), "{p}");
        }
        for p in ["/home/x/PeriWire.AppImage", "/tmp/.mount_PeriWiabc/usr/bin/periwire-gui", "/home/x/usr/bin/periwire-gui"] {
            assert!(!is_packaged(Path::new(p)), "{p}");
        }
    }
}
