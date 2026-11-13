//! Client for the periwire daemon's control API.
//!
//! Wire format: newline-delimited JSON over a local socket.
//!   request : {"id":1,"method":"status","params":{}}
//!   response: {"id":1,"result":{...}}  or  {"id":1,"error":{"code":"not_found","message":"..."}}
//!   event   : {"event":"attachment.state","time":1700000000,"data":{...}}   (after `subscribe`)
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::path::PathBuf;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};

/// Error returned by the daemon for a request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiError {
    /// Machine-readable: invalid, not_found, busy, denied, unsupported, failed.
    pub code: String,
    pub message: String,
}
impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}
impl std::error::Error for ApiError {}
impl ApiError {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self { code: code.into(), message: message.into() }
    }
}

/// Why connecting to the daemon failed.
#[derive(Debug)]
pub enum ConnectError {
    /// No daemon socket found/accepting. Carries the paths tried.
    NotRunning(Vec<PathBuf>),
    /// Socket exists but we may not use it.
    Denied(PathBuf),
    Io(std::io::Error),
}
impl std::fmt::Display for ConnectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConnectError::NotRunning(p) => {
                write!(f, "the periwire daemon is not running (looked for {}). Start it with `periwire daemon`.", p.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(", "))
            }
            ConnectError::Denied(p) => write!(f, "permission denied on {}. The daemon runs as another user: run `periwire daemon --socket-group <group>` or use sudo.", p.display()),
            ConnectError::Io(e) => write!(f, "{e}"),
        }
    }
}
impl std::error::Error for ConnectError {}

/// Windows: the daemon listens on this named pipe instead of a Unix socket.
pub const PIPE_NAME: &str = r"\\.\pipe\periwire";

/// Where the daemon creates its control socket by default (`bind` side).
pub fn default_socket_path(as_root: bool, home: &std::path::Path) -> PathBuf {
    if let Some(p) = std::env::var_os("PERIWIRE_SOCKET") {
        return p.into();
    }
    if cfg!(windows) {
        return PIPE_NAME.into();
    }
    if as_root {
        return if cfg!(target_os = "macos") { "/var/run/periwire/periwire.sock".into() } else { "/run/periwire/periwire.sock".into() };
    }
    if let Some(d) = std::env::var_os("XDG_RUNTIME_DIR") {
        return PathBuf::from(d).join("periwire.sock");
    }
    home.join("periwire.sock")
}

/// Places a client looks for a running daemon, most specific first.
pub fn socket_candidates() -> Vec<PathBuf> {
    let mut v = vec![];
    if let Some(p) = std::env::var_os("PERIWIRE_SOCKET") {
        v.push(p.into());
        return v;
    }
    if cfg!(windows) {
        v.push(PIPE_NAME.into());
        return v;
    }
    v.push("/run/periwire/periwire.sock".into());
    v.push("/var/run/periwire/periwire.sock".into());
    if let Some(d) = std::env::var_os("XDG_RUNTIME_DIR") {
        v.push(PathBuf::from(d).join("periwire.sock"));
    }
    if let Some(c) = dirs::config_dir() {
        v.push(c.join("periwire").join("periwire.sock"));
    }
    v
}

type Rd = BufReader<Box<dyn AsyncRead + Send + Unpin>>;
type Wr = Box<dyn AsyncWrite + Send + Unpin>;

pub struct Client {
    r: Rd,
    w: Wr,
    next: u64,
    queued: VecDeque<Value>,
    pub path: PathBuf,
}

impl Client {
    pub async fn connect_default() -> Result<Self, ConnectError> {
        let cands = socket_candidates();
        for p in &cands {
            match Self::connect(p).await {
                Ok(c) => return Ok(c),
                Err(ConnectError::NotRunning(_)) => continue,
                Err(e) => return Err(e),
            }
        }
        Err(ConnectError::NotRunning(cands))
    }

    #[cfg(unix)]
    pub async fn connect(path: &std::path::Path) -> Result<Self, ConnectError> {
        match tokio::net::UnixStream::connect(path).await {
            Ok(s) => {
                let (r, w) = s.into_split();
                Ok(Self::from_halves(Box::new(r), Box::new(w), path))
            }
            Err(e) => Err(map_connect_err(e, path)),
        }
    }

    #[cfg(windows)]
    pub async fn connect(path: &std::path::Path) -> Result<Self, ConnectError> {
        use tokio::net::windows::named_pipe::ClientOptions;
        // ERROR_PIPE_BUSY (231): all instances are in use for a moment; retry briefly.
        for _ in 0..20 {
            match ClientOptions::new().open(path) {
                Ok(s) => {
                    let (r, w) = tokio::io::split(s);
                    return Ok(Self::from_halves(Box::new(r), Box::new(w), path));
                }
                Err(e) if e.raw_os_error() == Some(231) => tokio::time::sleep(std::time::Duration::from_millis(50)).await,
                Err(e) => return Err(map_connect_err(e, path)),
            }
        }
        Err(ConnectError::Io(std::io::Error::new(std::io::ErrorKind::TimedOut, "control pipe busy")))
    }

    #[cfg(not(any(unix, windows)))]
    pub async fn connect(_: &std::path::Path) -> Result<Self, ConnectError> {
        Err(ConnectError::Io(std::io::Error::new(std::io::ErrorKind::Unsupported, "control channel not implemented on this OS")))
    }

    fn from_halves(r: Box<dyn AsyncRead + Send + Unpin>, w: Wr, path: &std::path::Path) -> Self {
        Client { r: BufReader::new(r), w, next: 1, queued: VecDeque::new(), path: path.to_path_buf() }
    }

    /// Call a method and wait for its response. Events that arrive meanwhile are queued for `next_event`.
    pub async fn call(&mut self, method: &str, params: Value) -> Result<Value, ApiError> {
        let id = self.next;
        self.next += 1;
        let mut line = json!({"id": id, "method": method, "params": params}).to_string();
        line.push('\n');
        self.w.write_all(line.as_bytes()).await.map_err(|e| ApiError::new("failed", format!("daemon connection lost: {e}")))?;
        loop {
            let v = self.read_line().await.ok_or_else(|| ApiError::new("failed", "daemon closed the connection"))?;
            if v.get("event").is_some() {
                self.queued.push_back(v);
                continue;
            }
            if v.get("id").and_then(Value::as_u64) != Some(id) {
                continue;
            }
            if let Some(e) = v.get("error") {
                return Err(serde_json::from_value(e.clone()).unwrap_or_else(|_| ApiError::new("failed", e.to_string())));
            }
            return Ok(v.get("result").cloned().unwrap_or(Value::Null));
        }
    }

    /// Next pushed event (after calling `subscribe`). None when the daemon goes away.
    pub async fn next_event(&mut self) -> Option<Value> {
        if let Some(v) = self.queued.pop_front() {
            return Some(v);
        }
        loop {
            let v = self.read_line().await?;
            if v.get("event").is_some() {
                return Some(v);
            }
        }
    }

    async fn read_line(&mut self) -> Option<Value> {
        loop {
            let mut s = String::new();
            let n = self.r.read_line(&mut s).await.ok()?;
            if n == 0 {
                return None;
            }
            if let Ok(v) = serde_json::from_str::<Value>(s.trim()) {
                return Some(v);
            }
        }
    }
}

fn map_connect_err(e: std::io::Error, path: &std::path::Path) -> ConnectError {
    match e.kind() {
        std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused => ConnectError::NotRunning(vec![path.to_path_buf()]),
        std::io::ErrorKind::PermissionDenied => ConnectError::Denied(path.to_path_buf()),
        _ => ConnectError::Io(e),
    }
}
