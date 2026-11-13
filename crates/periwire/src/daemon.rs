//! The daemon: owns the exporter, the importer attachments, the trust list and the configuration,
//! and exposes them through the local control API (see `api.rs`).
use crate::attach::{self, Registry};
use crate::config::{Config, Connection};
use crate::export::{self, Shared};
use crate::{events, identity, import};
use anyhow::{bail, Result};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use tokio::sync::watch;

#[derive(Clone, Default)]
pub struct ExportStatus {
    pub state: &'static str, // disabled | listening | error
    pub error: Option<String>,
}

pub struct Daemon {
    pub started: u64,
    pub fingerprint: Mutex<String>,
    pub cfg: Mutex<Config>,
    pub shared: Arc<Shared>,
    pub atts: Arc<Registry>,
    pub export: Mutex<ExportStatus>,
    export_stop: Mutex<Option<watch::Sender<bool>>>,
    pub stop: watch::Sender<bool>,
    /// false when the CLI builds a throw-away instance to answer a command without a running daemon.
    pub running: bool,
}

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

impl Daemon {
    pub fn new(running: bool) -> Result<Arc<Self>> {
        let id = identity::load_or_create()?;
        let cfg = Config::load()?;
        let shared = Shared::new(cfg.shares.clone(), cfg.ro_shares.clone(), cfg.labels.clone());
        Ok(Arc::new(Daemon {
            started: events::now(),
            fingerprint: Mutex::new(id.fingerprint),
            cfg: Mutex::new(cfg),
            shared,
            atts: Arc::new(Registry::default()),
            export: Mutex::new(ExportStatus { state: "disabled", error: None }),
            export_stop: Mutex::new(None),
            stop: watch::channel(false).0,
            running,
        }))
    }

    pub fn fingerprint(&self) -> String {
        self.fingerprint.lock().unwrap().clone()
    }

    /// Give a device a name people recognise (empty clears it). Peers see it too.
    pub fn set_label(&self, key: &str, label: &str) -> Result<()> {
        self.shared.set_label(key, label);
        let mut c = self.cfg.lock().unwrap();
        c.labels = self.shared.labels.read().unwrap().clone();
        c.save()?;
        events::emit("state", json!({}));
        Ok(())
    }

    /// New certificate. Peers must trust the new fingerprint; running sessions are dropped by restarting the exporter.
    pub async fn reset_identity(self: &Arc<Self>) -> Result<String> {
        let id = identity::reset()?;
        *self.fingerprint.lock().unwrap() = id.fingerprint.clone();
        crate::logln!("identity reset; new fingerprint {}", &id.fingerprint[..16]);
        let was = self.export_stop.lock().unwrap().is_some();
        if was {
            self.stop_export();
            tokio::time::sleep(std::time::Duration::from_millis(600)).await;
            self.start_export().await?;
        }
        events::emit("state", json!({}));
        Ok(id.fingerprint)
    }

    pub fn save_cfg(&self) -> Result<()> {
        self.cfg.lock().unwrap().save()
    }

    // ---- exporter ----

    pub async fn start_export(self: &Arc<Self>) -> Result<()> {
        if self.export_stop.lock().unwrap().is_some() {
            return Ok(());
        }
        let listen = self.cfg.lock().unwrap().export.listen.clone();
        let listener = match export::bind(&listen).await {
            Ok(l) => l,
            Err(e) => {
                *self.export.lock().unwrap() = ExportStatus { state: "error", error: Some(format!("{e:#}")) };
                events::emit("state", json!({}));
                return Err(e);
            }
        };
        let (tx, rx) = watch::channel(false);
        *self.export_stop.lock().unwrap() = Some(tx);
        *self.export.lock().unwrap() = ExportStatus { state: "listening", error: None };
        crate::logln!("exporter listening on {listen}");
        events::emit("state", json!({}));
        let (me, shared) = (self.clone(), self.shared.clone());
        tokio::spawn(async move {
            if let Err(e) = export::serve(shared, listener, rx).await {
                crate::logln!("exporter stopped: {e:#}");
            }
            *me.export_stop.lock().unwrap() = None;
            let mut st = me.export.lock().unwrap();
            if st.state == "listening" {
                *st = ExportStatus { state: "disabled", error: None };
            }
            drop(st);
            events::emit("state", json!({}));
        });
        Ok(())
    }

    pub fn stop_export(&self) {
        if let Some(tx) = self.export_stop.lock().unwrap().take() {
            let _ = tx.send(true);
            crate::logln!("exporter stopped; devices return to this machine");
        }
        *self.export.lock().unwrap() = ExportStatus { state: "disabled", error: None };
        events::emit("state", json!({}));
    }

    // ---- views ----

    pub fn devices(&self) -> Result<Vec<Value>> {
        export::device_views(&self.shared)
    }

    pub fn status(&self) -> Value {
        let cfg = self.cfg.lock().unwrap().clone();
        let exp = self.export.lock().unwrap().clone();
        let (import_ok, import_reason) = match attach::can_import() {
            Ok(()) => (true, Value::Null),
            Err(e) => (false, json!(e)),
        };
        let present: Vec<String> = export::local_devices().map(|d| d.iter().map(|(u, _)| format!("{}|{:04x}:{:04x}", u.busid, u.id_vendor, u.id_product)).collect()).unwrap_or_default();
        let shares: Vec<Value> = self
            .shared
            .specs()
            .into_iter()
            .map(|s| json!({"spec": s, "read_only": self.shared.ro_specs.read().unwrap().iter().any(|r| r.eq_ignore_ascii_case(&s)), "present": present.iter().any(|p| { let (b, v) = p.split_once('|').unwrap(); b == s || v.eq_ignore_ascii_case(&s) })}))
            .collect();
        let mut sessions: Vec<_> = self.shared.sessions.lock().unwrap().values().cloned().collect();
        sessions.sort_by_key(|s| s.id);
        let trusted: Vec<Value> = identity::load_trusted_named().unwrap_or_default().into_iter().map(|(f, n)| json!({"fingerprint": f, "name": n})).collect();
        json!({
            "version": VERSION, "os": std::env::consts::OS, "arch": std::env::consts::ARCH,
            "hostname": gethostname::gethostname().to_string_lossy(), "pid": std::process::id(),
            "daemon": self.running, "started": self.started, "uptime": events::now().saturating_sub(self.started),
            "root": is_root(), "fingerprint": self.fingerprint(),
            "export": {"enabled": cfg.export.enabled, "listen": cfg.export.listen, "state": exp.state, "error": exp.error},
            "import": {"available": import_ok, "reason": import_reason},
            "shares": shares,
            "sessions": sessions.iter().map(|s| s.view()).collect::<Vec<_>>(),
            "attachments": self.atts.list().iter().map(|a| a.view()).collect::<Vec<_>>(),
            "connections": cfg.connections,
            "trusted": trusted,
        })
    }

    // ---- shares ----

    /// Share a device. `read_only` write-protects it for everyone who imports it (USB storage only). Changing the mode of
    /// a share that is in use disconnects its current user, who reconnects under the new rule.
    pub fn add_share(&self, spec: &str, read_only: bool) -> Result<usize> {
        validate_spec(spec)?;
        {
            let mut s = self.shared.specs.write().unwrap();
            if !s.iter().any(|x| x.eq_ignore_ascii_case(spec)) {
                s.push(spec.to_lowercase());
            }
        }
        let was_ro = {
            let mut r = self.shared.ro_specs.write().unwrap();
            let was = r.iter().any(|x| x.eq_ignore_ascii_case(spec));
            r.retain(|x| !x.eq_ignore_ascii_case(spec));
            if read_only {
                r.push(spec.to_lowercase());
            }
            was
        };
        let mut kicked = 0;
        if was_ro != read_only {
            for s in self.shared.sessions.lock().unwrap().values() {
                let dev = format!("{:04x}:{:04x}", s.vid, s.pid);
                if spec == s.busid || spec.eq_ignore_ascii_case(&dev) {
                    kicked += self.shared.kick(s.id) as usize;
                }
            }
        }
        let mut c = self.cfg.lock().unwrap();
        if !c.shares.iter().any(|x| x.eq_ignore_ascii_case(spec)) {
            c.shares.push(spec.to_lowercase());
        }
        c.ro_shares.retain(|x| !x.eq_ignore_ascii_case(spec));
        if read_only {
            c.ro_shares.push(spec.to_lowercase());
        }
        c.save()?;
        events::emit("state", json!({}));
        Ok(kicked)
    }

    /// Stop sharing a device. Any peer currently using it is disconnected and the device returns here.
    pub fn remove_share(&self, spec: &str) -> Result<usize> {
        let before = self.shared.specs.read().unwrap().len();
        self.shared.specs.write().unwrap().retain(|x| !x.eq_ignore_ascii_case(spec));
        self.shared.ro_specs.write().unwrap().retain(|x| !x.eq_ignore_ascii_case(spec));
        let removed = before - self.shared.specs.read().unwrap().len();
        let mut kicked = 0;
        for s in self.shared.sessions.lock().unwrap().values() {
            let dev = format!("{:04x}:{:04x}", s.vid, s.pid);
            if spec == s.busid || spec.eq_ignore_ascii_case(&dev) {
                kicked += self.shared.kick(s.id) as usize;
            }
        }
        let mut c = self.cfg.lock().unwrap();
        c.shares.retain(|x| !x.eq_ignore_ascii_case(spec));
        c.ro_shares.retain(|x| !x.eq_ignore_ascii_case(spec));
        c.save()?;
        if removed == 0 && kicked == 0 {
            bail!("{spec} is not shared");
        }
        events::emit("state", json!({}));
        Ok(kicked)
    }

    // ---- connections (remembered imports) ----

    pub fn save_connection(&self, c: Connection) -> Result<()> {
        let mut cfg = self.cfg.lock().unwrap();
        match cfg.connections.iter_mut().find(|x| x.addr == c.addr && x.busid == c.busid) {
            Some(x) => *x = c,
            None => cfg.connections.push(c),
        }
        cfg.save()?;
        events::emit("state", json!({}));
        Ok(())
    }

    pub fn forget_connection(&self, addr: &str, busid: &str) -> Result<bool> {
        let mut cfg = self.cfg.lock().unwrap();
        let n = cfg.connections.len();
        cfg.connections.retain(|x| !(x.addr == addr && x.busid == busid));
        let gone = cfg.connections.len() != n;
        cfg.save()?;
        events::emit("state", json!({}));
        Ok(gone)
    }

    pub fn set_auto(&self, addr: &str, busid: &str, auto: bool) {
        let mut cfg = self.cfg.lock().unwrap();
        if let Some(x) = cfg.connections.iter_mut().find(|x| x.addr == addr && x.busid == busid) {
            x.auto = auto;
            let _ = cfg.save();
        }
    }

    /// Attach everything marked `auto` (called at daemon start).
    pub fn start_auto(&self) {
        let conns = self.cfg.lock().unwrap().connections.clone();
        for c in conns.into_iter().filter(|c| c.auto) {
            if attach::can_import().is_ok() && self.atts.find_target(&c.addr, &c.busid).is_none() {
                crate::logln!("auto-attaching {} from {}", c.busid, c.addr);
                self.atts.start(&c.addr, &c.busid, true, c.read_only);
            }
        }
    }

    // ---- trust ----

    pub async fn probe(&self, addr: &str) -> Result<Value> {
        let fp = import::probe_fingerprint(addr).await?;
        let known = identity::load_trusted()?.contains(&fp);
        Ok(json!({"addr": addr, "fingerprint": fp, "already_trusted": known}))
    }
}

pub fn is_root() -> bool {
    #[cfg(unix)]
    {
        unsafe { libc::geteuid() == 0 }
    }
    #[cfg(not(unix))]
    {
        false
    }
}

/// A share spec is a busid (`2-2`, `1-1.4`) or `vid:pid` in hex.
pub fn validate_spec(spec: &str) -> Result<()> {
    let busid = {
        let mut it = spec.split('-');
        match (it.next(), it.next(), it.next()) {
            (Some(b), Some(p), None) => b.parse::<u32>().is_ok() && !p.is_empty() && p.split('.').all(|x| x.parse::<u32>().is_ok()),
            _ => false,
        }
    };
    let vidpid = spec.len() == 9 && spec.as_bytes()[4] == b':' && spec.chars().enumerate().all(|(i, c)| i == 4 || c.is_ascii_hexdigit());
    if busid || vidpid {
        Ok(())
    } else {
        bail!("`{spec}` is not a busid (like 2-2 or 1-1.4) or a vid:pid (like 1234:5678)")
    }
}

/// Run the daemon until stopped.
pub async fn run(opts: crate::RunOpts) -> Result<()> {
    let d = Daemon::new(true)?;
    if let Some(l) = &opts.listen {
        d.cfg.lock().unwrap().export.listen = l.clone();
    }
    if opts.no_export {
        d.cfg.lock().unwrap().export.enabled = false;
    }
    for s in &opts.share {
        validate_spec(s)?;
        let mut sp = d.shared.specs.write().unwrap();
        if !sp.iter().any(|x| x == s) {
            sp.push(s.clone()); // runtime only: --share is not persisted
        }
    }
    crate::logln!("periwire {VERSION} starting; identity {}", d.fingerprint());
    if d.cfg.lock().unwrap().export.enabled {
        if let Err(e) = d.start_export().await {
            crate::logln!("exporter not started: {e:#}");
        }
    }
    let api = crate::api::serve(d.clone(), &opts).await?;
    d.start_auto();
    crate::tray::ensure(opts.no_tray); // the taskbar icon belongs to the desktop app; see tray.rs
    let mut stop = d.stop.subscribe();
    tokio::select! {
        _ = export::wait_for_stop() => {}
        _ = stop.changed() => {}
    }
    crate::logln!("shutting down: ejecting imports and releasing shared devices...");
    d.stop_export();
    d.atts.shutdown_all().await;
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    api.cleanup();
    crate::logln!("stopped");
    Ok(())
}
