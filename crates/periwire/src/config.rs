//! Persistent daemon configuration (`config.json` in the periwire home directory).
use crate::identity;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportCfg {
    pub enabled: bool,
    pub listen: String,
}
impl Default for ExportCfg {
    fn default() -> Self {
        Self { enabled: true, listen: "0.0.0.0:3241".into() }
    }
}

/// A remembered remote device. With `auto` the daemon attaches it at start and re-attaches after drops.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Connection {
    #[serde(default)]
    pub name: String,
    pub addr: String,
    pub busid: String,
    #[serde(default)]
    pub auto: bool,
    /// Write-protect the device while it is imported (USB storage only). Default: read and write.
    #[serde(default)]
    pub read_only: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub export: ExportCfg,
    /// Busids (`2-2`) or `vid:pid` of devices to share.
    #[serde(default)]
    pub shares: Vec<String>,
    /// The subset of `shares` that are write-protected for everyone who imports them (USB storage only).
    #[serde(default)]
    pub ro_shares: Vec<String>,
    #[serde(default)]
    pub connections: Vec<Connection>,
    /// User-chosen device names, keyed by `vid:pid[:serial]`.
    #[serde(default)]
    pub labels: std::collections::BTreeMap<String, String>,
}

fn path() -> std::path::PathBuf {
    identity::home().join("config.json")
}

impl Config {
    pub fn load() -> Result<Self> {
        match std::fs::read_to_string(path()) {
            Ok(s) => serde_json::from_str(&s).with_context(|| format!("parsing {}", path().display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e.into()),
        }
    }
    pub fn save(&self) -> Result<()> {
        let p = path();
        std::fs::create_dir_all(p.parent().unwrap())?;
        let tmp = p.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(&tmp, &p)?;
        Ok(())
    }
}
