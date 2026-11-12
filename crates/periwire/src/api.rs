//! Local control API: newline-delimited JSON over a Unix socket (see `periwire-client` for the wire format).
//! Everything the CLI, MCP server and GUI can do goes through `dispatch`.
use crate::config::Connection;
use crate::daemon::Daemon;
use crate::{doctor, events, identity};
use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
pub use periwire_client::ApiError;

type R = Result<Value, ApiError>;

fn invalid(m: impl Into<String>) -> ApiError {
    ApiError::new("invalid", m)
}
fn failed(e: anyhow::Error) -> ApiError {
    ApiError::new("failed", format!("{e:#}"))
}
fn str_p<'a>(p: &'a Value, k: &str) -> Option<&'a str> {
    p.get(k).and_then(Value::as_str)
}
fn req_str<'a>(p: &'a Value, k: &str) -> Result<&'a str, ApiError> {
    str_p(p, k).filter(|s| !s.is_empty()).ok_or_else(|| invalid(format!("missing parameter `{k}`")))
}
fn bool_p(p: &Value, k: &str) -> bool {
    p.get(k).and_then(Value::as_bool).unwrap_or(false)
}

/// Methods and their parameters; also served as `methods` for self-documentation.
pub const METHODS: &[(&str, &str)] = &[
    ("ping", "{} -> {version}"),
    ("identity", "{} -> {fingerprint, hostname}"),
    ("identity.reset", "{confirm: true} -> new certificate/fingerprint; every peer must trust the new one; sessions are dropped"),
    ("device.label", "{key, label} -> name a local device (key from `devices`); empty label clears; peers see it"),
    ("status", "{} -> full snapshot: export, shares, sessions, attachments, connections, trusted, import capability"),
    ("devices", "{} -> local USB devices with names, class, shared flag, who uses them, mounted filesystems"),
    ("share.add", "{spec, read_only?} -> share a local device (busid like 2-2, or vid:pid); read_only write-protects USB storage for every importer; calling it again changes the mode (a current user is disconnected)"),
    ("share.remove", "{spec} -> stop sharing; any peer using it is disconnected"),
    ("remote.list", "{addr} -> devices a remote machine shares with us"),
    ("attach", "{addr, busid, auto?, name?, read_only?} -> import a remote device here; auto=remember and reconnect; read_only=write-protect USB storage (default read and write)"),
    ("detach", "{id | busid, addr?, force?} -> safe eject: unmount, detach, release; force unmounts busy mounts lazily"),
    ("kick", "{session | busid} -> force-disconnect a peer using one of our shared devices"),
    ("trust.list", "{} -> trusted peer fingerprints"),
    ("trust.probe", "{addr} -> fingerprint a remote presents, NOT trusted yet; a human must compare it out of band"),
    ("trust.add", "{fingerprint, name?} -> trust a peer"),
    ("trust.remove", "{fingerprint} -> stop trusting a peer"),
    ("connection.save", "{addr, busid, auto?, name?} -> remember a remote device"),
    ("connection.remove", "{addr, busid} -> forget it"),
    ("export.set", "{enabled} -> start/stop the exporter (persisted)"),
    ("driver.status", "{} -> Windows virtual USB host controller driver state ({supported:false} elsewhere)"),
    ("driver.install", "{confirm: true} -> install the signed driver carried inside the program (Windows; the daemon must run as Administrator)"),
    ("driver.uninstall", "{confirm: true} -> remove it again"),
    ("doctor", "{} -> health checks with fixes"),
    ("logs", "{lines?} -> recent daemon log lines"),
    ("subscribe", "{} -> stream events on this connection"),
    ("shutdown", "{} -> stop the daemon gracefully"),
];

pub async fn dispatch(d: &Arc<Daemon>, method: &str, p: &Value) -> R {
    match method {
        "ping" => Ok(json!({"pong": true, "version": crate::daemon::VERSION})),
        "methods" => Ok(json!(METHODS.iter().map(|(m, h)| json!({"method": m, "params": h})).collect::<Vec<_>>())),
        "identity" => Ok(json!({"fingerprint": d.fingerprint(), "hostname": gethostname::gethostname().to_string_lossy()})),
        "identity.reset" => {
            if !bool_p(p, "confirm") {
                return Err(invalid("resetting the identity makes every peer distrust this computer; pass confirm=true"));
            }
            let fp = d.reset_identity().await.map_err(failed)?;
            Ok(json!({"fingerprint": fp}))
        }
        "driver.status" => {
            #[cfg(windows)]
            {
                let st = tokio::task::spawn_blocking(crate::windrv::status).await.map_err(|e| failed(e.into()))?;
                Ok(json!({"supported": true, "status": st}))
            }
            #[cfg(not(windows))]
            {
                Ok(json!({"supported": false, "note": "only Windows needs a separate driver"}))
            }
        }
        "driver.install" | "driver.uninstall" => {
            if !bool_p(p, "confirm") {
                return Err(invalid("changing the driver needs confirm=true (it is a kernel driver install)"));
            }
            #[cfg(windows)]
            {
                if !crate::windrv::is_admin() {
                    return Err(ApiError::new("denied", "the PeriWire daemon is not running as Administrator, so it cannot install a driver: start it from an elevated prompt or install the service (`periwire service install`)"));
                }
                let install = method == "driver.install";
                let r = tokio::task::spawn_blocking(move || {
                    if install { crate::windrv::install().map(Some) } else { crate::windrv::uninstall().map(|_| None) }
                })
                .await
                .map_err(|e| failed(e.into()))?
                .map_err(|e| ApiError::new("failed", format!("{e:#}")))?;
                crate::logln!("{}", if install { "installed the Windows virtual USB host controller driver" } else { "removed the Windows virtual USB host controller driver" });
                let st = tokio::task::spawn_blocking(crate::windrv::status).await.map_err(|e| failed(e.into()))?;
                Ok(json!({"reboot": r.unwrap_or(false), "status": st}))
            }
            #[cfg(not(windows))]
            {
                Err(ApiError::new("unsupported", "only Windows needs a separate driver"))
            }
        }
        "device.label" => {
            let key = req_str(p, "key")?;
            d.set_label(key, str_p(p, "label").unwrap_or("")).map_err(failed)?;
            Ok(json!({"key": key}))
        }
        "status" => {
            let d2 = d.clone();
            tokio::task::spawn_blocking(move || d2.status()).await.map_err(|e| failed(e.into()))
        }
        "devices" => {
            let d2 = d.clone();
            let v = tokio::task::spawn_blocking(move || d2.devices()).await.map_err(|e| failed(e.into()))?.map_err(failed)?;
            Ok(json!(v))
        }
        "share.add" => {
            let spec = req_str(p, "spec")?;
            let read_only = bool_p(p, "read_only");
            let kicked = d.add_share(spec, read_only).map_err(|e| invalid(format!("{e:#}")))?;
            crate::logln!("sharing {spec}{}", if read_only { " (read-only for everyone)" } else { "" });
            Ok(json!({"spec": spec, "read_only": read_only, "disconnected": kicked}))
        }
        "share.remove" => {
            let spec = req_str(p, "spec")?;
            let kicked = d.remove_share(spec).map_err(|e| ApiError::new("not_found", format!("{e:#}")))?;
            crate::logln!("stopped sharing {spec} ({kicked} peer(s) disconnected)");
            Ok(json!({"spec": spec, "disconnected": kicked}))
        }
        "remote.list" => {
            let addr = req_str(p, "addr")?;
            Ok(json!(crate::import::list_remote(&norm_addr(addr)).await.map_err(failed)?))
        }
        "attach" => {
            let (addr, busid) = (norm_addr(req_str(p, "addr")?), req_str(p, "busid")?.to_string());
            let auto = bool_p(p, "auto");
            let read_only = bool_p(p, "read_only");
            if !d.running {
                return Err(ApiError::new("unsupported", "attach needs the daemon (start it with `periwire daemon`; on Linux as root)"));
            }
            crate::attach::can_import().map_err(|e| ApiError::new("unsupported", e))?;
            if let Some(a) = d.atts.find_target(&addr, &busid) {
                return Err(ApiError::new("busy", format!("{busid} from {addr} is already attached (attachment {})", a.id)));
            }
            if auto {
                d.save_connection(Connection { name: str_p(p, "name").unwrap_or("").into(), addr: addr.clone(), busid: busid.clone(), auto: true, read_only }).map_err(failed)?;
            }
            let att = d.atts.start(&addr, &busid, auto, read_only);
            d.atts.wait_attached(&att, Duration::from_secs(25)).await.map_err(failed)?;
            Ok(att.view())
        }
        "detach" => {
            let att = if let Some(id) = p.get("id").and_then(Value::as_u64) {
                d.atts.find(id)
            } else {
                let busid = req_str(p, "busid").map_err(|_| invalid("give `id` or `busid`"))?;
                let all: Vec<_> = d.atts.list().into_iter().filter(|a| a.busid == busid && str_p(p, "addr").map(|x| norm_addr(x) == a.addr).unwrap_or(true)).collect();
                if all.len() > 1 {
                    return Err(invalid(format!("{busid} is attached from several machines; give `addr` or `id`")));
                }
                all.into_iter().next()
            }
            .ok_or_else(|| ApiError::new("not_found", "no such attachment"))?;
            let unmounted = d.atts.detach(&att, bool_p(p, "force")).await.map_err(|e| ApiError::new("busy", format!("{e:#}")))?;
            d.set_auto(&att.addr, &att.busid, false); // a manual detach pauses auto-attach
            Ok(json!({"detached": true, "id": att.id, "unmounted": unmounted}))
        }
        "kick" => {
            let n = if let Some(id) = p.get("session").and_then(Value::as_u64) {
                d.shared.kick(id) as usize
            } else {
                d.shared.kick_busid(req_str(p, "busid").map_err(|_| invalid("give `session` or `busid`"))?)
            };
            if n == 0 {
                return Err(ApiError::new("not_found", "no such session"));
            }
            Ok(json!({"disconnected": n}))
        }
        "trust.list" => Ok(json!(identity::load_trusted_named().map_err(failed)?.into_iter().map(|(f, n)| json!({"fingerprint": f, "name": n})).collect::<Vec<_>>())),
        "trust.probe" => d.probe(&norm_addr(req_str(p, "addr")?)).await.map_err(failed),
        "trust.add" => {
            let fp = req_str(p, "fingerprint")?.to_lowercase();
            identity::add_trusted(&fp, str_p(p, "name").unwrap_or("peer")).map_err(|e| invalid(format!("{e:#}")))?;
            crate::logln!("trusted {}", &fp[..12]);
            events::emit("state", json!({}));
            Ok(json!({"fingerprint": fp}))
        }
        "trust.remove" => {
            let fp = req_str(p, "fingerprint")?;
            if !identity::remove_trusted(fp).map_err(failed)? {
                return Err(ApiError::new("not_found", "that fingerprint is not trusted"));
            }
            events::emit("state", json!({}));
            Ok(json!({"removed": fp}))
        }
        "connection.save" => {
            let c = Connection { name: str_p(p, "name").unwrap_or("").into(), addr: norm_addr(req_str(p, "addr")?), busid: req_str(p, "busid")?.into(), auto: bool_p(p, "auto"), read_only: bool_p(p, "read_only") };
            d.save_connection(c.clone()).map_err(failed)?;
            Ok(json!(c))
        }
        "connection.remove" => {
            let gone = d.forget_connection(&norm_addr(req_str(p, "addr")?), req_str(p, "busid")?).map_err(failed)?;
            if !gone {
                return Err(ApiError::new("not_found", "no such remembered connection"));
            }
            Ok(json!({"removed": true}))
        }
        "export.set" => {
            let on = p.get("enabled").and_then(Value::as_bool).ok_or_else(|| invalid("missing boolean `enabled`"))?;
            d.cfg.lock().unwrap().export.enabled = on;
            d.save_cfg().map_err(failed)?;
            if on {
                d.start_export().await.map_err(failed)?;
            } else {
                d.stop_export();
            }
            Ok(json!({"enabled": on}))
        }
        "doctor" => {
            let d2 = d.clone();
            Ok(json!(tokio::task::spawn_blocking(move || doctor::run(&d2)).await.map_err(|e| failed(e.into()))?))
        }
        "logs" => Ok(json!(events::recent(p.get("lines").and_then(Value::as_u64).unwrap_or(100) as usize))),
        "shutdown" => {
            let _ = d.stop.send(true);
            Ok(json!({"stopping": true}))
        }
        m => Err(ApiError::new("not_found", format!("unknown method `{m}` (see `methods`)"))),
    }
}

/// `host` -> `host:3241`; `[::1]` -> `[::1]:3241`; bare IPv6 gets brackets.
pub fn norm_addr(a: &str) -> String {
    let a = a.trim();
    if a.starts_with('[') {
        return if a.contains("]:") { a.into() } else { format!("{a}:3241") };
    }
    match a.matches(':').count() {
        0 => format!("{a}:3241"),
        1 => a.into(),
        _ => format!("[{a}]:3241"),
    }
}

pub struct ApiHandle {
    path: PathBuf,
}
impl ApiHandle {
    pub fn cleanup(&self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(unix)]
pub async fn serve(d: Arc<Daemon>, opts: &crate::RunOpts) -> Result<ApiHandle> {
    use std::os::unix::fs::PermissionsExt;
    use tokio::net::UnixListener;

    let path = opts.socket.clone().map(PathBuf::from).unwrap_or_else(|| periwire_client::default_socket_path(crate::daemon::is_root(), &identity::home()));
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
        if crate::daemon::is_root() && dir.file_name().map(|n| n == "periwire").unwrap_or(false) {
            let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o755));
        }
    }
    if path.exists() {
        if tokio::net::UnixStream::connect(&path).await.is_ok() {
            anyhow::bail!("another periwire daemon is already running (socket {})", path.display());
        }
        std::fs::remove_file(&path)?; // stale
    }
    let listener = UnixListener::bind(&path).with_context(|| format!("binding {}", path.display()))?;
    let mode = opts.socket_mode.unwrap_or(0o660);
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode))?;
    if crate::daemon::is_root() {
        // Whoever started us with sudo/pkexec gets to use the socket.
        let uid = std::env::var("SUDO_UID").or_else(|_| std::env::var("PKEXEC_UID")).ok().and_then(|v| v.parse::<u32>().ok());
        let mut gid = std::env::var("SUDO_GID").ok().and_then(|v| v.parse::<u32>().ok());
        if gid.is_none() {
            if let Some(u) = uid {
                let pw = unsafe { libc::getpwuid(u) };
                if !pw.is_null() {
                    gid = Some(unsafe { (*pw).pw_gid });
                }
            }
        }
        if let Some(g) = &opts.socket_group {
            let c = std::ffi::CString::new(g.as_str())?;
            let gr = unsafe { libc::getgrnam(c.as_ptr()) };
            if gr.is_null() {
                anyhow::bail!("group `{g}` does not exist");
            }
            gid = Some(unsafe { (*gr).gr_gid });
        }
        let pc = std::ffi::CString::new(path.to_string_lossy().as_bytes())?;
        unsafe { libc::chown(pc.as_ptr(), uid.unwrap_or(u32::MAX), gid.unwrap_or(u32::MAX)) };
    }
    crate::logln!("control socket {}", path.display());

    tokio::spawn(async move {
        loop {
            let Ok((s, _)) = listener.accept().await else { continue };
            tokio::spawn(handle_conn(d.clone(), s));
        }
    });
    Ok(ApiHandle { path })
}

/// One control connection: read requests line by line, push events after `subscribe`.
async fn handle_conn<S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + Unpin + 'static>(d: Arc<Daemon>, s: S) {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let (r, mut w) = tokio::io::split(s);
    let mut lines = BufReader::new(r).lines();
    let mut sub: Option<tokio::sync::broadcast::Receiver<Value>> = None;
    loop {
        let line = tokio::select! {
            l = lines.next_line() => match l { Ok(Some(l)) => l, _ => break },
            ev = async { match sub.as_mut() { Some(r) => r.recv().await, None => std::future::pending().await } } => {
                match ev {
                    Ok(v) => { if w.write_all(format!("{v}\n").as_bytes()).await.is_err() { break; } }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => { let _ = w.write_all(b"{\"event\":\"state\",\"data\":{}}\n").await; }
                    Err(_) => break,
                }
                continue;
            }
        };
        let req: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                let _ = w.write_all(format!("{}\n", json!({"id": null, "error": {"code": "invalid", "message": format!("bad JSON: {e}")}})).as_bytes()).await;
                continue;
            }
        };
        let id = req.get("id").cloned().unwrap_or(Value::Null);
        let method = req.get("method").and_then(Value::as_str).unwrap_or("");
        let params = req.get("params").cloned().unwrap_or(json!({}));
        let reply = if method == "subscribe" {
            sub = Some(events::bus().subscribe());
            Ok(json!({"subscribed": true}))
        } else {
            dispatch(&d, method, &params).await
        };
        let out = match reply {
            Ok(v) => json!({"id": id, "result": v}),
            Err(e) => json!({"id": id, "error": e}),
        };
        if w.write_all(format!("{out}\n").as_bytes()).await.is_err() {
            break;
        }
    }
}

#[cfg(windows)]
pub async fn serve(d: Arc<Daemon>, opts: &crate::RunOpts) -> Result<ApiHandle> {
    use tokio::net::windows::named_pipe::ServerOptions;
    let name = opts.socket.clone().unwrap_or_else(|| periwire_client_pipe());
    // Create the first instance now so a second daemon (or a bad name) fails loudly at startup.
    let mut server = ServerOptions::new().first_pipe_instance(true).create(&name).with_context(|| format!("creating control pipe {name} (is another daemon running?)"))?;
    crate::logln!("control pipe {name}");
    tokio::spawn(async move {
        loop {
            if server.connect().await.is_err() {
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
            let next = match ServerOptions::new().create(&name) {
                Ok(n) => n,
                Err(_) => {
                    tokio::time::sleep(Duration::from_millis(500)).await;
                    continue;
                }
            };
            let connected = std::mem::replace(&mut server, next);
            tokio::spawn(handle_conn(d.clone(), connected));
        }
    });
    Ok(ApiHandle { path: PathBuf::new() })
}

#[cfg(windows)]
fn periwire_client_pipe() -> String {
    periwire_client::PIPE_NAME.to_string()
}

#[cfg(not(any(unix, windows)))]
pub async fn serve(_: Arc<Daemon>, _: &crate::RunOpts) -> Result<ApiHandle> {
    anyhow::bail!("the control channel is not implemented on this OS yet")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addr_normalisation() {
        assert_eq!(norm_addr("host"), "host:3241");
        assert_eq!(norm_addr("host:99"), "host:99");
        assert_eq!(norm_addr("10.0.0.5"), "10.0.0.5:3241");
        assert_eq!(norm_addr("[::1]"), "[::1]:3241");
        assert_eq!(norm_addr("[::1]:7"), "[::1]:7");
        assert_eq!(norm_addr("fe80::1"), "[fe80::1]:3241");
    }

    #[test]
    fn spec_validation() {
        use crate::daemon::validate_spec as v;
        for ok in ["2-2", "1-1.4", "10-3.2.1", "18a5:0243", "ABCD:ef01"] {
            assert!(v(ok).is_ok(), "{ok}");
        }
        for bad in ["", "2", "2-", "-2", "18a5:024", "18a5-0243", "zzzz:0000", "2-2; rm -rf /", "1-1x"] {
            assert!(v(bad).is_err(), "{bad}");
        }
    }

    // One test touches the process-wide PERIWIRE_HOME, so everything state-related lives here.
    #[tokio::test]
    async fn control_api_roundtrip() {
        let dir = std::env::temp_dir().join(format!("periwire-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::env::set_var("PERIWIRE_HOME", &dir);
        let d = Daemon::new(false).unwrap();

        // sharing is validated, persisted and idempotent
        assert!(dispatch(&d, "share.add", &json!({"spec": "nope"})).await.is_err());
        dispatch(&d, "share.add", &json!({"spec": "18A5:0243"})).await.unwrap();
        dispatch(&d, "share.add", &json!({"spec": "18a5:0243"})).await.unwrap();
        assert_eq!(crate::config::Config::load().unwrap().shares, vec!["18a5:0243"]);
        let st = dispatch(&d, "status", &json!({})).await.unwrap();
        assert_eq!(st["shares"].as_array().unwrap().len(), 1);
        dispatch(&d, "share.remove", &json!({"spec": "18a5:0243"})).await.unwrap();
        assert_eq!(dispatch(&d, "share.remove", &json!({"spec": "18a5:0243"})).await.unwrap_err().code, "not_found");

        // an owner-side read-only share is persisted, shown in status, switchable, and cleared when unshared
        let r = dispatch(&d, "share.add", &json!({"spec": "0781:5581", "read_only": true})).await.unwrap();
        assert_eq!(r["read_only"], true);
        assert_eq!(crate::config::Config::load().unwrap().ro_shares, vec!["0781:5581"]);
        let st = dispatch(&d, "status", &json!({})).await.unwrap();
        assert_eq!(st["shares"][0]["read_only"], true);
        dispatch(&d, "share.add", &json!({"spec": "0781:5581"})).await.unwrap(); // back to read and write
        assert!(crate::config::Config::load().unwrap().ro_shares.is_empty());
        assert_eq!(dispatch(&d, "status", &json!({})).await.unwrap()["shares"][0]["read_only"], false);
        dispatch(&d, "share.add", &json!({"spec": "0781:5581", "read_only": true})).await.unwrap();
        dispatch(&d, "share.remove", &json!({"spec": "0781:5581"})).await.unwrap();
        let cfg = crate::config::Config::load().unwrap();
        assert!(cfg.shares.is_empty() && cfg.ro_shares.is_empty());

        // trust list round trip, and bad input is rejected
        let fp = "ab".repeat(32);
        assert_eq!(dispatch(&d, "trust.add", &json!({"fingerprint": "xyz"})).await.unwrap_err().code, "invalid");
        dispatch(&d, "trust.add", &json!({"fingerprint": fp, "name": "box"})).await.unwrap();
        let l = dispatch(&d, "trust.list", &json!({})).await.unwrap();
        assert_eq!(l[0]["name"], "box");
        dispatch(&d, "trust.remove", &json!({"fingerprint": fp})).await.unwrap();
        assert_eq!(dispatch(&d, "trust.remove", &json!({"fingerprint": fp})).await.unwrap_err().code, "not_found");

        // attaching needs a daemon; unknown methods and missing params are clean errors
        assert_eq!(dispatch(&d, "attach", &json!({"addr": "h", "busid": "1-1"})).await.unwrap_err().code, "unsupported");
        assert_eq!(dispatch(&d, "nope", &json!({})).await.unwrap_err().code, "not_found");
        assert_eq!(dispatch(&d, "share.add", &json!({})).await.unwrap_err().code, "invalid");

        // doctor always answers and every check is well formed
        let doc = dispatch(&d, "doctor", &json!({})).await.unwrap();
        assert!(doc.as_array().unwrap().iter().all(|c| ["ok", "warn", "fail", "info"].contains(&c["status"].as_str().unwrap())));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
