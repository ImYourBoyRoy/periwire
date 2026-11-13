//! MCP (Model Context Protocol) server on stdio, so an AI assistant can drive periwire.
//! Each tool maps onto one control-API call; see docs/MCP.md for the tool reference.
use crate::backend::Backend;
use crate::cli;
use anyhow::Result;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

const INSTRUCTIONS: &str = "\
periwire forwards USB devices between computers. One daemon per computer both SHARES local devices and IMPORTS (attaches) remote ones.
Flows: check periwire_doctor then periwire_status. Share: periwire_list_devices -> periwire_share. Use a remote device: periwire_list_remote -> periwire_attach, then periwire_eject when done (safe: unmounts first; force only if the user accepts data loss). Take a device back: periwire_disconnect_peer (abrupt).
Trust: computers authenticate by certificate fingerprint. periwire_trust_probe only READS a fingerprint. NEVER call periwire_trust_add unless the human compared that fingerprint with `periwire id` on the other computer and told you to trust it; a wrong trust lets someone else take the user's USB devices. periwire_reset_identity and periwire_driver_install also need the user's explicit request.
read_only on share/attach write-protects USB storage (other devices are refused); default is read and write.
A busid (2-2, 1-1.4) is a port on one computer; vid:pid (1234:5678) is a model. Importing needs the daemon with privileges (root on Linux, Administrator on Windows); status and sharing do not. Replies are compact; detail=true gives raw objects. Never format, repartition or write to a device for the user; periwire only moves the connection.";

/// Lean tool definition: annotations only where they differ from the MCP defaults (readOnly=false, destructive=true),
/// and no empty `required`, to keep the tool list cheap for the model.
fn tool(name: &str, desc: &str, props: Value, required: &[&str], ro: bool, destructive: bool) -> Value {
    let mut schema = json!({"type": "object", "properties": props});
    if !required.is_empty() {
        schema["required"] = json!(required);
    }
    let ann = if ro {
        json!({"readOnlyHint": true})
    } else if destructive {
        json!({})
    } else {
        json!({"destructiveHint": false})
    };
    let mut t = json!({"name": name, "description": desc, "inputSchema": schema});
    if ann.as_object().map(|o| !o.is_empty()).unwrap_or(false) {
        t["annotations"] = ann;
    }
    t
}

fn tools() -> Vec<Value> {
    let addr = json!({"type": "string", "description": "host or host:port (default port 3241)"});
    let busid = json!({"type": "string", "description": "port id like 2-2 (from periwire_list_devices / periwire_list_remote)"});
    let spec = json!({"type": "string", "description": "busid (2-2) or vid:pid (1234:5678)"});
    let fp = json!({"type": "string", "description": "64 hex characters from `periwire id`"});
    let ro = json!({"type": "boolean", "description": "write-protect USB storage (default false: read and write; other device kinds are refused)"});
    let confirmed = json!({"type": "boolean", "description": "true = the user explicitly asked for this"});
    vec![
        tool("periwire_status", "Compact snapshot: daemon, shares, who uses them, attached remote devices, trusted peers, local devices. detail=true returns the full raw objects.", json!({"detail": {"type": "boolean"}}), &[], true, false),
        tool("periwire_doctor", "Health checks. Returns only problems, each with a fix (verbose=true includes passing checks).", json!({"verbose": {"type": "boolean"}}), &[], true, false),
        tool("periwire_list_devices", "USB devices on THIS computer: busid, id, name, kind, shared, used_by, mounts. A device mounted here cannot be imported until ejected here. detail=true adds all fields.", json!({"detail": {"type": "boolean"}}), &[], true, false),
        tool("periwire_list_remote", "Devices another computer shares with us (needs mutual trust).", json!({"addr": addr}), &["addr"], true, false),
        tool("periwire_share", "Share a local device with trusted computers. read_only write-protects USB storage for every importer.", json!({"spec": spec, "read_only": ro}), &["spec"], false, false),
        tool("periwire_unshare", "Stop sharing a device; a current user is disconnected at once (abrupt, may lose data).", json!({"spec": spec}), &["spec"], false, true),
        tool("periwire_attach", "Import a remote device here as if plugged in. auto remembers it and reconnects. Needs the daemon (root on Linux); waits up to 25 s.", json!({"addr": addr, "busid": busid, "auto": {"type": "boolean"}, "name": {"type": "string", "description": "label for a remembered connection"}, "read_only": ro}), &["addr", "busid"], false, false),
        tool("periwire_eject", "Safely eject an imported device (unmounts first; fails if files are open). force only if the user accepts possible data loss.", json!({"target": {"type": "string", "description": "attachment number, busid or vid:pid"}, "force": {"type": "boolean"}}), &["target"], false, true),
        tool("periwire_disconnect_peer", "Force a computer off one of OUR shared devices (abrupt; prefer asking them to eject).", json!({"target": {"type": "string", "description": "session number or busid"}}), &["target"], false, true),
        tool("periwire_trust_list", "Trusted computers (fingerprints).", json!({}), &[], true, false),
        tool("periwire_trust_probe", "Read the fingerprint a host presents WITHOUT trusting it. Show it to the user to compare with `periwire id` there.", json!({"addr": addr}), &["addr"], true, false),
        tool("periwire_trust_add", "Trust a computer. Only after the user compared its fingerprint out of band; set confirmed_by_user=true to attest.", json!({"fingerprint": fp, "name": {"type": "string"}, "confirmed_by_user": confirmed}), &["fingerprint", "confirmed_by_user"], false, true),
        tool("periwire_trust_remove", "Stop trusting a computer.", json!({"fingerprint": fp}), &["fingerprint"], false, true),
        tool("periwire_forget", "Forget a remembered (auto) connection.", json!({"addr": addr, "busid": busid}), &["addr", "busid"], false, true),
        tool("periwire_label_device", "Name a local device (peers see it). Empty clears. key comes from periwire_list_devices detail=true.", json!({"key": {"type": "string"}, "label": {"type": "string"}}), &["key", "label"], false, false),
        tool("periwire_reset_identity", "New fingerprint for THIS computer: all peers must re-trust it and sessions drop. Only on the user's explicit request (confirmed_by_user=true).", json!({"confirmed_by_user": confirmed}), &["confirmed_by_user"], false, true),
        tool("periwire_driver_status", "Windows only: is the virtual USB driver installed?", json!({}), &[], true, false),
        tool("periwire_driver_install", "Windows only: install the signed virtual USB driver PeriWire carries (kernel driver; USB 3 hubs restart briefly; daemon must be Administrator). Only on the user's request (confirmed_by_user=true).", json!({"confirmed_by_user": confirmed}), &["confirmed_by_user"], false, true),
        tool("periwire_export", "Turn the exporter on or off; off returns every shared device.", json!({"enabled": {"type": "boolean"}}), &["enabled"], false, true),
        tool("periwire_logs", "Recent daemon log lines.", json!({"lines": {"type": "integer", "minimum": 1, "maximum": 1000, "description": "default 50"}}), &[], true, false),
    ]
}

// ---- compact replies: drop noise, keep what a model needs to act ----

fn s_of(v: &Value, k: &str) -> String {
    v.get(k).and_then(Value::as_str).unwrap_or("").to_string()
}
fn set_if(o: &mut serde_json::Map<String, Value>, k: &str, cond: bool, v: Value) {
    if cond {
        o.insert(k.into(), v);
    }
}
/// Remove nulls and empty containers (but keep false/0, which carry meaning).
fn prune(v: Value) -> Value {
    match v {
        Value::Object(m) => Value::Object(
            m.into_iter().map(|(k, v)| (k, prune(v))).filter(|(_, v)| !(v.is_null() || matches!(v, Value::Array(a) if a.is_empty()) || matches!(v, Value::Object(o) if o.is_empty()))).collect(),
        ),
        Value::Array(a) => Value::Array(a.into_iter().map(prune).collect()),
        x => x,
    }
}
fn dev_name(d: &Value) -> String {
    let n = s_of(d, "display");
    if n.is_empty() {
        format!("{}:{}", s_of(d, "vid"), s_of(d, "pid"))
    } else {
        n
    }
}
fn compact_device(d: &Value) -> Value {
    let mut o = serde_json::Map::new();
    o.insert("busid".into(), d["busid"].clone());
    o.insert("id".into(), json!(format!("{}:{}", s_of(d, "vid"), s_of(d, "pid"))));
    o.insert("name".into(), json!(dev_name(d)));
    o.insert("kind".into(), d["kind"].clone());
    set_if(&mut o, "speed", d["speed"].is_string(), json!(s_of(d, "speed").split_whitespace().next().unwrap_or("")));
    set_if(&mut o, "shared", d["shared"] == json!(true), json!(true));
    set_if(&mut o, "ro", d["shared_read_only"] == json!(true), json!(true));
    set_if(&mut o, "used_by", d["in_use_by"].is_string(), d["in_use_by"].clone());
    set_if(&mut o, "mounts", d["mounted"].as_array().map(|a| !a.is_empty()).unwrap_or(false), d["mounted"].clone());
    Value::Object(o)
}
fn compact_attachment(a: &Value) -> Value {
    let mut o = serde_json::Map::new();
    for k in ["id", "addr", "busid", "state", "error"] {
        o.insert(k.into(), a[k].clone());
    }
    o.insert("name".into(), json!(if a["info"].is_object() { dev_name(&a["info"]) } else { format!("{}:{}", s_of(a, "vid"), s_of(a, "pid")) }));
    set_if(&mut o, "ro", a["read_only"] == json!(true), json!(true));
    set_if(&mut o, "auto", a["auto"] == json!(true), json!(true));
    set_if(&mut o, "mounts", a["mounts"].as_array().map(|x| !x.is_empty()).unwrap_or(false), a["mounts"].clone());
    o.insert("in".into(), a["bytes_in"].clone());
    o.insert("out".into(), a["bytes_out"].clone());
    Value::Object(o)
}
fn compact_status(st: &Value, devices: &Value) -> Value {
    let arr = |v: &Value| v.as_array().cloned().unwrap_or_default();
    let attached = arr(&st["attachments"]);
    let sessions: Vec<Value> = arr(&st["sessions"])
        .iter()
        .map(|s| {
            let mut o = serde_json::Map::new();
            o.insert("id".into(), s["id"].clone());
            o.insert("peer".into(), json!(if s_of(s, "peer_name").is_empty() { s_of(s, "peer") } else { s_of(s, "peer_name") }));
            o.insert("busid".into(), s["busid"].clone());
            o.insert("name".into(), json!(if s["info"].is_object() { dev_name(&s["info"]) } else { format!("{}:{}", s_of(s, "vid"), s_of(s, "pid")) }));
            set_if(&mut o, "ro", s["read_only"] == json!(true), json!(true));
            Value::Object(o)
        })
        .collect();
    let remembered: Vec<Value> = arr(&st["connections"])
        .iter()
        .filter(|c| !attached.iter().any(|a| a["addr"] == c["addr"] && a["busid"] == c["busid"]))
        .map(|c| json!({"addr": c["addr"], "busid": c["busid"], "name": c["name"], "ro": c["read_only"] == json!(true), "auto": c["auto"] == json!(true)}))
        .collect();
    prune(json!({
        "version": st["version"], "os": st["os"], "host": st["hostname"], "root": st["root"],
        "export": {"enabled": st["export"]["enabled"], "state": st["export"]["state"], "listen": st["export"]["listen"], "error": st["export"]["error"]},
        "import": {"ok": st["import"]["available"], "why": st["import"]["reason"]},
        "shares": arr(&st["shares"]).iter().map(|x| json!({"spec": x["spec"], "present": x["present"], "ro": x["read_only"] == json!(true)})).collect::<Vec<_>>(),
        "sessions": sessions,
        "attached": attached.iter().map(compact_attachment).collect::<Vec<_>>(),
        "remembered": remembered,
        "trusted": arr(&st["trusted"]).iter().map(|t| json!({"name": t["name"], "fp": s_of(t, "fingerprint").chars().take(16).collect::<String>()})).collect::<Vec<_>>(),
        "devices": arr(devices).iter().map(compact_device).collect::<Vec<_>>(),
    }))
}

fn hint(code: &str) -> &'static str {
    match code {
        "not_running" => " Start the daemon with `periwire daemon` (as root on Linux for importing) or install it with `periwire service install`.",
        "denied" => " The daemon's control socket belongs to another user; run it with `--socket-group` or use sudo.",
        "unsupported" => " Run periwire_doctor for details.",
        _ => "",
    }
}

async fn call_tool(name: &str, a: &Value) -> Result<Value, (String, String)> {
    let need = matches!(name, "periwire_attach" | "periwire_eject" | "periwire_disconnect_peer" | "periwire_export" | "periwire_logs");
    let mut b = Backend::open(need).await.map_err(|e| (e.code.clone(), e.message))?;
    let e2 = |e: crate::api::ApiError| (e.code, e.message);
    let ea = |e: anyhow::Error| ("failed".to_string(), format!("{e:#}"));
    let g = |k: &str| a.get(k).cloned().unwrap_or(Value::Null);
    match name {
        "periwire_status" => {
            let mut st = b.call("status", json!({})).await.map_err(e2)?;
            let devs = b.call("devices", json!({})).await.unwrap_or(json!([]));
            if a.get("detail").and_then(Value::as_bool) == Some(true) {
                st["devices"] = devs;
                Ok(st)
            } else {
                Ok(compact_status(&st, &devs))
            }
        }
        "periwire_doctor" => {
            let all = b.call("doctor", json!({})).await.map_err(e2)?;
            let checks = all.as_array().cloned().unwrap_or_default();
            let bad = |c: &Value| matches!(c["status"].as_str(), Some("warn") | Some("fail"));
            let n_bad = checks.iter().filter(|c| bad(c)).count();
            let list: Vec<Value> = checks
                .iter()
                .filter(|c| a.get("verbose").and_then(Value::as_bool) == Some(true) || bad(c))
                .map(|c| prune(json!({"id": c["id"], "status": c["status"], "detail": c["detail"], "fix": c["fix"]})))
                .collect();
            Ok(json!({"ok": checks.len() - n_bad, "problems": n_bad, "checks": list}))
        }
        "periwire_list_devices" => {
            let d = b.call("devices", json!({})).await.map_err(e2)?;
            if a.get("detail").and_then(Value::as_bool) == Some(true) {
                Ok(json!({"devices": d}))
            } else {
                Ok(prune(json!({"devices": d.as_array().cloned().unwrap_or_default().iter().map(compact_device).collect::<Vec<_>>()})))
            }
        }
        "periwire_list_remote" => {
            let d = b.call("remote.list", json!({"addr": g("addr")})).await.map_err(e2)?;
            Ok(prune(json!({"devices": d.as_array().cloned().unwrap_or_default().iter().map(compact_device).collect::<Vec<_>>()})))
        }
        "periwire_share" => b.call("share.add", json!({"spec": g("spec"), "read_only": g("read_only")})).await.map_err(e2),
        "periwire_unshare" => b.call("share.remove", json!({"spec": g("spec")})).await.map_err(e2),
        "periwire_attach" => {
            Ok(prune(compact_attachment(&b.call("attach", json!({"addr": g("addr"), "busid": g("busid"), "auto": g("auto"), "name": g("name"), "read_only": g("read_only")})).await.map_err(e2)?)))
        }
        "periwire_eject" => {
            let t = a.get("target").and_then(Value::as_str).unwrap_or("");
            let att = cli::resolve_attachment(&mut b, t).await.map_err(ea)?;
            b.call("detach", json!({"id": att["id"], "force": a.get("force").and_then(Value::as_bool).unwrap_or(false)})).await.map_err(e2)
        }
        "periwire_disconnect_peer" => {
            let t = a.get("target").and_then(Value::as_str).unwrap_or("");
            let se = cli::resolve_session(&mut b, t).await.map_err(ea)?;
            b.call("kick", json!({"session": se["id"]})).await.map_err(e2)
        }
        "periwire_trust_list" => Ok(json!({"trusted": b.call("trust.list", json!({})).await.map_err(e2)?})),
        "periwire_trust_probe" => b.call("trust.probe", json!({"addr": g("addr")})).await.map_err(e2),
        "periwire_trust_add" => {
            if a.get("confirmed_by_user").and_then(Value::as_bool) != Some(true) {
                return Err(("denied".into(), "refusing to trust a computer without the user's confirmation: show the fingerprint to the user, have them compare it with `periwire id` on the other computer, then call again with confirmed_by_user=true".into()));
            }
            b.call("trust.add", json!({"fingerprint": g("fingerprint"), "name": a.get("name").cloned().unwrap_or(json!("peer"))})).await.map_err(e2)
        }
        "periwire_trust_remove" => b.call("trust.remove", json!({"fingerprint": g("fingerprint")})).await.map_err(e2),
        "periwire_forget" => b.call("connection.remove", json!({"addr": g("addr"), "busid": g("busid")})).await.map_err(e2),
        "periwire_label_device" => b.call("device.label", json!({"key": g("key"), "label": g("label")})).await.map_err(e2),
        "periwire_reset_identity" => {
            if a.get("confirmed_by_user").and_then(Value::as_bool) != Some(true) {
                return Err(("denied".into(), "refusing to reset the identity without the user's explicit confirmation".into()));
            }
            b.call("identity.reset", json!({"confirm": true})).await.map_err(e2)
        }
        "periwire_driver_status" => b.call("driver.status", json!({})).await.map_err(e2),
        "periwire_driver_install" => {
            if a.get("confirmed_by_user").and_then(Value::as_bool) != Some(true) {
                return Err(("denied".into(), "refusing to install a kernel driver without the user's explicit confirmation".into()));
            }
            b.call("driver.install", json!({"confirm": true})).await.map_err(e2)
        }
        "periwire_export" => b.call("export.set", json!({"enabled": g("enabled")})).await.map_err(e2),
        "periwire_logs" => Ok(json!({"lines": b.call("logs", json!({"lines": a.get("lines").cloned().unwrap_or(json!(50))})).await.map_err(e2)?})),
        _ => Err(("not_found".into(), format!("unknown tool {name}"))),
    }
}

fn reply(id: &Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}
fn rpc_error(id: &Value, code: i32, msg: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": msg}})
}

pub async fn serve() -> Result<()> {
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    let mut out = tokio::io::stdout();
    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let Ok(req) = serde_json::from_str::<Value>(&line) else {
            let r = rpc_error(&Value::Null, -32700, "parse error");
            out.write_all(format!("{r}\n").as_bytes()).await?;
            continue;
        };
        let Some(id) = req.get("id").cloned() else { continue }; // notification
        let method = req["method"].as_str().unwrap_or("");
        let resp = match method {
            "initialize" => {
                let want = req["params"]["protocolVersion"].as_str().unwrap_or("2025-06-18");
                let ver = if ["2025-06-18", "2025-03-26", "2024-11-05"].contains(&want) { want } else { "2025-06-18" };
                reply(
                    &id,
                    json!({"protocolVersion": ver, "capabilities": {"tools": {"listChanged": false}},
                    "serverInfo": {"name": "periwire", "title": "periwire USB forwarding", "version": crate::daemon::VERSION}, "instructions": INSTRUCTIONS}),
                )
            }
            "ping" => reply(&id, json!({})),
            "tools/list" => reply(&id, json!({"tools": tools()})),
            "tools/call" => {
                let name = req["params"]["name"].as_str().unwrap_or("");
                let args = req["params"].get("arguments").cloned().unwrap_or(json!({}));
                match call_tool(name, &args).await {
                    Ok(v) => reply(&id, json!({"content": [{"type": "text", "text": serde_json::to_string(&v)?}], "isError": false})),
                    Err((code, msg)) => reply(&id, json!({"content": [{"type": "text", "text": format!("{msg} [{code}].{}", hint(&code))}], "isError": true})),
                }
            }
            "resources/list" => reply(&id, json!({"resources": []})),
            "prompts/list" => reply(&id, json!({"prompts": []})),
            m => rpc_error(&id, -32601, &format!("method not found: {m}")),
        };
        out.write_all(format!("{resp}\n").as_bytes()).await?;
        out.flush().await?;
    }
    Ok(())
}
