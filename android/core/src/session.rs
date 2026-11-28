//! Uma sessão assistindo: conecta, recebe vídeo/áudio/chat e reconecta sozinho (até 3 minutos).

use std::{
    sync::{atomic::Ordering, Arc},
    time::{Duration, Instant},
};

use anyhow::{anyhow, Result};
use serde_json::json;
use tokio::io::{AsyncBufReadExt, BufReader};

use crate::{proto, Shared};

pub struct Opts {
    pub target: String,
    pub password: String,
    pub nick: String,
    pub data_dir: String,
}

/// Por que a sessão terminou.
enum End {
    /// Não adianta tentar de novo (senha errada, removido, recusado, versão antiga).
    Fatal(String),
    /// O transmissor encerrou a transmissão.
    HostEnded,
    /// Caiu: tenta reconectar.
    Lost(anyhow::Error),
    /// O usuário saiu.
    Stopped,
}

/// Segredo deste celular (para o transmissor reconhecer quem ele já aprovou).
fn secret(dir: &str) -> String {
    let path = std::path::Path::new(dir).join("viewer-secret.txt");
    if let Ok(s) = std::fs::read_to_string(&path) {
        if s.trim().len() >= 32 {
            return s.trim().to_string();
        }
    }
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(format!("{:?}{:?}{}", std::time::SystemTime::now(), Instant::now(), std::process::id()).as_bytes());
    h.update((&h as *const _ as usize).to_le_bytes());
    let s: String = h.finalize().iter().map(|b| format!("{b:02x}")).collect();
    let _ = std::fs::write(&path, &s);
    s
}

fn close_reason(e: &quinn::ConnectionError) -> Option<End> {
    if let quinn::ConnectionError::ApplicationClosed(c) = e {
        return Some(match u64::from(c.error_code) {
            1 => End::Fatal("senha incorreta".into()),
            2 => End::Fatal("você foi removido pelo transmissor".into()),
            3 => End::Fatal("o transmissor recusou a entrada".into()),
            4 => End::Fatal("versão antiga: atualize o GameStream".into()),
            5 => End::HostEnded,
            _ => return None,
        });
    }
    None
}

struct Joined {
    conn: quinn::Connection,
    first: quinn::RecvStream,
    ctl: Option<(quinn::SendStream, quinn::RecvStream)>,
    _ep: quinn::Endpoint,
}

async fn connect(o: &Opts, s: &Shared, token: Option<&str>, reconnecting: bool) -> Result<Joined, End> {
    let lost = |e: anyhow::Error| End::Lost(e);
    let mut ep = proto::client_bind().map_err(lost)?;
    let port = ep.local_addr().map_err(|e| lost(e.into()))?.port();
    let reply = proto::reply_addrs(port);

    let inv = if let Some(code) = proto::parse_code(&o.target) {
        s.status(format!("Procurando a sala {}...", proto::display(&code)));
        let (c, r) = (code.clone(), reply.clone());
        let text = tokio::task::spawn_blocking(move || proto::room_join(&c, &r)).await.map_err(|e| lost(e.into()))?;
        let text = match text {
            Ok(t) => t,
            Err(e) if e.to_string().contains("não encontrada") && !reconnecting => return Err(End::Fatal(e.to_string())),
            Err(e) => return Err(lost(e)),
        };
        let inv = proto::decode_invite(&text).map_err(|e| End::Fatal(e.to_string()))?;
        if proto::code_from_fp(&inv.fingerprint.unwrap()) != code {
            return Err(End::Fatal("o convite recebido não corresponde ao código da sala".into()));
        }
        inv
    } else {
        let inv = proto::decode_invite(&o.target).map_err(|e| End::Fatal(format!("{e}: digite o código da sala (ex.: KX7-92B)")))?;
        if let Some(fp) = inv.fingerprint {
            let (code, r) = (proto::code_from_fp(&fp), reply.clone());
            let _ = tokio::time::timeout(Duration::from_secs(3), tokio::task::spawn_blocking(move || proto::room_join(&code, &r))).await;
        }
        inv
    };
    s.status("Conectando ao transmissor...");
    ep.set_default_client_config(proto::client_config(inv.fingerprint).map_err(lost)?);

    // Tenta todos os endereços ao mesmo tempo, repetindo a cada 2 s.
    let start = Instant::now();
    let limit = Duration::from_secs(if reconnecting { 15 } else { 25 });
    let mut tries = tokio::task::JoinSet::new();
    let mut next = tokio::time::Instant::now();
    let mut last_err: Option<anyhow::Error> = None;
    let conn = loop {
        if s.stop.load(Ordering::Relaxed) {
            return Err(End::Stopped);
        }
        if tokio::time::Instant::now() >= next {
            for a in inv.addrs.clone() {
                let ep = ep.clone();
                let a = proto::for_socket(a, &ep);
                tries.spawn(async move { anyhow::Ok(ep.connect(a, "gamestream")?.await?) });
            }
            next += Duration::from_secs(2);
        }
        if start.elapsed() > limit {
            let d = last_err.map(|e| format!(" ({e:#})")).unwrap_or_default();
            return Err(lost(anyhow!("tempo de conexão esgotado{d}")));
        }
        tokio::select! {
            r = tries.join_next(), if !tries.is_empty() => match r {
                Some(Ok(Ok(c))) => break c,
                Some(Ok(Err(e))) => last_err = Some(e),
                Some(Err(e)) => last_err = Some(e.into()),
                None => {}
            },
            _ = tokio::time::sleep_until(next) => {}
            _ = s.stop_notify.notified() => return Err(End::Stopped),
        }
    };
    tries.abort_all();

    let mut auth = conn.open_uni().await.map_err(|e| lost(e.into()))?;
    auth.write_all(o.password.as_bytes()).await.map_err(|e| lost(e.into()))?;
    let _ = auth.finish();
    let ctl = match conn.open_bi().await {
        Ok((mut send, recv)) => {
            let vk = inv.fingerprint.map(|fp| proto::viewer_key(&secret(&o.data_dir), &fp));
            let hello = json!({ "t": "hello", "name": o.nick, "token": token, "v": "android-0.1", "p": proto::PROTOCOL, "vk": vk });
            let mut line = hello.to_string();
            line.push('\n');
            match send.write_all(line.as_bytes()).await {
                Ok(()) => Some((send, recv)),
                Err(_) => None,
            }
        }
        Err(_) => None,
    };
    let code = proto::security_code(&conn).unwrap_or_default();
    s.event(json!({ "t": "connected", "code": code }));
    s.status("Aguardando o transmissor liberar a entrada...");
    // O primeiro stream só chega depois da aprovação (sala de espera).
    let first = tokio::select! {
        r = conn.accept_uni() => r.map_err(|e| close_reason(&e).unwrap_or_else(|| lost(e.into())))?,
        _ = s.stop_notify.notified() => return Err(End::Stopped),
    };
    Ok(Joined { conn, first, ctl, _ep: ep })
}

/// Lê um stream de vídeo (pedaços HEVC) e entrega quadro a quadro.
async fn video_stream(mut st: quinn::RecvStream, s: Arc<Shared>) -> Result<()> {
    let mut buf = Vec::new();
    let mut split = proto::AuSplitter::default();
    while proto::read_msg(&mut st, &mut buf).await? {
        split.push(&buf, |au, key| s.push_video(au, key));
    }
    Ok(())
}

/// Áudio: Opus (padrão) ou PCM sem compressão (opção "Áudio sem compressão" do transmissor).
async fn audio_stream(mut st: quinn::RecvStream, s: Arc<Shared>, pcm: bool) -> Result<()> {
    let mut hdr = [0u8; 6];
    st.read_exact(&mut hdr).await?;
    let rate = u32::from_le_bytes(hdr[0..4].try_into().unwrap());
    let ch = u16::from_le_bytes(hdr[4..6].try_into().unwrap());
    s.event(json!({ "t": "audio", "rate": rate, "ch": ch, "pcm": pcm }));
    let mut buf = Vec::new();
    while proto::read_msg(&mut st, &mut buf).await? {
        s.push_audio(buf.clone());
    }
    Ok(())
}

async fn stats_stream(st: quinn::RecvStream, s: Arc<Shared>) -> Result<()> {
    let mut lines = BufReader::new(st).lines();
    while let Some(l) = lines.next_line().await? {
        let mut v = json!({ "t": "stats" });
        for kv in l.split(';') {
            if let Some((k, val)) = kv.split_once('=') {
                if let Ok(n) = val.parse::<f64>() {
                    v[k] = json!(n);
                }
            }
        }
        s.event(v);
    }
    Ok(())
}

/// Canal que o celular ainda não mostra (ex.: câmeras): lê e descarta. Largar o canal faria o
/// transmissor achar que deu erro e derrubar a conexão inteira.
async fn drain_stream(mut st: quinn::RecvStream) -> Result<()> {
    let mut buf = vec![0u8; 64 * 1024];
    while st.read(&mut buf).await?.is_some() {}
    Ok(())
}

/// Recebe e distribui tudo até a conexão terminar.
async fn session(j: Joined, s: Arc<Shared>, token: &mut Option<String>) -> End {
    let Joined { conn, first, ctl, .. } = j;
    s.reset_video();
    s.status("");
    let (out_tx, mut out_rx) = tokio::sync::mpsc::unbounded_channel::<serde_json::Value>();
    s.set_out(Some(out_tx));
    let mut tasks = tokio::task::JoinSet::new();

    // Canal de controle: chat, lista da sala, avisos.
    let (tok_tx, mut tok_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    if let Some((mut send, recv)) = ctl {
        let s2 = s.clone();
        tasks.spawn(async move {
            let mut lines = BufReader::new(recv).lines();
            loop {
                tokio::select! {
                    l = lines.next_line() => {
                        let Some(l) = l? else { break };
                        let Ok(v) = serde_json::from_str::<serde_json::Value>(&l) else { continue };
                        match v["t"].as_str().unwrap_or("") {
                            "welcome" => {
                                if let Some(t) = v["token"].as_str() {
                                    let _ = tok_tx.send(t.chars().take(64).collect());
                                }
                                s2.event(json!({ "t": "welcome", "host": v["host"] }));
                            }
                            "roster" => {
                                let names: Vec<serde_json::Value> = v["list"].as_array().map(|a| a.iter().take(64).map(|m| m["name"].clone()).collect()).unwrap_or_default();
                                s2.event(json!({ "t": "roster", "names": names }));
                            }
                            "chat" | "react" | "privacy" | "notice" | "quality" | "image" | "ping" | "draw" => s2.event(v),
                            _ => {}
                        }
                    }
                    m = out_rx.recv() => {
                        let Some(m) = m else { break };
                        let mut line = m.to_string();
                        line.push('\n');
                        send.write_all(line.as_bytes()).await?;
                    }
                }
            }
            anyhow::Ok(())
        });
    }

    let mut pending = Some(first);
    let mut tok_open = true;
    let end = loop {
        let mut st = match pending.take() {
            Some(st) => st,
            None => tokio::select! {
                r = conn.accept_uni() => match r {
                    Ok(st) => st,
                    Err(e) => break close_reason(&e).unwrap_or_else(|| End::Lost(e.into())),
                },
                Some(r) = tasks.join_next() => {
                    if let Ok(Err(e)) = r {
                        crate::alog(&format!("stream terminou com erro: {e:#}"));
                        if conn.close_reason().is_none() {
                            // Um stream falhou (ex.: dados inválidos): segue com os outros.
                            s.status(format!("aviso: {e:#}"));
                        }
                    }
                    continue;
                }
                t = tok_rx.recv(), if tok_open => {
                    match t {
                        Some(t) => *token = Some(t),
                        None => tok_open = false,
                    }
                    continue;
                }
                _ = s.stop_notify.notified() => break End::Stopped,
            },
        };
        let mut kind = [0u8; 1];
        if st.read_exact(&mut kind).await.is_err() {
            continue;
        }
        let s2 = s.clone();
        match kind[0] {
            proto::STREAM_VIDEO => {
                tasks.spawn(video_stream(st, s2));
            }
            proto::STREAM_AUDIO_OPUS => {
                tasks.spawn(audio_stream(st, s2, false));
            }
            proto::STREAM_AUDIO => {
                tasks.spawn(audio_stream(st, s2, true));
            }
            proto::STREAM_STATS => {
                tasks.spawn(stats_stream(st, s2));
            }
            _ => {
                // Câmeras e outros: o app do celular ainda não mostra, mas mantém o canal aberto.
                tasks.spawn(drain_stream(st));
            }
        }
        if s.stop.load(Ordering::Relaxed) {
            break End::Stopped;
        }
    };
    s.set_out(None);
    tasks.abort_all();
    if matches!(end, End::Stopped) {
        conn.close(0u32.into(), b"saiu");
    }
    end
}

pub async fn run(o: Opts, s: Arc<Shared>) {
    let mut token: Option<String> = None;
    let mut lost_at: Option<Instant> = None;
    // O transmissor encerrou: espera até 1 minuto ele voltar na mesma sala.
    let mut ended_at: Option<Instant> = None;
    let mut attempt = 0u32;
    loop {
        let reconnecting = lost_at.is_some();
        let end = match connect(&o, &s, token.as_deref(), reconnecting).await {
            Ok(j) => {
                lost_at = None;
                ended_at = None;
                attempt = 0;
                session(j, s.clone(), &mut token).await
            }
            Err(e) => e,
        };
        match end {
            End::Stopped => return,
            End::Fatal(msg) => {
                s.event(json!({ "t": "ended", "text": msg, "fatal": true }));
                return;
            }
            End::HostEnded if ended_at.is_none() => {
                ended_at = Some(Instant::now());
                lost_at = Some(Instant::now());
                token = None;
                s.status("O transmissor encerrou a transmissão. Esperando voltar (até 1 minuto)...");
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_secs(3)) => {}
                    _ = s.stop_notify.notified() => return,
                }
            }
            End::HostEnded => {
                s.event(json!({ "t": "ended", "text": "O transmissor encerrou a transmissão.", "fatal": false }));
                return;
            }
            End::Lost(_) if ended_at.is_some_and(|t| t.elapsed() > Duration::from_secs(60)) => {
                s.event(json!({ "t": "ended", "text": "O transmissor encerrou a transmissão.", "fatal": false }));
                return;
            }
            End::Lost(_) if ended_at.is_some() => {
                s.status("O transmissor encerrou a transmissão. Esperando voltar (até 1 minuto)...");
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_secs(3)) => {}
                    _ = s.stop_notify.notified() => return,
                }
            }
            End::Lost(e) => {
                let since = *lost_at.get_or_insert_with(Instant::now);
                if since.elapsed() > Duration::from_secs(180) || (!reconnecting && token.is_none() && attempt == 0 && e.to_string().contains("esgotado")) {
                    s.event(json!({ "t": "ended", "text": format!("Não foi possível conectar: {e:#}"), "fatal": true }));
                    return;
                }
                attempt += 1;
                crate::alog(&format!("conexão perdida: {e:#}"));
                s.status(format!("Conexão perdida ({e:#}). Reconectando (tentativa {attempt})..."));
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_millis(1500)) => {}
                    _ = s.stop_notify.notified() => return,
                }
                if s.stop.load(Ordering::Relaxed) {
                    return;
                }
            }
        }
    }
}
