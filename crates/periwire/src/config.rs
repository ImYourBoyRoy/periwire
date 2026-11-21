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

#[derive(Debug, Clone, Serialize, Deserialize)]
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
    /// Ask GitHub now and then whether a newer release exists (the desktop app shows it). Off: `periwire` never contacts GitHub by itself.
    #[serde(default = "yes")]
    pub update_check: bool,
    /// User-chosen device names, keyed by `vid:pid[:serial]`.
    #[serde(default)]
    pub labels: std::collections::BTreeMap<String, String>,
    /// What `export` was before a command-line override (`--listen`, `--no-export`) replaced it for this run. Saving writes this, so an
    /// override lasts only as long as the daemon does and is never made permanent by an unrelated change such as sharing a device.
    #[serde(skip)]
    persisted_export: Option<ExportCfg>,
}

fn yes() -> bool {
    true
}

impl Default for Config {
    fn default() -> Self {
        Self { export: ExportCfg::default(), shares: vec![], ro_shares: vec![], connections: vec![], update_check: true, labels: Default::default(), persisted_export: None }
    }
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
    /// Replace the exporter settings for this run only.
    pub fn override_export(&mut self, listen: Option<String>, enabled: Option<bool>) {
        if self.persisted_export.is_none() {
            self.persisted_export = Some(self.export.clone());
        }
        if let Some(l) = listen {
            self.export.listen = l;
        }
        if let Some(e) = enabled {
            self.export.enabled = e;
        }
    }
    /// The operator turned the exporter on or off: that choice is kept (also past a run-only override).
    pub fn set_export_enabled(&mut self, on: bool) {
        self.export.enabled = on;
        if let Some(p) = &mut self.persisted_export {
            p.enabled = on;
        }
    }
    pub fn save(&self) -> Result<()> {
        let p = path();
        std::fs::create_dir_all(p.parent().unwrap())?;
        let tmp = p.with_extension("json.tmp");
        let mut on_disk = self.clone();
        if let Some(p) = &self.persisted_export {
            on_disk.export = p.clone();
        }
        std::fs::write(&tmp, serde_json::to_vec_pretty(&on_disk)?)?;
        std::fs::rename(&tmp, &p)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn update_checks_are_on_unless_switched_off() {
        assert!(Config::default().update_check, "a new install checks");
        let c: Config = serde_json::from_str("{}").unwrap();
        assert!(c.update_check, "a config written before the option existed checks too");
        let off: Config = serde_json::from_str(r#"{"update_check": false}"#).unwrap();
        assert!(!off.update_check);
    }

    #[test]
    fn a_run_only_override_is_not_saved_but_an_explicit_choice_is() {
        let mut c = Config::default();
        c.override_export(Some("127.0.0.1:9".into()), Some(false));
        assert!(!c.export.enabled && c.export.listen == "127.0.0.1:9", "in effect for this run");
        let mut on_disk = c.clone();
        on_disk.export = c.persisted_export.clone().unwrap();
        assert!(on_disk.export.enabled && on_disk.export.listen == "0.0.0.0:3241", "what a save writes is what was there before");
        c.set_export_enabled(true);
        assert!(c.persisted_export.as_ref().unwrap().enabled, "turning the exporter on is the operator's choice and stays");
        assert_eq!(c.persisted_export.as_ref().unwrap().listen, "0.0.0.0:3241", "the run-only address still is not saved");
    }
}
