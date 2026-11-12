//! How commands reach the engine: through the running daemon, or (for read-only/offline commands)
//! an in-process instance that answers from local state.
use crate::api::{self, ApiError};
use crate::daemon::Daemon;
use serde_json::Value;
use std::sync::Arc;
use periwire_client::{Client, ConnectError};

pub enum Backend {
    Remote(Client),
    Local(Arc<Daemon>),
}

impl Backend {
    /// `need_daemon`: fail (with a helpful message) instead of falling back to local state.
    pub async fn open(need_daemon: bool) -> Result<Self, ApiError> {
        match Client::connect_default().await {
            Ok(c) => Ok(Backend::Remote(c)),
            Err(ConnectError::NotRunning(p)) if !need_daemon => {
                let _ = p;
                Daemon::new(false).map(Backend::Local).map_err(|e| ApiError::new("failed", format!("{e:#}")))
            }
            Err(e @ ConnectError::NotRunning(_)) => Err(ApiError::new("not_running", e.to_string())),
            Err(e) => Err(ApiError::new("denied", e.to_string())),
        }
    }

    pub fn has_daemon(&self) -> bool {
        matches!(self, Backend::Remote(_))
    }

    pub async fn call(&mut self, method: &str, params: Value) -> Result<Value, ApiError> {
        match self {
            Backend::Remote(c) => c.call(method, params).await,
            Backend::Local(d) => api::dispatch(d, method, &params).await,
        }
    }
}
