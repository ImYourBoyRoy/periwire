//! The command-line interface. Every command is a thin wrapper over one control-API call,
//! rendered for humans (or as raw JSON with `--json`).
use crate::api::ApiError;
use crate::backend::Backend;
use crate::ui::*;
use anyhow::{bail, Result};
use serde_json::{json, Value};
use std::io::{IsTerminal, Write};

fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(Value::as_str).unwrap_or("")
}
fn n(v: &Value, k: &str) -> u64 {
    v.get(k).and_then(Value::as_u64).unwrap_or(0)
}
fn arr<'a>(v: &'a Value, k: &str) -> &'a [Value] {
    v.get(k).and_then(Value::as_array).map(Vec::as_slice).unwrap_or(&[])
}

pub fn print_json(v: &Value) {
    println!("{}", serde_json::to_string_pretty(v).unwrap());
}

pub async fn call(b: &mut Backend, m: &str, p: Value) -> Result<Value> {
    b.call(m, p).await.map_err(|e: ApiError| anyhow::anyhow!("{}", e.message))
}

fn dev_name(d: &Value) -> String {
    let dn = s(d, "display");
    if !dn.is_empty() {
        return dn.to_string();
    }
    let mf = s(d, "manufacturer");
    let pr = s(d, "product");
    match (mf.is_empty(), pr.is_empty()) {
        (true, true) => String::from("(unnamed)"),
        (false, true) => mf.to_string(),
        (true, false) => pr.to_string(),
        (false, false) => if pr.to_lowercase().contains(&mf.to_lowercase()) { pr.to_string() } else { format!("{mf} {pr}") },
    }
}

// ---------- status ----------

pub async fn status(b: &mut Backend, json_out: bool) -> Result<i32> {
    let mut st = call(b, "status", json!({})).await?;
    let devs = call(b, "devices", json!({})).await.unwrap_or(json!([]));
    if json_out {
        st["devices"] = devs;
        print_json(&st);
        return Ok(0);
    }
    let daemon = st["daemon"].as_bool().unwrap_or(false);
    println!(
        "{} {}  {} {} {}  host {}",
        bold("periwire"), s(&st, "version"), dim("·"), s(&st, "os"), s(&st, "arch"), s(&st, "hostname")
    );
    if daemon {
        println!("daemon    {} up {}  pid {}{}", green("running"), dur(n(&st, "uptime")), n(&st, "pid"), if st["root"].as_bool() == Some(true) { "  (root)" } else { "" });
    } else {
        println!("daemon    {}  {}", yellow("not running"), dim("(start it: periwire daemon)"));
    }
    println!("identity  {}  {}", short_fp(s(&st, "fingerprint")), dim("(full: periwire id)"));

    let ex = &st["export"];
    let state = match s(ex, "state") {
        "listening" => green(&format!("listening on {}", s(ex, "listen"))),
        "error" => red(&format!("error: {}", s(ex, "error"))),
        _ => dim("off").to_string(),
    };
    println!("\n{}  {}", bold("SHARING"), state);
    let shares = arr(&st, "shares");
    if shares.is_empty() {
        println!("  {}", dim("nothing shared yet — `periwire devices`, then `periwire share <busid>`"));
    }
    for sh in shares {
        let spec = s(sh, "spec");
        let dev = devs.as_array().and_then(|a| a.iter().find(|d| s(d, "busid") == spec || format!("{}:{}", s(d, "vid"), s(d, "pid")).eq_ignore_ascii_case(spec)));
        let mut line = format!("  {:<8}", if spec.contains(':') { "" } else { spec });
        match dev {
            Some(d) => {
                line += &format!(" {}:{}  {}  ", s(d, "vid"), s(d, "pid"), dev_name(d));
                if let Some(u) = d.get("in_use_by").and_then(Value::as_str) {
                    line += &cyan(&format!("in use by {u}"));
                } else {
                    line += &green("ready");
                }
            }
            None => line += &yellow("not plugged in"),
        }
        println!("{line}");
    }
    for se in arr(&st, "sessions") {
        println!(
            "    {} #{} {} → {}  {}  ↓{} ↑{}",
            dim("session"), n(se, "id"), if se["info"].is_object() { dev_name(&se["info"]) } else { s(se, "busid").to_string() },
            if s(se, "peer_name").is_empty() { s(se, "peer") } else { s(se, "peer_name") },
            dur(crate::events::now().saturating_sub(n(se, "since"))), bytes(n(se, "bytes_out")), bytes(n(se, "bytes_in"))
        );
    }

    println!("\n{}  {}", bold("IMPORTED"), if st["import"]["available"] == json!(true) { String::new() } else { dim(&format!("importing unavailable: {}", s(&st["import"], "reason"))) });
    let atts = arr(&st, "attachments");
    if atts.is_empty() {
        println!("  {}", dim("nothing attached — `periwire remote <host>`, then `periwire attach <host> <busid>`"));
    }
    for a in atts {
        let state = match s(a, "state") {
            "attached" => green("attached"),
            "connecting" | "reconnecting" => yellow(s(a, "state")),
            x => dim(x),
        };
        let nm = if a["info"].is_object() { format!("{} ({})", dev_name(&a["info"]), s(&a["info"], "kind_label")) } else { format!("{}:{}", s(a, "vid"), s(a, "pid")) };
        println!(
            "  #{} {:<8} {}  from {}  {}{}{}",
            n(a, "id"), s(a, "busid"), nm, s(a, "addr"), state,
            if a["auto"] == json!(true) { dim("  auto") } else { String::new() },
            format!("{}{}", if a["read_only"] == json!(true) { yellow("  read-only") } else { String::new() }, if s(a, "state") == "attached" { format!("  {}  ↓{} ↑{}", dur(crate::events::now().saturating_sub(n(a, "attached_at"))), bytes(n(a, "bytes_in")), bytes(n(a, "bytes_out"))) } else { String::new() })
        );
        if let Some(e) = a["error"].as_str() { println!("      {}", yellow(e)); }
        let mounts: Vec<&str> = arr(a, "mounts").iter().filter_map(Value::as_str).collect();
        if !mounts.is_empty() { println!("      mounted at {}", mounts.join(", ")); }
    }
    let idle: Vec<_> = arr(&st, "connections").iter().filter(|c| !atts.iter().any(|a| s(a, "addr") == s(c, "addr") && s(a, "busid") == s(c, "busid"))).collect();
    if !idle.is_empty() {
        println!("\n{}", bold("REMEMBERED"));
        for c in idle {
            println!("  {:<8} from {}  {}{}", s(c, "busid"), s(c, "addr"), if c["auto"] == json!(true) { "auto-attach on" } else { "auto-attach off" }, if s(c, "name").is_empty() { String::new() } else { format!("  ({})", s(c, "name")) });
        }
    }
    println!("\n{}  {}", bold("TRUSTED"), arr(&st, "trusted").len());
    for t in arr(&st, "trusted") {
        println!("  {}  {}", short_fp(s(t, "fingerprint")), s(t, "name"));
    }
    Ok(0)
}

pub async fn devices(b: &mut Backend, json_out: bool) -> Result<i32> {
    let v = call(b, "devices", json!({})).await?;
    if json_out {
        print_json(&v);
        return Ok(0);
    }
    let rows: Vec<Vec<String>> = v.as_array().unwrap().iter().map(|d| {
        let st = if let Some(u) = d["in_use_by"].as_str() { cyan(&format!("in use by {u}")) } else if d["shared"] == json!(true) { green(if d["shared_read_only"] == json!(true) { "shared (read-only)" } else { "shared" }) } else { dim("—") };
        let mut st = st;
        if !arr(d, "mounted").is_empty() { st += &yellow("  mounted locally"); }
        vec![s(d, "busid").into(), format!("{}:{}", s(d, "vid"), s(d, "pid")), dev_name(d), s(d, "kind_label").into(), s(d, "speed").split(' ').next().unwrap_or("").into(), st]
    }).collect();
    if rows.is_empty() { println!("no shareable USB devices found"); } else { table(&["busid", "id", "device", "kind", "speed", "state"], &rows); }
    Ok(0)
}

pub async fn remote(b: &mut Backend, addr: &str, json_out: bool) -> Result<i32> {
    let v = call(b, "remote.list", json!({"addr": addr})).await?;
    if json_out { print_json(&v); return Ok(0); }
    let rows: Vec<Vec<String>> = arr(&json!({"d": v}), "d").iter().map(|d| vec![s(d, "busid").into(), format!("{}:{}", s(d, "vid"), s(d, "pid")), dev_name(d), s(d, "kind_label").into(), s(d, "speed").split(' ').next().unwrap_or("").into()]).collect();
    if rows.is_empty() { println!("{addr} is not sharing any devices with you"); } else { table(&["busid", "id", "device", "kind", "speed"], &rows); }
    Ok(0)
}

pub async fn doctor(b: &mut Backend, json_out: bool) -> Result<i32> {
    let v = call(b, "doctor", json!({})).await?;
    let checks = v.as_array().cloned().unwrap_or_default();
    let fails = checks.iter().filter(|c| s(c, "status") == "fail").count();
    if json_out { print_json(&v); return Ok(if fails > 0 { 4 } else { 0 }); }
    for c in &checks {
        let (mark, col): (&str, fn(&str) -> String) = match s(c, "status") { "ok" => ("✔", green), "warn" => ("!", yellow), "fail" => ("✘", red), _ => ("·", dim) };
        println!("{} {}  {}", col(mark), bold(s(c, "title")), dim(s(c, "detail")));
        if let Some(f) = c["fix"].as_str() { if s(c, "status") != "ok" { println!("    fix: {f}"); } }
    }
    let warns = checks.iter().filter(|c| s(c, "status") == "warn").count();
    println!("\n{}", if fails > 0 { red(&format!("{fails} problem(s)")) } else if warns > 0 { yellow(&format!("ok with {warns} warning(s)")) } else { green("all good") });
    Ok(if fails > 0 { 4 } else { 0 })
}

pub async fn simple(b: &mut Backend, json_out: bool, method: &str, params: Value, human: impl Fn(&Value) -> String) -> Result<i32> {
    let v = call(b, method, params).await?;
    if json_out { print_json(&v); } else { let t = human(&v); if !t.is_empty() { println!("{t}"); } }
    Ok(0)
}

// ---------- target resolution ----------

pub async fn resolve_attachment(b: &mut Backend, target: &str) -> Result<Value> {
    let st = call(b, "status", json!({})).await?;
    let atts = arr(&st, "attachments");
    let (spec, addr) = target.split_once('@').map(|(a, b)| (a, Some(crate::api::norm_addr(b)))).unwrap_or((target, None));
    let m: Vec<&Value> = atts.iter().filter(|a| {
        (spec.parse::<u64>().ok() == Some(n(a, "id")) && !spec.contains('-'))
            || s(a, "busid") == spec
            || format!("{}:{}", s(a, "vid"), s(a, "pid")).eq_ignore_ascii_case(spec)
    }).filter(|a| addr.as_deref().map(|x| x == s(a, "addr")).unwrap_or(true)).collect();
    match m.len() {
        0 => bail!("no attached device matches `{target}` (see `periwire status`)"),
        1 => Ok(m[0].clone()),
        _ => bail!("`{target}` matches several attachments; use its number (`periwire status`) or busid@host"),
    }
}

pub async fn resolve_session(b: &mut Backend, target: &str) -> Result<Value> {
    let st = call(b, "status", json!({})).await?;
    let ss = arr(&st, "sessions");
    let m: Vec<&Value> = ss.iter().filter(|x| (target.parse::<u64>().ok() == Some(n(x, "id")) && !target.contains('-')) || s(x, "busid") == target).collect();
    match m.len() {
        0 => bail!("no peer is using `{target}` right now (see `periwire status`)"),
        1 => Ok(m[0].clone()),
        _ => bail!("several sessions match `{target}`; use the session number"),
    }
}

// ---------- logs / watch ----------

pub async fn logs(lines: usize, follow: bool) -> Result<i32> {
    let mut c = periwire_client::Client::connect_default().await.map_err(|e| anyhow::anyhow!("{e}"))?;
    let v = c.call("logs", json!({"lines": lines})).await.map_err(|e| anyhow::anyhow!("{e}"))?;
    for l in v.as_array().unwrap() { println!("{}", l.as_str().unwrap_or("")); }
    if follow {
        c.call("subscribe", json!({})).await.map_err(|e| anyhow::anyhow!("{e}"))?;
        while let Some(ev) = c.next_event().await {
            if ev["event"] == "log" { println!("{}", ev["data"]["line"].as_str().unwrap_or("")); let _ = std::io::stdout().flush(); }
        }
    }
    Ok(0)
}

pub async fn watch(json_out: bool) -> Result<i32> {
    let mut c = periwire_client::Client::connect_default().await.map_err(|e| anyhow::anyhow!("{e}"))?;
    c.call("subscribe", json!({})).await.map_err(|e| anyhow::anyhow!("{e}"))?;
    while let Some(ev) = c.next_event().await {
        if json_out { println!("{ev}"); } else if ev["event"] != "log" { println!("{}  {}  {}", dim(&hms(n(&ev, "time"))), bold(s(&ev, "event")), if ev["data"] == json!({}) { String::new() } else { ev["data"].to_string() }); }
        let _ = std::io::stdout().flush();
    }
    println!("daemon closed the connection");
    Ok(0)
}

// ---------- trust ----------

pub async fn trust_host(b: &mut Backend, addr: &str, yes: bool, json_out: bool) -> Result<i32> {
    let p = call(b, "trust.probe", json!({"addr": addr})).await?;
    let fp = s(&p, "fingerprint").to_string();
    if p["already_trusted"] == json!(true) {
        if json_out { print_json(&p); } else { println!("{addr} is already trusted ({})", short_fp(&fp)); }
        return Ok(0);
    }
    if json_out && !yes { print_json(&p); return Ok(0); }
    println!("{addr} presents this fingerprint:\n\n  {}\n\nOn that machine run `periwire id` and compare. Only trust it if they are identical.", bold(&fp));
    if !yes {
        if !std::io::stdin().is_terminal() { bail!("not a terminal: compare the fingerprint yourself, then run `periwire trust add {fp} --name <name>`"); }
        print!("Trust this machine? [y/N] ");
        std::io::stdout().flush()?;
        let mut a = String::new();
        std::io::stdin().read_line(&mut a)?;
        if !a.trim().eq_ignore_ascii_case("y") { println!("not trusted"); return Ok(1); }
    }
    call(b, "trust.add", json!({"fingerprint": fp, "name": addr})).await?;
    println!("{} {addr}", green("trusted"));
    Ok(0)
}

// ---------- service files ----------

pub fn service_text(user_mode: bool) -> String {
    let exe = std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_else(|_| "/usr/local/bin/periwire".into());
    if cfg!(target_os = "macos") {
        return format!(r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>Label</key><string>com.glassprovidence.periwire.daemon</string>
  <key>ProgramArguments</key><array><string>{exe}</string><string>daemon</string></array>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
</dict></plist>
"#);
    }
    if user_mode {
        format!("[Unit]\nDescription=periwire USB forwarding daemon (user)\n\n[Service]\nExecStart={exe} daemon\nRestart=on-failure\nTimeoutStopSec=20\n\n[Install]\nWantedBy=default.target\n")
    } else {
        format!("[Unit]\nDescription=periwire USB forwarding daemon\nAfter=network-online.target\nWants=network-online.target\n\n[Service]\nExecStart={exe} daemon --socket-group periwire\nEnvironment=PERIWIRE_HOME=/var/lib/periwire\nStateDirectory=periwire\nRestart=on-failure\nKillSignal=SIGTERM\nTimeoutStopSec=20\n\n[Install]\nWantedBy=multi-user.target\n")
    }
}

/// Find a local device's label key from a busid or vid:pid.
pub async fn device_key(b: &mut Backend, spec: &str) -> Result<String> {
    let devs = call(b, "devices", json!({})).await?;
    let list = devs.as_array().cloned().unwrap_or_default();
    let m: Vec<&Value> = list.iter().filter(|d| s(d, "busid") == spec || format!("{}:{}", s(d, "vid"), s(d, "pid")).eq_ignore_ascii_case(spec)).collect();
    match m.len() {
        0 => bail!("no local device matches `{spec}` (see `periwire devices`)"),
        1 => Ok(s(m[0], "key").to_string()),
        _ => bail!("`{spec}` matches several devices; use the busid"),
    }
}
