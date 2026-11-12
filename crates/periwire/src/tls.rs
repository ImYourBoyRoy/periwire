//! Mutual TLS 1.3 with self-signed certificates and fingerprint pinning (no CA, no passwords).
use crate::identity::{fingerprint, Identity};
use anyhow::Result;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{verify_tls12_signature, verify_tls13_signature, CryptoProvider, WebPkiSupportedAlgorithms};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, UnixTime};
use rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
use rustls::{ClientConfig, DigitallySignedStruct, DistinguishedName, Error, ServerConfig, SignatureScheme};
use std::{collections::HashSet, sync::{Arc, Mutex}};

fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

/// Accepts a peer only if its certificate fingerprint is pinned. In `learn` mode (used by
/// `trust-host`) it records the fingerprint instead of rejecting.
#[derive(Debug)]
pub struct Pinned {
    trusted: HashSet<String>,
    algs: WebPkiSupportedAlgorithms,
    learn: Option<Arc<Mutex<Option<String>>>>,
    peer_kind: &'static str,
}

impl Pinned {
    pub fn new(trusted: HashSet<String>, peer_kind: &'static str) -> Self {
        Self { trusted, algs: provider().signature_verification_algorithms, learn: None, peer_kind }
    }
    pub fn learning(slot: Arc<Mutex<Option<String>>>) -> Self {
        Self { trusted: HashSet::new(), algs: provider().signature_verification_algorithms, learn: Some(slot), peer_kind: "server" }
    }
    fn check(&self, der: &CertificateDer<'_>) -> Result<(), Error> {
        let fp = fingerprint(der);
        if let Some(slot) = &self.learn {
            *slot.lock().unwrap() = Some(fp);
            return Ok(());
        }
        if self.trusted.contains(&fp) {
            Ok(())
        } else {
            eprintln!("rejected untrusted {} {fp}\n  to allow it, run: periwire trust {fp}", self.peer_kind);
            Err(Error::General("untrusted peer".into()))
        }
    }
}

impl ServerCertVerifier for Pinned {
    fn verify_server_cert(&self, end_entity: &CertificateDer<'_>, _i: &[CertificateDer<'_>], _n: &ServerName<'_>, _o: &[u8], _t: UnixTime) -> Result<ServerCertVerified, Error> {
        self.check(end_entity)?;
        Ok(ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(&self, m: &[u8], c: &CertificateDer<'_>, d: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, Error> {
        verify_tls12_signature(m, c, d, &self.algs)
    }
    fn verify_tls13_signature(&self, m: &[u8], c: &CertificateDer<'_>, d: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, Error> {
        verify_tls13_signature(m, c, d, &self.algs)
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.algs.supported_schemes()
    }
}

impl ClientCertVerifier for Pinned {
    fn offer_client_auth(&self) -> bool { true }
    fn client_auth_mandatory(&self) -> bool { true }
    fn root_hint_subjects(&self) -> &[DistinguishedName] { &[] }
    fn verify_client_cert(&self, end_entity: &CertificateDer<'_>, _i: &[CertificateDer<'_>], _t: UnixTime) -> Result<ClientCertVerified, Error> {
        self.check(end_entity)?;
        Ok(ClientCertVerified::assertion())
    }
    fn verify_tls12_signature(&self, m: &[u8], c: &CertificateDer<'_>, d: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, Error> {
        verify_tls12_signature(m, c, d, &self.algs)
    }
    fn verify_tls13_signature(&self, m: &[u8], c: &CertificateDer<'_>, d: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, Error> {
        verify_tls13_signature(m, c, d, &self.algs)
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.algs.supported_schemes()
    }
}

fn key_der(id: &Identity) -> PrivateKeyDer<'static> {
    PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(id.key.clone()))
}

pub fn server_config(id: &Identity, trusted: HashSet<String>) -> Result<Arc<ServerConfig>> {
    let cfg = ServerConfig::builder_with_provider(provider())
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .with_client_cert_verifier(Arc::new(Pinned::new(trusted, "client")))
        .with_single_cert(vec![CertificateDer::from(id.cert.clone())], key_der(id))?;
    Ok(Arc::new(cfg))
}

pub fn client_config(id: &Identity, verifier: Pinned) -> Result<Arc<ClientConfig>> {
    let cfg = ClientConfig::builder_with_provider(provider())
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(verifier))
        .with_client_auth_cert(vec![CertificateDer::from(id.cert.clone())], key_der(id))?;
    Ok(Arc::new(cfg))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn ident() -> Identity {
        let ck = rcgen::generate_simple_self_signed(vec!["periwire".to_string()]).unwrap();
        let cert = ck.cert.der().to_vec();
        Identity { fingerprint: fingerprint(&cert), cert, key: ck.key_pair.serialize_der() }
    }

    async fn roundtrip(server_trusts: Option<&Identity>, client_trusts: Option<&Identity>, srv: &Identity, cli: &Identity) -> bool {
        let st: HashSet<String> = server_trusts.map(|i| i.fingerprint.clone()).into_iter().collect();
        let ct: HashSet<String> = client_trusts.map(|i| i.fingerprint.clone()).into_iter().collect();
        let acceptor = tokio_rustls::TlsAcceptor::from(server_config(srv, st).unwrap());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let mut s = acceptor.accept(tcp).await.ok()?;
            let mut b = [0u8; 4];
            s.read_exact(&mut b).await.ok()?;
            s.write_all(&b).await.ok()?;
            s.flush().await.ok()?;
            Some(())
        });
        let cfg = client_config(cli, Pinned::new(ct, "server")).unwrap();
        let tcp = tokio::net::TcpStream::connect(addr).await.unwrap();
        let name = rustls::pki_types::ServerName::try_from("periwire").unwrap();
        let ok = async {
            let mut s = tokio_rustls::TlsConnector::from(cfg).connect(name, tcp).await.ok()?;
            s.write_all(b"ping").await.ok()?;
            let mut b = [0u8; 4];
            s.read_exact(&mut b).await.ok()?;
            (&b == b"ping").then_some(())
        }
        .await
        .is_some();
        let _ = server.await;
        ok
    }

    #[tokio::test]
    async fn mutual_pinning_accepts_trusted_peers() {
        let (s, c) = (ident(), ident());
        assert!(roundtrip(Some(&c), Some(&s), &s, &c).await);
    }

    #[tokio::test]
    async fn rejects_untrusted_server() {
        let (s, c) = (ident(), ident());
        assert!(!roundtrip(Some(&c), None, &s, &c).await);
    }

    #[tokio::test]
    async fn rejects_untrusted_client() {
        let (s, c) = (ident(), ident());
        assert!(!roundtrip(None, Some(&s), &s, &c).await);
    }

    #[tokio::test]
    async fn rejects_wrong_pinned_server() {
        let (s, c, other) = (ident(), ident(), ident());
        assert!(!roundtrip(Some(&c), Some(&other), &s, &c).await);
    }
}
