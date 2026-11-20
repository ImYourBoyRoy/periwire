//! Per-machine identity (self-signed cert) and the list of trusted peer fingerprints.
use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use std::{collections::HashSet, fs, io::Write, path::PathBuf};

pub fn home() -> PathBuf {
    if let Ok(p) = std::env::var("PERIWIRE_HOME") {
        return p.into();
    }
    dirs::config_dir().unwrap_or_else(|| ".".into()).join("periwire")
}

pub struct Identity {
    pub cert: Vec<u8>,
    pub key: Vec<u8>,
    pub fingerprint: String,
}

pub fn fingerprint(der: &[u8]) -> String {
    Sha256::digest(der).iter().map(|b| format!("{b:02x}")).collect()
}

fn write_private(path: &PathBuf, data: &[u8]) -> Result<()> {
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    opts.open(path)?.write_all(data)?;
    Ok(())
}

/// Throw away this computer's certificate and make a new one. Every peer must trust the new fingerprint afterwards.
pub fn reset() -> Result<Identity> {
    let dir = home();
    let _ = fs::remove_file(dir.join("cert.der"));
    let _ = fs::remove_file(dir.join("key.der"));
    load_or_create()
}

pub fn load_or_create() -> Result<Identity> {
    let dir = home();
    fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&dir, fs::Permissions::from_mode(0o700));
    }
    let (cp, kp) = (dir.join("cert.der"), dir.join("key.der"));
    if cp.exists() && kp.exists() {
        let cert = fs::read(&cp)?;
        let key = fs::read(&kp)?;
        let fingerprint = fingerprint(&cert);
        return Ok(Identity { cert, key, fingerprint });
    }
    let ck = rcgen::generate_simple_self_signed(vec!["periwire".to_string()])?;
    let cert = ck.cert.der().to_vec();
    let key = ck.key_pair.serialize_der();
    fs::write(&cp, &cert)?;
    write_private(&kp, &key)?;
    let fingerprint = fingerprint(&cert);
    Ok(Identity { cert, key, fingerprint })
}

fn trusted_path() -> PathBuf {
    home().join("trusted.txt")
}

/// A fingerprint is exactly 64 hexadecimal characters.
fn is_fingerprint(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|c| c.is_ascii_hexdigit())
}

/// A name goes on the same line as its fingerprint, so it may not carry a line break (or any other control character), and stays short.
fn clean_name(name: &str) -> String {
    name.chars().map(|c| if c.is_control() { ' ' } else { c }).collect::<String>().trim().chars().take(100).collect()
}

/// (fingerprint, name) for each well-formed line; comments, blank lines and anything whose first word is not a fingerprint are ignored.
fn parse_trusted(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|l| {
            let l = l.trim();
            let mut it = l.splitn(2, char::is_whitespace);
            let fp = it.next()?;
            is_fingerprint(fp).then(|| (fp.to_lowercase(), it.next().unwrap_or("").trim().to_string()))
        })
        .collect()
}

fn read_trusted() -> Result<Vec<(String, String)>> {
    let p = trusted_path();
    if !p.exists() {
        return Ok(vec![]);
    }
    Ok(parse_trusted(&fs::read_to_string(p)?))
}

pub fn load_trusted() -> Result<HashSet<String>> {
    Ok(read_trusted()?.into_iter().map(|(f, _)| f).collect())
}

/// (fingerprint, name) for every trusted peer.
pub fn load_trusted_named() -> Result<Vec<(String, String)>> {
    read_trusted()
}

/// Returns true if the fingerprint was trusted and is now removed.
pub fn remove_trusted(fp: &str) -> Result<bool> {
    let all = load_trusted_named()?;
    let keep: Vec<_> = all.iter().filter(|(f, _)| !f.eq_ignore_ascii_case(fp)).collect();
    if keep.len() == all.len() {
        return Ok(false);
    }
    let body: String = keep.iter().map(|(f, n)| format!("{f} {n}\n")).collect();
    let tmp = trusted_path().with_extension("txt.tmp");
    fs::write(&tmp, body)?;
    fs::rename(&tmp, trusted_path())?; // never leave a half-written trust list behind
    Ok(true)
}

pub fn add_trusted(fp: &str, name: &str) -> Result<()> {
    if !is_fingerprint(fp) {
        anyhow::bail!("fingerprint must be 64 hex characters");
    }
    fs::create_dir_all(home())?;
    if load_trusted()?.contains(&fp.to_lowercase()) {
        return Ok(());
    }
    let mut f = fs::OpenOptions::new().create(true).append(true).open(trusted_path())?;
    writeln!(f, "{} {}", fp.to_lowercase(), clean_name(name))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_cannot_smuggle_a_second_fingerprint_onto_the_list() {
        let a = "a".repeat(64);
        let b = "b".repeat(64);
        assert_eq!(clean_name(&format!("x\n{b} y")), format!("x {b} y"), "the line break is gone, so it stays inside the name");
        let line = format!("{a} {}\n", clean_name(&format!("x\n{b} y")));
        let parsed = parse_trusted(&line);
        assert_eq!(parsed.len(), 1, "one line, one entry");
        assert_eq!(parsed[0].0, a);
    }

    #[test]
    fn only_well_formed_lines_count_as_trusted() {
        let a = "a".repeat(64);
        let text = format!("# comment\n\nnot-a-fingerprint laptop\n{} lap top \n{}\n{} \n", a.to_uppercase(), "c".repeat(64), "d".repeat(63));
        let got = parse_trusted(&text);
        assert_eq!(got, vec![(a, "lap top".to_string()), ("c".repeat(64), String::new())]);
        assert_eq!(clean_name(&"é".repeat(300)).chars().count(), 100);
    }
}
