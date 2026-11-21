//! The persistent per-user `PATH` on Windows (`HKCU\Environment`), after enchi's `user_path.rs`.
//!
//! The list editing is pure and tested everywhere; the registry access is Windows-only and a no-op elsewhere, where shells get `PATH`
//! from their profiles instead (see `profile`). The value keeps its `REG_EXPAND_SZ` type so entries like `%USERPROFILE%\bin` written
//! by other tools still expand.
use std::io;
use std::path::Path;

const SEPARATOR: char = ';';

fn normalized(segment: &str) -> String {
    segment.trim().replace('/', "\\").trim_end_matches('\\').to_ascii_lowercase()
}

/// Put `entry` first in `current`, removing earlier copies (case-insensitively).
pub(crate) fn with_entry_first(current: &str, entry: &str) -> String {
    let key = normalized(entry);
    let rest = current.split(SEPARATOR).filter(|s| !normalized(s).is_empty() && normalized(s) != key);
    std::iter::once(entry.trim()).chain(rest).collect::<Vec<_>>().join(&SEPARATOR.to_string())
}

/// Drop `entry` (and nothing else) from `current`.
pub(crate) fn without_entry(current: &str, entry: &str) -> String {
    let key = normalized(entry);
    current.split(SEPARATOR).filter(|s| !normalized(s).is_empty() && normalized(s) != key).collect::<Vec<_>>().join(&SEPARATOR.to_string())
}

/// Is `entry` in `current`?
#[cfg(test)]
pub(crate) fn contains(current: &str, entry: &str) -> bool {
    let key = normalized(entry);
    current.split(SEPARATOR).any(|s| normalized(s) == key)
}

/// Put `dir` first in the user's persistent PATH. Returns whether the value changed.
pub fn add(dir: &Path) -> io::Result<bool> {
    let d = dir.to_string_lossy().into_owned();
    platform::update(|cur| with_entry_first(cur, &d))
}

/// Remove `dir` from the user's persistent PATH. Returns whether the value changed.
pub fn remove(dir: &Path) -> io::Result<bool> {
    let d = dir.to_string_lossy().into_owned();
    platform::update(|cur| without_entry(cur, &d))
}

#[cfg(windows)]
mod platform {
    use std::borrow::Cow;
    use std::io;
    use winreg::enums::{RegType, HKEY_CURRENT_USER, KEY_READ, KEY_WRITE};
    use winreg::{RegKey, RegValue};

    #[link(name = "user32")]
    extern "system" {
        fn SendMessageTimeoutW(hwnd: isize, msg: u32, wparam: usize, lparam: isize, flags: u32, timeout: u32, result: *mut usize) -> isize;
    }

    /// Tell running programs (Explorer, new terminals) that the environment changed.
    fn broadcast_environment_change() {
        const HWND_BROADCAST: isize = 0xffff;
        const WM_SETTINGCHANGE: u32 = 0x001A;
        const SMTO_ABORTIFHUNG: u32 = 0x0002;
        let payload: Vec<u16> = "Environment\0".encode_utf16().collect();
        let mut result = 0usize;
        // SAFETY: `payload` is a NUL-terminated UTF-16 string that outlives the call.
        unsafe {
            SendMessageTimeoutW(HWND_BROADCAST, WM_SETTINGCHANGE, 0, payload.as_ptr() as isize, SMTO_ABORTIFHUNG, 5000, &mut result);
        }
    }

    pub(super) fn update(edit: impl FnOnce(&str) -> String) -> io::Result<bool> {
        let key = RegKey::predef(HKEY_CURRENT_USER).open_subkey_with_flags("Environment", KEY_READ | KEY_WRITE)?;
        let (current, vtype) = match key.get_raw_value("Path") {
            Ok(raw) => (key.get_value::<String, _>("Path").unwrap_or_default(), raw.vtype),
            Err(e) if e.kind() == io::ErrorKind::NotFound => (String::new(), RegType::REG_EXPAND_SZ),
            Err(e) => return Err(e),
        };
        let updated = edit(&current);
        if updated == current {
            return Ok(false);
        }
        let bytes: Vec<u8> = updated.encode_utf16().chain(std::iter::once(0)).flat_map(u16::to_le_bytes).collect();
        let value = RegValue { bytes: Cow::Owned(bytes), vtype: if vtype == RegType::REG_SZ { RegType::REG_SZ } else { RegType::REG_EXPAND_SZ } };
        key.set_raw_value("Path", &value)?;
        broadcast_environment_change();
        Ok(true)
    }
}

#[cfg(not(windows))]
mod platform {
    use std::io;
    pub(super) fn update(_edit: impl FnOnce(&str) -> String) -> io::Result<bool> {
        Ok(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_entry_goes_first_without_duplicates_and_is_idempotent() {
        let once = with_entry_first(r"C:\Windows;C:\Tools\;c:\periwire", r"C:\PeriWire");
        assert_eq!(once, r"C:\PeriWire;C:\Windows;C:\Tools\", "the old copy goes, the others are left as written");
        assert_eq!(with_entry_first(&once, r"c:\periwire\"), r"c:\periwire\;C:\Windows;C:\Tools\", "case and a trailing backslash do not make a second entry");
        assert_eq!(with_entry_first("", r"C:\PeriWire"), r"C:\PeriWire");
        assert_eq!(with_entry_first(&with_entry_first(r"C:\a", r"C:\b"), r"C:\b"), r"C:\b;C:\a");
    }

    #[test]
    fn removal_drops_only_that_entry_and_presence_is_case_blind() {
        assert_eq!(without_entry(r"C:\a;C:\PeriWire\;C:\b", r"c:\periwire"), r"C:\a;C:\b");
        assert_eq!(without_entry(r"C:\a;C:\PeriWire\bin;C:\b", r"c:\periwire"), r"C:\a;C:\PeriWire\bin;C:\b", "a folder under it stays");
        assert!(contains(r"C:\a;C:\PeriWire\", r"c:\periwire") && !contains(r"C:\a", r"c:\periwire"));
    }

    #[test]
    fn non_windows_hosts_do_nothing() {
        if !cfg!(windows) {
            assert!(!add(Path::new("/x")).unwrap() && !remove(Path::new("/x")).unwrap());
        }
    }
}
