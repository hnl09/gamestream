//! Sala no servidor de encontro (worker/): conexão automática sem código de resposta, código curto
//! (ex.: KX7-92B) e teste de velocidade. O servidor só troca endereços; o vídeo nunca passa por ele.
//! Sem servidor configurado ou fora do ar, o app continua funcionando com convite + código de resposta.

use std::{
    collections::hash_map::RandomState,
    hash::{BuildHasher, Hasher},
    net::SocketAddr,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};
use tokio::sync::mpsc::UnboundedSender;

use crate::state::{self, log};

/// Endereço do servidor de encontro publicado (worker/). Pode ser trocado por GAMESTREAM_SERVER.
pub const DEFAULT_SERVER: &str = "https://gamestream-rendezvous.gamestream.workers.dev";

const ALPHABET: &[u8] = b"23456789ABCDEFGHJKMNPQRSTUVWXYZ";

pub fn server() -> Option<String> {
    let s = std::env::var("GAMESTREAM_SERVER").unwrap_or_else(|_| DEFAULT_SERVER.to_string());
    let s = s.trim().trim_end_matches('/').to_string();
    (!s.is_empty()).then_some(s)
}

/// Código da sala: 6 caracteres derivados do SHA-256 da impressão digital do transmissor
/// (mesmo algoritmo do worker). Como o espectador confere o código contra a identidade recebida,
/// nem o servidor consegue redirecionar para outro transmissor.
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

/// "KX792B" -> "KX7-92B"
pub fn display(code: &str) -> String {
    format!("{}-{}", &code[..3], &code[3..])
}

/// Aceita "KX7-92B", "kx792b", " KX7 92B " etc.
pub fn parse_code(input: &str) -> Option<String> {
    let c: String = input
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '-')
        .map(|c| c.to_ascii_uppercase())
        .collect();
    (c.len() == 6 && c.bytes().all(|b| ALPHABET.contains(&b))).then_some(c)
}

/// Segredo que vincula a sala a este computador (o primeiro a usar o código vira o dono).
pub fn load_secret() -> String {
    let p = std::path::PathBuf::from(std::env::var("APPDATA").unwrap_or_else(|_| ".".into()))
        .join("GameStream")
        .join("room-secret");
    if let Ok(s) = std::fs::read_to_string(&p) {
        if s.trim().len() >= 32 {
            return s.trim().to_string();
        }
    }
    let mut seed = Vec::new();
    for _ in 0..8 {
        let mut h = RandomState::new().build_hasher();
        h.write_u128(Instant::now().elapsed().as_nanos() ^ std::process::id() as u128);
        seed.extend_from_slice(&h.finish().to_le_bytes());
    }
    let secret: String = Sha256::digest(&seed).iter().map(|b| format!("{b:02x}")).collect();
    if let Some(d) = p.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let _ = std::fs::write(&p, &secret);
    secret
}

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(6))
        .timeout_read(Duration::from_secs(10))
        .user_agent(&format!("GameStream/{}", env!("CARGO_PKG_VERSION")))
        .build()
}

// ---------- transmissor ----------

/// Mantém a sala aberta enquanto existir; ao ser descartada, fecha a conexão.
pub struct RoomHost {
    stop: Arc<AtomicBool>,
}

impl Drop for RoomHost {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// Abre a sala `code` no servidor e repassa os endereços dos espectadores que entrarem para `punch`
/// (o transmissor libera o firewall para eles automaticamente). Reconecta sozinho se cair.
pub fn start_host(server: String, code: String, invite: String, secret: String, punch: UnboundedSender<Vec<SocketAddr>>) -> RoomHost {
    let stop = Arc::new(AtomicBool::new(false));
    let stop2 = stop.clone();
    std::thread::spawn(move || {
        let url = format!("{}/v1/room/{code}/host", server.replacen("http", "ws", 1));
        let mut backoff = 2u64;
        let mut first = true;
        while !stop2.load(Ordering::Relaxed) {
            match run_host(&url, &invite, &secret, &punch, &stop2) {
                Ok(()) => break,
                Err(e) => {
                    state::host(|h| h.room_online = false);
                    if first || backoff >= 32 {
                        log(crate::tr!("[sala] indisponível: {e:#}", "[room] unavailable: {e:#}"));
                    }
                    first = false;
                }
            }
            for _ in 0..backoff * 10 {
                if stop2.load(Ordering::Relaxed) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            backoff = (backoff * 2).min(60);
        }
        state::host(|h| h.room_online = false);
    });
    RoomHost { stop }
}

fn run_host(url: &str, invite: &str, secret: &str, punch: &UnboundedSender<Vec<SocketAddr>>, stop: &AtomicBool) -> Result<()> {
    use tungstenite::{stream::MaybeTlsStream, Message};
    let (mut ws, _) = tungstenite::connect(url).context(crate::i18n::t("sem conexão com o servidor de encontro"))?;
    let timeout = Some(Duration::from_millis(500));
    match ws.get_ref() {
        MaybeTlsStream::Plain(s) => s.set_read_timeout(timeout)?,
        MaybeTlsStream::Rustls(s) => s.get_ref().set_read_timeout(timeout)?,
        _ => {}
    }
    let hello = serde_json::json!({ "type": "hello", "invite": invite, "secret": secret });
    ws.send(Message::text(hello.to_string()))?;
    let mut last_ping = Instant::now();
    loop {
        if stop.load(Ordering::Relaxed) {
            let _ = ws.close(None);
            return Ok(());
        }
        if last_ping.elapsed() > Duration::from_secs(25) {
            // Respondido automaticamente pelo servidor, sem consumir cota.
            ws.send(Message::text("ping"))?;
            last_ping = Instant::now();
        }
        let msg = match ws.read() {
            Ok(m) => m,
            Err(tungstenite::Error::Io(e))
                if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) =>
            {
                continue
            }
            Err(e) => return Err(e.into()),
        };
        let text = match msg {
            Message::Text(t) => t.to_string(),
            Message::Close(f) => bail!("{}", f.map(|f| f.reason.to_string()).unwrap_or_default()),
            _ => continue,
        };
        if text == "pong" {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else { continue };
        match v["type"].as_str() {
            Some("ok") => {
                state::host(|h| h.room_online = true);
                log(crate::i18n::t("[sala] conectada ao servidor de encontro: espectadores conectam automaticamente."));
            }
            Some("punch") => {
                let addrs: Vec<SocketAddr> = v["addrs"]
                    .as_array()
                    .map(|a| a.iter().filter_map(|x| x.as_str()?.parse().ok()).collect())
                    .unwrap_or_default();
                if !addrs.is_empty() {
                    let _ = punch.send(addrs);
                }
            }
            Some("error") => bail!("{}", v["error"].as_str().unwrap_or("erro")),
            _ => {}
        }
    }
}

// ---------- espectador ----------

/// Entra na sala: avisa o transmissor (que libera o firewall para `reply`) e devolve o convite dele.
pub fn join(server: &str, code: &str, reply: &[SocketAddr]) -> Result<String> {
    let body = serde_json::json!({ "reply": reply.iter().map(|a| a.to_string()).collect::<Vec<_>>() });
    let resp = agent()
        .post(&format!("{server}/v1/room/{code}/join"))
        .set("content-type", "application/json")
        .send_string(&body.to_string());
    let text = match resp {
        Ok(r) => r.into_string()?,
        Err(ureq::Error::Status(404, _)) => {
            bail!("{}", crate::i18n::t("sala não encontrada ou transmissão encerrada"))
        }
        Err(e) => return Err(e).context(crate::i18n::t("sem conexão com o servidor de encontro")),
    };
    let v: serde_json::Value = serde_json::from_str(&text)?;
    Ok(v["invite"].as_str().context(crate::i18n::t("resposta inesperada do servidor"))?.to_string())
}

/// Quais salas estão ao vivo agora (para a lista de salas recentes). Códigos sem hífen.
pub fn status(server: &str, codes: &[String]) -> Result<std::collections::HashMap<String, bool>> {
    let body = serde_json::json!({ "codes": codes });
    let text = agent()
        .post(&format!("{server}/v1/status"))
        .set("content-type", "application/json")
        .send_string(&body.to_string())?
        .into_string()?;
    let v: serde_json::Value = serde_json::from_str(&text)?;
    Ok(v["live"]
        .as_object()
        .map(|m| m.iter().map(|(k, v)| (k.clone(), v.as_bool() == Some(true))).collect())
        .unwrap_or_default())
}

// ---------- teste de velocidade ----------

/// Corpo do teste de velocidade: repete os dados até o prazo (ou até `max` bytes).
struct DeadlineReader {
    data: Arc<Vec<u8>>,
    pos: usize,
    sent: usize,
    max: usize,
    deadline: Instant,
}

impl std::io::Read for DeadlineReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.sent >= self.max || Instant::now() >= self.deadline {
            return Ok(0);
        }
        let n = buf.len().min(self.data.len() - self.pos).min(self.max - self.sent).min(256 << 10);
        buf[..n].copy_from_slice(&self.data[self.pos..self.pos + n]);
        self.pos = (self.pos + n) % self.data.len();
        self.sent += n;
        Ok(n)
    }
}

/// Mede o upload até o servidor (Cloudflare mais próximo) por ~`secs` segundos, com 4 conexões.
pub fn speedtest(server: &str, secs: f64) -> Result<f64> {
    const CHUNK: usize = 8 << 20;
    let mut data = vec![0u8; CHUNK];
    // Dados pseudoaleatórios (não compressíveis).
    let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
    for b in data.iter_mut() {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        *b = x as u8;
    }
    let data = Arc::new(data);
    let start = Instant::now();
    let deadline = start + Duration::from_secs_f64(secs);
    let total = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let mut handles = Vec::new();
    for _ in 0..4 {
        let (data, total, url) = (data.clone(), total.clone(), format!("{server}/v1/speedtest"));
        handles.push(std::thread::spawn(move || -> Result<()> {
            let a = ureq::AgentBuilder::new().timeout(Duration::from_secs(30)).build();
            // Um envio contínuo por conexão (poucos pedidos ao servidor, que limita pedidos por IP).
            // Cada envio vai até o prazo ou até 90 MB (limite de corpo da Cloudflare no plano gratuito).
            while Instant::now() < deadline {
                let body = DeadlineReader { data: data.clone(), pos: 0, sent: 0, max: 90 << 20, deadline };
                let r = a.post(&url).set("content-type", "application/octet-stream").send(body)?;
                let v: serde_json::Value = serde_json::from_str(&r.into_string()?)?;
                total.fetch_add(v["bytes"].as_u64().unwrap_or(0), Ordering::Relaxed);
            }
            Ok(())
        }));
    }
    let mut last_err = None;
    for h in handles {
        if let Ok(Err(e)) = h.join() {
            last_err = Some(e);
        }
    }
    let bytes = total.load(Ordering::Relaxed);
    if bytes == 0 {
        return Err(last_err.unwrap_or_else(|| anyhow::anyhow!("nenhum dado enviado")));
    }
    Ok(bytes as f64 * 8.0 / start.elapsed().as_secs_f64() / 1e6)
}
