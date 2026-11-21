//! Shell integration after enchi's `profile.rs`: one delimited block per profile file that puts the command's folder on `PATH` and loads
//! completions. The block can be updated or removed without touching anything else in the file. Only POSIX shells need this: Windows
//! (PowerShell and cmd alike) reads the persistent user PATH instead (see `user_path`).
use std::io;
use std::path::{Path, PathBuf};

pub const BEGIN: &str = "# >>> periwire >>>";
pub const END: &str = "# <<< periwire <<<";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shell {
    Bash,
    Zsh,
    Fish,
}

impl Shell {
    pub fn name(self) -> &'static str {
        match self {
            Shell::Bash => "bash",
            Shell::Zsh => "zsh",
            Shell::Fish => "fish",
        }
    }
    pub fn from_path(p: &str) -> Option<Self> {
        match Path::new(p).file_name()?.to_str()? {
            "bash" => Some(Shell::Bash),
            "zsh" => Some(Shell::Zsh),
            "fish" => Some(Shell::Fish),
            _ => None,
        }
    }
}

/// A shell profile this tool may edit, and whether it is the whole file's business (fish's `conf.d` file is ours alone).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub shell: Shell,
    pub path: PathBuf,
    pub whole_file: bool,
}

/// Which profile files to put the block in: those that already exist, plus the login shell's own file (created if missing). Fish uses a file of its own
/// in `conf.d` when fish is set up at all.
pub fn targets(home: &Path, login_shell: Option<Shell>, macos: bool) -> Vec<Target> {
    let mut v = vec![];
    let bashrc = home.join(".bashrc");
    let bash_profile = home.join(".bash_profile");
    let zshrc = home.join(".zshrc");
    if bashrc.exists() || login_shell == Some(Shell::Bash) {
        v.push(Target { shell: Shell::Bash, path: bashrc, whole_file: false });
    }
    if macos && bash_profile.exists() {
        v.push(Target { shell: Shell::Bash, path: bash_profile, whole_file: false });
        // macOS terminals start bash as a login shell
    }
    if zshrc.exists() || login_shell == Some(Shell::Zsh) || (macos && login_shell.is_none()) {
        v.push(Target { shell: Shell::Zsh, path: zshrc, whole_file: false });
    }
    let fish_dir = home.join(".config/fish");
    if fish_dir.exists() || login_shell == Some(Shell::Fish) {
        v.push(Target { shell: Shell::Fish, path: fish_dir.join("conf.d/periwire.fish"), whole_file: true });
    }
    v
}

/// The text between the markers, markers included, ending in a newline.
pub fn render(shell: Shell, bin_dir: &Path) -> String {
    let dir = bin_dir.display().to_string();
    let q = dir.replace('\'', "'\\''");
    let body = match shell {
        Shell::Bash => format!(
            "# Added by `periwire self-install`; `periwire self-uninstall` removes it.\ncase \":$PATH:\" in *\":{dir}:\"*) ;; *) export PATH='{q}':\"$PATH\" ;; esac\nif command -v periwire >/dev/null 2>&1; then eval \"$(periwire completions bash)\"; fi\n"
        ),
        Shell::Zsh => format!(
            "# Added by `periwire self-install`; `periwire self-uninstall` removes it.\ncase \":$PATH:\" in *\":{dir}:\"*) ;; *) export PATH='{q}':\"$PATH\" ;; esac\nif command -v periwire >/dev/null 2>&1 && (( $+functions[compdef] )); then eval \"$(periwire completions zsh)\"; fi\n"
        ),
        Shell::Fish => format!(
            "# Added by `periwire self-install`; `periwire self-uninstall` removes it.\nif not contains -- '{q}' $PATH\n    set -gx PATH '{q}' $PATH\nend\nif command -sq periwire\n    periwire completions fish | source\nend\n"
        ),
    };
    format!("{BEGIN}\n{body}{END}\n")
}

#[derive(Debug, PartialEq, Eq)]
pub enum Change {
    Added,
    Updated,
    Unchanged,
}

/// Insert or replace the block in `existing`.
pub fn upsert(existing: &str, block: &str) -> (String, Change) {
    match (existing.find(BEGIN), existing.find(END)) {
        (Some(a), Some(b)) if b > a => {
            let end = b + END.len();
            let end = if existing[end..].starts_with('\n') { end + 1 } else { end };
            let new = format!("{}{}{}", &existing[..a], block, &existing[end..]);
            let change = if new == existing { Change::Unchanged } else { Change::Updated };
            (new, change)
        }
        _ => {
            let mut s = existing.to_string();
            if !s.is_empty() && !s.ends_with('\n') {
                s.push('\n');
            }
            if !s.is_empty() {
                s.push('\n');
            }
            s.push_str(block);
            (s, Change::Added)
        }
    }
}

/// Take the block out (and the blank line that separated it). Returns whether there was one.
pub fn remove(existing: &str) -> (String, bool) {
    match (existing.find(BEGIN), existing.find(END)) {
        (Some(a), Some(b)) if b > a => {
            let end = b + END.len();
            let end = if existing[end..].starts_with('\n') { end + 1 } else { end };
            let before = existing[..a].strip_suffix("\n\n").map(|s| format!("{s}\n")).unwrap_or_else(|| existing[..a].to_string());
            (format!("{before}{}", &existing[end..]), true)
        }
        _ => (existing.to_string(), false),
    }
}

pub fn has_block(existing: &str) -> bool {
    matches!((existing.find(BEGIN), existing.find(END)), (Some(a), Some(b)) if b > a)
}

/// Write the block into `t`. Returns what changed.
pub fn apply(t: &Target, bin_dir: &Path) -> io::Result<Change> {
    let block = render(t.shell, bin_dir);
    let existing = std::fs::read_to_string(&t.path).unwrap_or_default();
    let (new, change) = if t.whole_file && !has_block(&existing) && !existing.is_empty() {
        return Ok(Change::Unchanged); // a file of that name that is not ours: leave it
    } else if t.whole_file {
        (
            block,
            if existing.is_empty() {
                Change::Added
            } else if existing == render(t.shell, bin_dir) {
                Change::Unchanged
            } else {
                Change::Updated
            },
        )
    } else {
        upsert(&existing, &block)
    };
    if change != Change::Unchanged {
        if let Some(d) = t.path.parent() {
            std::fs::create_dir_all(d)?;
        }
        std::fs::write(&t.path, new)?;
    }
    Ok(change)
}

/// Remove our block from `path`; a file that held nothing else is deleted. Returns whether anything was removed.
pub fn strip(path: &Path) -> io::Result<bool> {
    let Ok(existing) = std::fs::read_to_string(path) else { return Ok(false) };
    let (new, had) = remove(&existing);
    if !had {
        return Ok(false);
    }
    if new.trim().is_empty() {
        std::fs::remove_file(path)?;
    } else {
        std::fs::write(path, new)?;
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(n: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("pw-profile-{n}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn the_block_is_added_once_updated_in_place_and_removed_cleanly() {
        let before = "export A=1\nalias ll='ls -l'\n";
        let b1 = render(Shell::Bash, Path::new("/home/x/.local/bin"));
        let (once, c) = upsert(before, &b1);
        assert_eq!(c, Change::Added);
        assert!(once.starts_with(before) && once.contains(BEGIN) && once.contains("/home/x/.local/bin"));
        assert_eq!(upsert(&once, &b1).1, Change::Unchanged, "twice changes nothing");
        let b2 = render(Shell::Bash, Path::new("/opt/pw/bin"));
        let (moved, c) = upsert(&once, &b2);
        assert_eq!(c, Change::Updated);
        assert_eq!(moved.matches(BEGIN).count(), 1);
        assert!(moved.contains("/opt/pw/bin") && !moved.contains("/home/x/.local/bin"));
        let (gone, had) = remove(&moved);
        assert!(had);
        assert_eq!(gone, before, "everything else in the file is exactly as it was");
        assert!(!remove(&gone).1);
    }

    #[test]
    fn a_path_with_a_quote_is_quoted_safely() {
        let b = render(Shell::Bash, Path::new("/home/o'neil/bin"));
        assert!(b.contains(r"'/home/o'\''neil/bin'"));
    }

    #[test]
    fn profiles_are_chosen_from_what_exists_and_the_login_shell() {
        let h = scratch("targets");
        assert!(targets(&h, None, false).is_empty(), "nothing is created for a shell nobody uses");
        let t = targets(&h, Some(Shell::Zsh), false);
        assert_eq!(t.iter().map(|t| t.path.file_name().unwrap().to_str().unwrap()).collect::<Vec<_>>(), [".zshrc"]);
        std::fs::write(h.join(".bashrc"), "x").unwrap();
        std::fs::create_dir_all(h.join(".config/fish")).unwrap();
        let names: Vec<_> = targets(&h, Some(Shell::Zsh), false).iter().map(|t| t.shell.name()).collect();
        assert_eq!(names, ["bash", "zsh", "fish"]);
        assert_eq!(targets(&scratch("mac"), None, true).len(), 1, "a Mac without a known shell still gets zsh, its default");
        assert_eq!(Shell::from_path("/usr/bin/zsh"), Some(Shell::Zsh));
        assert_eq!(Shell::from_path("/bin/dash"), None);
    }

    #[test]
    fn apply_and_strip_work_on_real_files_and_leave_foreign_fish_files_alone() {
        let h = scratch("apply");
        let rc = Target { shell: Shell::Bash, path: h.join(".bashrc"), whole_file: false };
        std::fs::write(&rc.path, "echo hi").unwrap();
        assert_eq!(apply(&rc, Path::new("/p/bin")).unwrap(), Change::Added);
        assert_eq!(apply(&rc, Path::new("/p/bin")).unwrap(), Change::Unchanged);
        assert!(std::fs::read_to_string(&rc.path).unwrap().starts_with("echo hi\n"));
        assert!(strip(&rc.path).unwrap());
        assert_eq!(std::fs::read_to_string(&rc.path).unwrap(), "echo hi\n");
        let fish = Target { shell: Shell::Fish, path: h.join("conf.d/periwire.fish"), whole_file: true };
        assert_eq!(apply(&fish, Path::new("/p/bin")).unwrap(), Change::Added);
        assert!(strip(&fish.path).unwrap() && !fish.path.exists(), "a file that held only our block is deleted");
        std::fs::create_dir_all(h.join("conf.d")).unwrap();
        std::fs::write(&fish.path, "someone else's file\n").unwrap();
        assert_eq!(apply(&fish, Path::new("/p/bin")).unwrap(), Change::Unchanged);
        assert_eq!(std::fs::read_to_string(&fish.path).unwrap(), "someone else's file\n");
    }
}
