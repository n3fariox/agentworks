//! Optional HTTPS for the browser listener, with opt-in client-certificate enforcement.
use axum::{extract::connect_info::Connected, serve::IncomingStream};
use std::{io, net::SocketAddr, path::PathBuf, sync::Arc, time::Duration};
use tokio::{
    net::{TcpListener, TcpStream},
    sync::mpsc,
};
use tokio_rustls::{
    TlsAcceptor,
    rustls::{
        self, RootCertStore,
        pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject},
        server::WebPkiClientVerifier,
    },
    server::TlsStream,
};
use tokio_util::either::Either;

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

pub struct Config {
    cert: PathBuf,
    key: PathBuf,
    client_ca: Option<PathBuf>,
}

impl Config {
    /// `None` serves plain HTTP. Partial settings are errors, so a typo never
    /// silently falls back to HTTP.
    pub fn from_env() -> Result<Option<Self>, String> {
        let var = |name| {
            std::env::var_os(name)
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
        };
        match (
            var("AGENTWORKS_TLS_CERT"),
            var("AGENTWORKS_TLS_KEY"),
            var("AGENTWORKS_TLS_CLIENT_CA"),
        ) {
            (Some(cert), Some(key), client_ca) => Ok(Some(Self {
                cert,
                key,
                client_ca,
            })),
            (None, None, None) => Ok(None),
            _ => Err("AGENTWORKS_TLS_CERT and AGENTWORKS_TLS_KEY must be set together, and AGENTWORKS_TLS_CLIENT_CA requires both".into()),
        }
    }

    pub fn requires_client_cert(&self) -> bool {
        self.client_ca.is_some()
    }

    pub fn acceptor(&self) -> Result<TlsAcceptor, String> {
        let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
        let certs = read_certs(&self.cert)?;
        let key = PrivateKeyDer::from_pem_file(&self.key)
            .map_err(|e| format!("{}: {e}", self.key.display()))?;
        let builder = rustls::ServerConfig::builder_with_provider(provider.clone())
            .with_safe_default_protocol_versions()
            .map_err(|e| e.to_string())?;
        let builder = match &self.client_ca {
            Some(path) => {
                let mut roots = RootCertStore::empty();
                for cert in read_certs(path)? {
                    roots
                        .add(cert)
                        .map_err(|e| format!("{}: {e}", path.display()))?;
                }
                // Required by default: a handshake without a certificate signed by these roots fails.
                let verifier = WebPkiClientVerifier::builder_with_provider(roots.into(), provider)
                    .build()
                    .map_err(|e| format!("{}: {e}", path.display()))?;
                builder.with_client_cert_verifier(verifier)
            }
            None => builder.with_no_client_auth(),
        };
        let mut config = builder
            .with_single_cert(certs, key)
            .map_err(|e| format!("{}: {e}", self.cert.display()))?;
        // axum serves HTTP/1.1 only; WebSocket upgrades depend on it.
        config.alpn_protocols = vec![b"http/1.1".to_vec()];
        Ok(TlsAcceptor::from(Arc::new(config)))
    }
}

fn read_certs(path: &PathBuf) -> Result<Vec<CertificateDer<'static>>, String> {
    let certs = CertificateDer::pem_file_iter(path)
        .and_then(|certs| certs.collect::<Result<Vec<_>, _>>())
        .map_err(|e| format!("{}: {e}", path.display()))?;
    if certs.is_empty() {
        return Err(format!("{}: no PEM certificates", path.display()));
    }
    Ok(certs)
}

pub type Io = Either<TcpStream, TlsStream<TcpStream>>;

/// Accepts plain or TLS connections. Handshakes run in their own tasks, so a
/// slow or stalled client never blocks other connections.
pub struct Listener {
    local: SocketAddr,
    ready: mpsc::Receiver<(Io, SocketAddr)>,
}

impl Listener {
    pub fn new(tcp: TcpListener, tls: Option<TlsAcceptor>) -> io::Result<Self> {
        let local = tcp.local_addr()?;
        let (tx, ready) = mpsc::channel(64);
        tokio::spawn(accept_loop(tcp, tls, tx));
        Ok(Self { local, ready })
    }
}

async fn accept_loop(
    tcp: TcpListener,
    tls: Option<TlsAcceptor>,
    tx: mpsc::Sender<(Io, SocketAddr)>,
) {
    loop {
        let (stream, peer) = match tcp.accept().await {
            Ok(connection) => connection,
            // Typically EMFILE; back off as axum's own listener does.
            Err(_) => {
                tokio::time::sleep(Duration::from_secs(1)).await;
                continue;
            }
        };
        let Some(acceptor) = tls.clone() else {
            if tx.send((Either::Left(stream), peer)).await.is_err() {
                return;
            }
            continue;
        };
        let tx = tx.clone();
        tokio::spawn(async move {
            match tokio::time::timeout(HANDSHAKE_TIMEOUT, acceptor.accept(stream)).await {
                Ok(Ok(stream)) => {
                    let _ = tx.send((Either::Right(stream), peer)).await;
                }
                Ok(Err(e)) => eprintln!("TLS handshake with {peer} failed: {e}"),
                Err(_) => eprintln!("TLS handshake with {peer} timed out"),
            }
        });
    }
}

impl axum::serve::Listener for Listener {
    type Io = Io;
    type Addr = SocketAddr;

    async fn accept(&mut self) -> (Io, SocketAddr) {
        match self.ready.recv().await {
            Some(connection) => connection,
            None => std::future::pending().await,
        }
    }

    fn local_addr(&self) -> io::Result<SocketAddr> {
        Ok(self.local)
    }
}

/// The browser's address, for sign-in logs and PAM's remote-host item.
#[derive(Clone, Copy, Debug)]
pub struct ClientAddr(pub SocketAddr);

impl Connected<IncomingStream<'_, Listener>> for ClientAddr {
    fn connect_info(stream: IncomingStream<'_, Listener>) -> Self {
        Self(*stream.remote_addr())
    }
}
