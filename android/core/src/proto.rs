//! Protocolo do GameStream (o mesmo do app do PC: src/net.rs, src/invite.rs, src/room.rs).

use std::{
    net::{IpAddr, Ipv6Addr, SocketAddr},
    sync::Arc,
    time::Duration,
};

use anyhow::{bail, Context, Result};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD as B64, Engine};
use quinn::{ClientConfig, Endpoint, EndpointConfig, TransportConfig, VarInt};
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use sha2::{Digest, Sha256};
use socket2::{Domain, Protocol, Socket, Type};

pub const PROTOCOL: u32 = 1;
pub const DEFAULT_PORT: u16 = 9000;
pub const STREAM_VIDEO: u8 = 1;
pub const STREAM_AUDIO: u8 = 2;
pub const STREAM_STATS: u8 = 3;
pub const STREAM_AUDIO_OPUS: u8 = 4;
pub const SERVER: &str = "https://gamestream-rendezvous.gamestream.workers.dev";

const ALPHABET: &[u8] = b"23456789ABCDEFGHJKMNPQRSTUVWXYZ";

/// Aceita "KX7-92B", "kx792b", "gamestream://join/KX792B", "https://.../j/KX7-92B".
pub fn parse_code(input: &str) -> Option<String> {
    let t = input.trim();
    let t = t.split(['?', '#']).next().unwrap_or(t);
    let last = t.trim_end_matches('/').rsplit('/').next().unwrap_or(t);
    let c: String = last.chars().filter(|c| !c.is_whitespace() && *c != '-').map(|c| c.to_ascii_uppercase()).collect();
    (c.len() == 6 && c.bytes().all(|b| ALPHABET.contains(&b))).then_some(c)
}

pub fn display(code: &str) -> String {
    format!("{}-{}", &code[..3], &code[3..])
}

pub fn code_from_fp(fp: &[u8; 32]) -> String {
    let h = Sha256::digest(fp);
    let mut n = u64::from_be_bytes(h[..8].try_into().unwrap());
    let mut code = String::new();
    for _ in 0..6 {
        code.push(ALPHABET[(n % 31) as usize] as char);
        n /= 31;
    }
    code
}

pub struct Invite {
    pub addrs: Vec<SocketAddr>,
    pub fingerprint: Option<[u8; 32]>,
}

pub fn decode_invite(input: &str) -> Result<Invite> {
    let t: String = input.chars().filter(|c| !c.is_whitespace()).collect();
    let b = t.strip_prefix("GS-").context("convite inválido")?;
    let raw = String::from_utf8(B64.decode(b).context("convite inválido")?).context("convite inválido")?;
    let (a, f) = raw.split_once('|').context("convite inválido")?;
    let addrs = a
        .split(',')
        .filter(|s| !s.is_empty())
        .map(|s| s.parse::<SocketAddr>().or_else(|_| s.parse::<IpAddr>().map(|ip| SocketAddr::new(ip, DEFAULT_PORT))))
        .collect::<Result<Vec<_>, _>>()
        .context("convite inválido")?;
    let fp: [u8; 32] = B64.decode(f).ok().and_then(|v| v.try_into().ok()).context("convite inválido")?;
    if addrs.is_empty() {
        bail!("convite sem endereços");
    }
    Ok(Invite { addrs, fingerprint: Some(fp) })
}

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new().timeout(Duration::from_secs(15)).build()
}

/// Entra na sala: avisa o transmissor (que libera o firewall para `reply`) e devolve o convite dele.
pub fn room_join(code: &str, reply: &[SocketAddr]) -> Result<String> {
    let body = serde_json::json!({ "reply": reply.iter().map(|a| a.to_string()).collect::<Vec<_>>() });
    let resp = agent().post(&format!("{SERVER}/v1/room/{code}/join")).set("content-type", "application/json").send_string(&body.to_string());
    let text = match resp {
        Ok(r) => r.into_string()?,
        Err(ureq::Error::Status(404, _)) => bail!("sala não encontrada ou transmissão encerrada"),
        Err(e) => return Err(e).context("sem conexão com o servidor de encontro"),
    };
    let v: serde_json::Value = serde_json::from_str(&text)?;
    Ok(v["invite"].as_str().context("resposta inesperada do servidor")?.to_string())
}

/// Endereços deste celular acessíveis pela internet (para o transmissor liberar o firewall).
pub fn reply_addrs(port: u16) -> Vec<SocketAddr> {
    if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .filter(|i| !i.is_loopback())
        .map(|i| i.ip())
        .filter(|ip| match ip {
            IpAddr::V4(v4) => !v4.is_private() && !v4.is_link_local() && !(v4.octets()[0] == 100 && (v4.octets()[1] & 0xc0) == 64),
            IpAddr::V6(v6) => (v6.segments()[0] & 0xe000) == 0x2000,
        })
        .map(|ip| SocketAddr::new(ip, port))
        .collect()
}

fn transport() -> Arc<TransportConfig> {
    let mut t = TransportConfig::default();
    t.keep_alive_interval(Some(Duration::from_secs(2)));
    t.max_idle_timeout(Some(Duration::from_secs(10).try_into().unwrap()));
    t.stream_receive_window(VarInt::from_u32(8 * 1024 * 1024));
    t.receive_window(VarInt::from_u32(16 * 1024 * 1024));
    t.congestion_controller_factory(Arc::new(quinn::congestion::BbrConfig::default()));
    Arc::new(t)
}

/// Socket UDP dual-stack (IPv6 + IPv4), ou só IPv4.
pub fn client_bind() -> Result<Endpoint> {
    let v6 = || -> std::io::Result<std::net::UdpSocket> {
        let s = Socket::new(Domain::IPV6, Type::DGRAM, Some(Protocol::UDP))?;
        s.set_only_v6(false)?;
        s.bind(&SocketAddr::from((Ipv6Addr::UNSPECIFIED, 0)).into())?;
        Ok(s.into())
    };
    let sock = match v6() {
        Ok(s) => s,
        Err(_) => std::net::UdpSocket::bind(SocketAddr::from(([0, 0, 0, 0], 0)))?,
    };
    sock.set_nonblocking(true)?;
    Ok(Endpoint::new(EndpointConfig::default(), None, sock, Arc::new(quinn::TokioRuntime))?)
}

/// IPv4 em um socket IPv6 dual-stack precisa do formato ::ffff:a.b.c.d.
pub fn for_socket(a: SocketAddr, ep: &Endpoint) -> SocketAddr {
    match (a, ep.local_addr()) {
        (SocketAddr::V4(v4), Ok(SocketAddr::V6(_))) => SocketAddr::new(IpAddr::V6(v4.ip().to_ipv6_mapped()), v4.port()),
        _ => a,
    }
}

pub fn client_config(pin: Option<[u8; 32]>) -> Result<ClientConfig> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let crypto = rustls::ClientConfig::builder_with_provider(provider.clone())
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(PinnedVerifier { provider, pin }))
        .with_no_client_auth();
    let mut cfg = ClientConfig::new(Arc::new(quinn::crypto::rustls::QuicClientConfig::try_from(crypto)?));
    cfg.transport_config(transport());
    Ok(cfg)
}

pub fn security_code(conn: &quinn::Connection) -> Option<String> {
    let certs = conn.peer_identity()?.downcast::<Vec<CertificateDer<'static>>>().ok()?;
    let fp: [u8; 32] = Sha256::digest(certs.first()?.as_ref()).into();
    Some(fp[..6].chunks(2).map(|c| format!("{:02X}{:02X}", c[0], c[1])).collect::<Vec<_>>().join("-"))
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
                return Err(rustls::Error::General("a identidade do transmissor não corresponde ao convite (possível interceptação)".into()));
            }
        }
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

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

/// Lê uma mensagem `[u32 tamanho][dados]`. `false` quando o stream terminou.
pub async fn read_msg(r: &mut quinn::RecvStream, buf: &mut Vec<u8>) -> Result<bool> {
    let mut len = [0u8; 4];
    match r.read_exact(&mut len).await {
        Ok(()) => {}
        Err(quinn::ReadExactError::FinishedEarly(_)) => return Ok(false),
        Err(e) => return Err(e.into()),
    }
    let n = u32::from_le_bytes(len) as usize;
    if n > 32 << 20 {
        bail!("mensagem grande demais");
    }
    buf.resize(n, 0);
    r.read_exact(buf).await?;
    Ok(true)
}

/// Chave deste celular para o transmissor (quem ele já aprovou entra direto da próxima vez).
pub fn viewer_key(secret: &str, host_fp: &[u8; 32]) -> String {
    let mut h = Sha256::new();
    h.update(b"gamestream-viewer-key\0");
    h.update(secret.as_bytes());
    h.update(host_fp);
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// Separa o vídeo (pedaços que cortam quadros em qualquer lugar) em quadros HEVC completos — o
/// decodificador do Android recebe um quadro por vez. Igual ao src/record.rs do PC.
#[derive(Default)]
pub struct AuSplitter {
    cur: Vec<u8>,
    pos: usize,
    seen_vcl: bool,
    key: bool,
}

impl AuSplitter {
    /// Chama `out(quadro, é_chave)` para cada quadro completo.
    pub fn push(&mut self, data: &[u8], mut out: impl FnMut(Vec<u8>, bool)) {
        self.cur.extend_from_slice(data);
        let mut i = self.pos;
        while i + 5 < self.cur.len() {
            let c = &self.cur;
            if c[i] == 0 && c[i + 1] == 0 && c[i + 2] == 1 {
                let t = (c[i + 3] >> 1) & 0x3f;
                let starts = matches!(t, 32..=35 | 39) || (t < 32 && c[i + 5] & 0x80 != 0);
                if starts && self.seen_vcl {
                    let sc = if i > 0 && c[i - 1] == 0 { i - 1 } else { i };
                    let rest = self.cur.split_off(sc);
                    let au = std::mem::replace(&mut self.cur, rest);
                    out(au, self.key);
                    self.seen_vcl = false;
                    self.key = false;
                    i -= sc;
                }
                if t < 32 {
                    self.seen_vcl = true;
                }
                if t == 32 {
                    self.key = true;
                }
                i += 3;
            } else {
                i += 1;
            }
        }
        self.pos = i;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_and_links() {
        assert_eq!(parse_code("kx7-92b").as_deref(), Some("KX792B"));
        assert_eq!(parse_code("gamestream://join/KX792B").as_deref(), Some("KX792B"));
        assert_eq!(parse_code("https://x.workers.dev/j/KX7-92B?a=1").as_deref(), Some("KX792B"));
        assert_eq!(parse_code("GS-abc"), None);
    }

    #[test]
    fn splits_frames() {
        let au1 = [&[0u8, 0, 0, 1, 0x40, 1, 0xaa][..], &[0, 0, 1, 0x42, 1, 0xbb], &[0, 0, 1, 0x26, 1, 0x80, 9]].concat();
        let au2 = vec![0u8, 0, 0, 1, 0x02, 1, 0x80, 7];
        let stream = [au1.clone(), au2.clone(), vec![0, 0, 0, 1, 0x02, 1, 0x80, 1, 1]].concat();
        let mut s = AuSplitter::default();
        let mut got = Vec::new();
        for c in stream.chunks(2) {
            s.push(c, |a, k| got.push((a, k)));
        }
        assert_eq!(got.len(), 2);
        assert_eq!(got[0], (au1, true));
        assert_eq!(got[1], (au2, false));
    }
}
