//! Transporte: QUIC (UDP) com TLS 1.3. Tudo que trafega (vídeo, áudio, senha) é criptografado.
//! O host tem uma identidade fixa (certificado salvo em %APPDATA%\GameStream) e o convite carrega
//! a impressão digital (SHA-256) desse certificado: o viewer recusa qualquer outro (anti-MITM).

use std::{
    net::{Ipv6Addr, SocketAddr},
    path::PathBuf,
    sync::Arc,
    time::Duration,
};

use anyhow::{bail, Result};
use quinn::{ClientConfig, Endpoint, EndpointConfig, ServerConfig, TransportConfig, VarInt};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, UnixTime};
use sha2::{Digest, Sha256};
use socket2::{Domain, Protocol, Socket, Type};

pub const DEFAULT_PORT: u16 = 9000;
pub const STREAM_VIDEO: u8 = 1;
pub const STREAM_AUDIO: u8 = 2;

pub struct Identity {
    cert: Vec<u8>,
    key: Vec<u8>,
    pub fingerprint: [u8; 32],
}

/// Carrega (ou cria na primeira vez) a identidade do host.
pub fn load_identity() -> Result<Identity> {
    let dir = PathBuf::from(std::env::var("APPDATA").unwrap_or_else(|_| ".".into())).join("GameStream");
    let (cp, kp) = (dir.join("host-cert.der"), dir.join("host-key.der"));
    let (cert, key) = match (std::fs::read(&cp), std::fs::read(&kp)) {
        (Ok(c), Ok(k)) => (c, k),
        _ => {
            let ck = rcgen::generate_simple_self_signed(vec!["gamestream".into()])?;
            let (c, k) = (ck.cert.der().to_vec(), ck.key_pair.serialize_der());
            std::fs::create_dir_all(&dir)?;
            std::fs::write(&cp, &c)?;
            std::fs::write(&kp, &k)?;
            (c, k)
        }
    };
    let fingerprint = Sha256::digest(&cert).into();
    Ok(Identity { cert, key, fingerprint })
}

fn transport() -> Arc<TransportConfig> {
    let mut t = TransportConfig::default();
    t.keep_alive_interval(Some(Duration::from_secs(2)));
    t.max_idle_timeout(Some(Duration::from_secs(10).try_into().unwrap()));
    t.stream_receive_window(VarInt::from_u32(8 * 1024 * 1024));
    t.receive_window(VarInt::from_u32(16 * 1024 * 1024));
    t.send_window(16 * 1024 * 1024);
    Arc::new(t)
}

/// Socket UDP dual-stack (IPv6 + IPv4). Se o PC não tiver IPv6, cai para só IPv4.
fn bind_dual(port: u16) -> Result<std::net::UdpSocket> {
    let v6 = || -> std::io::Result<std::net::UdpSocket> {
        let s = Socket::new(Domain::IPV6, Type::DGRAM, Some(Protocol::UDP))?;
        s.set_only_v6(false)?;
        s.bind(&SocketAddr::from((Ipv6Addr::UNSPECIFIED, port)).into())?;
        Ok(s.into())
    };
    let sock = match v6() {
        Ok(s) => s,
        Err(_) => std::net::UdpSocket::bind(SocketAddr::from(([0, 0, 0, 0], port)))?,
    };
    sock.set_nonblocking(true)?;
    Ok(sock)
}

pub fn server_endpoint(port: u16, id: &Identity) -> Result<Endpoint> {
    let mut cfg = ServerConfig::with_single_cert(
        vec![CertificateDer::from(id.cert.clone())],
        PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(id.key.clone())),
    )?;
    cfg.transport_config(transport());
    Ok(Endpoint::new(EndpointConfig::default(), Some(cfg), bind_dual(port)?, Arc::new(quinn::TokioRuntime))?)
}

/// `pin` = impressão digital esperada do host (vem do convite). Sem pin, aceita qualquer host.
pub fn client_endpoint(pin: Option<[u8; 32]>) -> Result<Endpoint> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let crypto = rustls::ClientConfig::builder_with_provider(provider.clone())
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(PinnedVerifier { provider, pin }))
        .with_no_client_auth();
    let mut cfg = ClientConfig::new(Arc::new(quinn::crypto::rustls::QuicClientConfig::try_from(crypto)?));
    cfg.transport_config(transport());
    let mut ep = Endpoint::new(EndpointConfig::default(), None, bind_dual(0)?, Arc::new(quinn::TokioRuntime))?;
    ep.set_default_client_config(cfg);
    Ok(ep)
}

/// Código curto (ex: "7F3A-91C2-0BDE") da impressão digital, para conferir por voz.
pub fn security_code(fp: &[u8; 32]) -> String {
    fp[..6]
        .chunks(2)
        .map(|c| format!("{:02X}{:02X}", c[0], c[1]))
        .collect::<Vec<_>>()
        .join("-")
}

/// Código de segurança do host com quem esta conexão foi feita (a partir do certificado recebido).
pub fn peer_security_code(conn: &quinn::Connection) -> Option<String> {
    let certs = conn.peer_identity()?.downcast::<Vec<CertificateDer<'static>>>().ok()?;
    let fp: [u8; 32] = Sha256::digest(certs.first()?.as_ref()).into();
    Some(security_code(&fp))
}

/// Escreve uma mensagem `[u32 tamanho][dados]`.
pub fn frame(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + data.len());
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(data);
    out
}

/// Lê uma mensagem `[u32 tamanho][dados]`. Retorna `false` quando o stream terminou.
pub async fn read_msg(r: &mut quinn::RecvStream, buf: &mut Vec<u8>) -> Result<bool> {
    let mut len = [0u8; 4];
    match r.read_exact(&mut len).await {
        Ok(()) => {}
        Err(quinn::ReadExactError::FinishedEarly(_)) => return Ok(false),
        Err(e) => return Err(e.into()),
    }
    let n = u32::from_le_bytes(len) as usize;
    if n > 32 << 20 {
        bail!("mensagem grande demais ({n} bytes)");
    }
    buf.resize(n, 0);
    r.read_exact(buf).await?;
    Ok(true)
}

#[derive(Debug)]
struct PinnedVerifier {
    provider: Arc<rustls::crypto::CryptoProvider>,
    pin: Option<[u8; 32]>,
}

impl rustls::client::danger::ServerCertVerifier for PinnedVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        if let Some(pin) = self.pin {
            let fp: [u8; 32] = Sha256::digest(end_entity.as_ref()).into();
            if fp != pin {
                return Err(rustls::Error::General(
                    "a identidade do host não confere com o convite (possível interceptação!)".into(),
                ));
            }
        }
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    // As assinaturas são sempre verificadas: prova que o host tem a chave privada do certificado.
    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.provider.signature_verification_algorithms.supported_schemes()
    }
}
