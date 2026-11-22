//! Listening for importers: binding, the TLS handshake with its limits, and the accept loop.
use super::*;

pub async fn bind(listen: &str) -> Result<TcpListener> {
    TcpListener::bind(listen).await.map_err(|e| anyhow!("cannot listen on {listen}: {e}"))
}

/// How long a connection may take to finish the TLS handshake, and how many may be in that state at once. Anyone who can reach the port can
/// open connections without credentials; without these limits a peer that never finishes the handshake would hold a task and a file descriptor forever.
pub(super) const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
pub(super) const MAX_HANDSHAKES: usize = 32;

/// Run the TLS handshake, but give up after `limit`.
pub(super) async fn accept_tls<S: AsyncReadExt + AsyncWriteExt + Unpin>(acceptor: &tokio_rustls::TlsAcceptor, stream: S, limit: Duration) -> Result<tokio_rustls::server::TlsStream<S>> {
    match tokio::time::timeout(limit, acceptor.accept(stream)).await {
        Ok(r) => Ok(r?),
        Err(_) => bail!("the TLS handshake did not finish within {} s", limit.as_secs_f32()),
    }
}

/// Accept importers until `stop` fires; then let sessions wind down (their devices go back to this host).
pub async fn serve(shared: Arc<Shared>, listener: TcpListener, mut stop: watch::Receiver<bool>) -> Result<()> {
    let active = Arc::new(AtomicUsize::new(0));
    let handshakes = Arc::new(tokio::sync::Semaphore::new(MAX_HANDSHAKES));
    loop {
        let (tcp, peer) = tokio::select! {
            r = listener.accept() => match r {
                Ok(c) => c,
                Err(e) => {
                    // Running out of file descriptors or a connection reset while queued is not a reason to stop sharing.
                    crate::logln!("accepting a connection failed ({e}); trying again shortly");
                    tokio::time::sleep(Duration::from_millis(200)).await;
                    continue;
                }
            },
            _ = stop.changed() => break,
        };
        let _ = tcp.set_nodelay(true);
        // Too many connections still unauthenticated: drop this one at once rather than queue work for strangers.
        let Ok(permit) = handshakes.clone().try_acquire_owned() else {
            crate::logln!("[{peer}] dropped: {MAX_HANDSHAKES} connections are already in the middle of a handshake");
            continue;
        };
        // Re-read identity and trust list so a reset or a new pairing applies at once; a read error loses this connection, not the exporter.
        let cfg = match identity::load_or_create().and_then(|id| Ok((id, identity::load_trusted()?))).and_then(|(id, trusted)| tls::server_config(&id, trusted)) {
            Ok(c) => c,
            Err(e) => {
                crate::logln!("[{peer}] dropped: could not load this computer's identity or trust list: {e:#}");
                continue;
            }
        };
        let (sd, active, shared) = (stop.clone(), active.clone(), shared.clone());
        active.fetch_add(1, Ordering::SeqCst);
        tokio::spawn(async move {
            let acceptor = tokio_rustls::TlsAcceptor::from(cfg);
            match accept_tls(&acceptor, tcp, HANDSHAKE_TIMEOUT).await {
                Ok(s) => {
                    drop(permit); // authenticated: no longer counts against the handshake limit
                    if let Err(e) = handle(s, shared, peer.to_string(), sd).await {
                        crate::logln!("[{peer}] connection ended: {e:#}");
                    }
                }
                Err(e) => crate::logln!("[{peer}] TLS failed: {e}"),
            }
            active.fetch_sub(1, Ordering::SeqCst);
        });
    }
    for _ in 0..100 {
        if active.load(Ordering::SeqCst) == 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    // Devices still waiting for an importer go back to this machine now (the daemon is stopping).
    let held = shared.parked.lock().unwrap().drain();
    drop(held);
    super::hostprep::restore_all(); // whatever is still stepped aside comes back, even if a device was never released properly
    Ok(())
}

pub async fn wait_for_stop() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut term = signal(SignalKind::terminate()).expect("sigterm handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}
