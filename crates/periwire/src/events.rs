//! Process-wide log ring buffer and event bus (events are pushed to subscribed control clients).
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock};
use tokio::sync::broadcast;

pub fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

static BUS: OnceLock<broadcast::Sender<Value>> = OnceLock::new();
pub fn bus() -> &'static broadcast::Sender<Value> {
    BUS.get_or_init(|| broadcast::channel(256).0)
}

/// Push an event to every subscribed client. Kinds: `state` (anything changed; clients re-read status),
/// `attachment.ended`, `session.opened`, `session.closed`, `share.changed`, `log`.
pub fn emit(kind: &str, data: Value) {
    let _ = bus().send(json!({"event": kind, "time": now(), "data": data}));
}

static LOG: Mutex<VecDeque<String>> = Mutex::new(VecDeque::new());

fn hms(t: u64) -> String {
    format!("{:02}:{:02}:{:02}", (t / 3600) % 24, (t / 60) % 60, t % 60)
}

pub fn log(msg: String) {
    let line = format!("{} {msg}", hms(now()));
    eprintln!("{line}");
    let mut l = LOG.lock().unwrap();
    if l.len() >= 1000 {
        l.pop_front();
    }
    l.push_back(line.clone());
    drop(l);
    emit("log", json!({"line": line}));
}

pub fn recent(n: usize) -> Vec<String> {
    let l = LOG.lock().unwrap();
    l.iter().skip(l.len().saturating_sub(n)).cloned().collect()
}

#[macro_export]
macro_rules! logln {
    ($($a:tt)*) => { $crate::events::log(format!($($a)*)) };
}
