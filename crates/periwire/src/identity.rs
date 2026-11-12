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

pub fn load_trusted() -> Result<HashSet<String>> {
    let p = trusted_path();
    if !p.exists() {
        return Ok(HashSet::new());
    }
    Ok(fs::read_to_string(p)?
        .lines()
        .filter_map(|l| l.split_whitespace().next())
        .filter(|f| !f.starts_with('#'))
        .map(str::to_string)
        .collect())
}

/// (fingerprint, name) for every trusted peer.
pub fn load_trusted_named() -> Result<Vec<(String, String)>> {
    let p = trusted_path();
    if !p.exists() {
        return Ok(vec![]);
    }
    Ok(fs::read_to_string(p)?
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
        .filter_map(|l| {
            let mut it = l.splitn(2, char::is_whitespace);
            Some((it.next()?.to_string(), it.next().unwrap_or("").trim().to_string()))
        })
        .collect())
}

/// Returns true if the fingerprint was trusted and is now removed.
pub fn remove_trusted(fp: &str) -> Result<bool> {
    let all = load_trusted_named()?;
    let keep: Vec<_> = all.iter().filter(|(f, _)| !f.eq_ignore_ascii_case(fp)).collect();
    if keep.len() == all.len() {
        return Ok(false);
    }
    let body: String = keep.iter().map(|(f, n)| format!("{f} {n}\n")).collect();
    fs::write(trusted_path(), body)?;
    Ok(true)
}

pub fn add_trusted(fp: &str, name: &str) -> Result<()> {
    if fp.len() != 64 || !fp.chars().all(|c| c.is_ascii_hexdigit()) {
        anyhow::bail!("fingerprint must be 64 hex characters");
    }
    fs::create_dir_all(home())?;
    if load_trusted()?.contains(fp) {
        return Ok(());
    }
    let mut f = fs::OpenOptions::new().create(true).append(true).open(trusted_path())?;
    writeln!(f, "{} {}", fp.to_lowercase(), name)?;
    Ok(())
}
