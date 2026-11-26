//! Viewer: conecta no host via QUIC (tenta todos os endereços do convite ao mesmo tempo),
//! decodifica o HEVC na GPU (mpv, modo baixa latência) e toca o áudio com buffer curto.
//! O painel de controles (volume, zoom no scroll, tela cheia) roda dentro da janela do mpv
//! (assets/overlay.lua); volume/mudo são sincronizados com o app via IPC (named pipe).

use std::{
    future::Future,
    path::PathBuf,
    process::Stdio,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

use anyhow::{anyhow, bail, Context, Result};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt},
    net::windows::named_pipe::ClientOptions,
    process::Command,
    task::JoinSet,
};

use crate::{
    audio, invite, net,
    state::{self, log, view_phase, ViewCtl, ViewPhase},
};

pub struct ViewOpts {
    pub invite: String,
    pub password: Option<String>,
    pub mpv: Option<String>,
    /// Saída de áudio (None = padrão do Windows).
    pub audio_device: Option<String>,
}

/// Procura o mpv.exe ao lado do gamestream.exe; senão, usa o do PATH.
pub fn find_mpv(explicit: Option<String>) -> PathBuf {
    if let Some(p) = explicit {
        return p.into();
    }
    if let Ok(exe) = std::env::current_exe() {
        let local = exe.with_file_name("mpv.exe");
        if local.exists() {
            return local;
        }
    }
    "mpv".into()
}

const OVERLAY_LUA: &str = include_str!("../assets/overlay.lua");

/// Grava o script do painel (embutido no .exe) numa pasta temporária e devolve o caminho.
fn overlay_script() -> Option<PathBuf> {
    let p = std::env::temp_dir().join(format!("gamestream-overlay-{}.lua", env!("CARGO_PKG_VERSION")));
    if std::fs::read_to_string(&p).ok().as_deref() != Some(OVERLAY_LUA) {
        std::fs::write(&p, OVERLAY_LUA).ok()?;
    }
    Some(p)
}

/// Argumentos do mpv em modo baixa latência lendo HEVC do stdin.
/// `mode` = "viewer" (com controles de som) ou "preview" (prévia do host, sem som).
pub fn mpv_args(mode: &str, title: &str, ipc: Option<&str>) -> Vec<String> {
    let mut a: Vec<String> = [
        "--profile=low-latency",
        "--untimed",
        "--cache=no",
        "--hwdec=auto",
        "--vd-lavc-threads=1",
        "--video-latency-hacks=yes",
        "--demuxer-lavf-format=hevc",
        "--demuxer-lavf-probesize=32",
        "--demuxer-lavf-analyzeduration=0",
        "--audio=no",
        "--osc=no",
        "--keep-open=no",
        "--force-window=immediate",
        "--really-quiet",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    a.push(format!("--title={title}"));
    if mode == "preview" {
        a.push("--autofit=40%".into());
    }
    if let Some(p) = overlay_script() {
        a.push(format!("--script={}", p.display()));
        a.push(format!("--script-opts=gs-mode={mode}"));
    }
    if let Some(pipe) = ipc {
        a.push(format!("--input-ipc-server={pipe}"));
    }
    a.push("-".into());
    a
}

pub async fn run(o: ViewOpts, stop: impl Future<Output = ()>) -> Result<()> {
    state::view(|v| *v = state::ViewStatus::default());
    let r = tokio::select! {
        r = run_inner(o) => r,
        _ = stop => Ok(()),
    };
    match &r {
        Ok(()) => view_phase(ViewPhase::Idle, "Desconectado."),
        Err(e) => {
            log(format!("Erro: {e:#}"));
            view_phase(ViewPhase::Error, format!("{e:#}"));
        }
    }
    r
}

async fn run_inner(o: ViewOpts) -> Result<()> {
    let inv = invite::decode(&o.invite, net::DEFAULT_PORT).await?;
    if inv.fingerprint.is_none() {
        log("Aviso: conexão por IP direto, sem verificação da identidade do transmissor. Recomenda-se usar o convite GS-...");
    }
    view_phase(ViewPhase::Connecting, "Conectando...");
    log(format!("Conectando ({} endereço(s))...", inv.addrs.len()));
    let ep = net::client_endpoint(inv.fingerprint)?;

    // Tenta todos os endereços em paralelo; o primeiro que responder ganha.
    let mut tries = JoinSet::new();
    for a in inv.addrs.clone() {
        let ep = ep.clone();
        tries.spawn(async move {
            let c = ep.connect(a, "gamestream")?.await?;
            anyhow::Ok(c)
        });
    }
    let conn = tokio::time::timeout(Duration::from_secs(8), async {
        let mut last = None;
        while let Some(r) = tries.join_next().await {
            match r {
                Ok(Ok(c)) => return Ok(c),
                Ok(Err(e)) => last = Some(e),
                Err(e) => last = Some(e.into()),
            }
        }
        Err(last.unwrap_or_else(|| anyhow!("nenhum endereço")))
    })
    .await
    .map_err(|_| anyhow!("tempo de conexão esgotado. Verifique se a transmissão está ativa e se há um caminho de rede disponível (mesma VPN, IPv6 ou porta aberta)."))??;
    tries.abort_all();

    // Sem convite (IP direto) não há como saber se é mesmo o host: não manda a senha às cegas.
    if inv.fingerprint.is_none() && o.password.is_some() {
        conn.close(0u32.into(), b"");
        bail!("por segurança, a senha só é enviada em conexões pelo convite GS-..., que verifica a identidade do transmissor");
    }
    let mut auth = conn.open_uni().await?;
    auth.write_all(o.password.unwrap_or_default().as_bytes()).await?;
    auth.finish()?;
    let via = conn.remote_address().ip().to_canonical().to_string();
    let code = net::peer_security_code(&conn).unwrap_or_else(|| "?".into());
    let verified = inv.fingerprint.is_some();
    log(format!(
        "Conectado a {via} (criptografado, {}). Código de segurança do transmissor: {code}",
        if verified { "identidade verificada pelo convite" } else { "identidade não verificada" }
    ));
    state::view(|v| {
        v.phase = ViewPhase::Connected;
        v.text = "Conectado".into();
        v.peer = via.clone();
        v.code = code.clone();
        v.verified = verified;
    });

    let pipe = format!(r"\\.\pipe\gamestream-mpv-{}", std::process::id());
    let mpv = find_mpv(o.mpv);
    let mut child = Command::new(&mpv)
        .args(mpv_args("viewer", &format!("GameStream - {via}"), Some(&pipe)))
        .stdin(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .with_context(|| format!("player não encontrado ({}). O mpv.exe deve estar na mesma pasta do gamestream.exe", mpv.display()))?;
    let mut stdin = Some(child.stdin.take().unwrap());

    let mut tasks = JoinSet::new();
    let audio_device = o.audio_device;
    let control = mpv_control(&pipe);
    tokio::pin!(control);
    let frames = Arc::new(AtomicU64::new(0));
    let bytes = Arc::new(AtomicU64::new(0));
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let (mut last_frames, mut last_bytes, mut last_t) = (0u64, 0u64, std::time::Instant::now());
    let result = loop {
        tokio::select! {
            st = conn.accept_uni() => {
                let mut st = match st {
                    Ok(s) => s,
                    Err(quinn::ConnectionError::ApplicationClosed(c)) => {
                        break Err(anyhow!("o transmissor encerrou a conexão: {}", String::from_utf8_lossy(&c.reason)));
                    }
                    Err(e) => break Err(anyhow!("conexão com o transmissor perdida: {e}")),
                };
                let mut kind = [0u8; 1];
                st.read_exact(&mut kind).await?;
                match kind[0] {
                    net::STREAM_VIDEO => {
                        let Some(mut pipe_in) = stdin.take() else { bail!("o transmissor enviou dois fluxos de vídeo") };
                        let (frames, bytes) = (frames.clone(), bytes.clone());
                        tasks.spawn(async move {
                            let mut buf = Vec::new();
                            while net::read_msg(&mut st, &mut buf).await? {
                                frames.fetch_add(count_frames(&buf), Ordering::Relaxed);
                                bytes.fetch_add(buf.len() as u64, Ordering::Relaxed);
                                pipe_in.write_all(&buf).await?;
                            }
                            anyhow::Ok(())
                        });
                    }
                    net::STREAM_STATS => {
                        tasks.spawn(async move {
                            let mut lines = tokio::io::BufReader::new(st).lines();
                            while let Some(l) = lines.next_line().await? {
                                let mut loss = None;
                                let mut target = None;
                                for kv in l.split(';') {
                                    match kv.split_once('=') {
                                        Some(("loss", v)) => loss = v.parse::<f64>().ok(),
                                        Some(("target", v)) => target = v.parse::<u32>().ok(),
                                        _ => {}
                                    }
                                }
                                state::view(|v| {
                                    if let Some(x) = loss { v.stats.loss = x; }
                                    if let Some(x) = target { v.stats.target = x; }
                                });
                            }
                            anyhow::Ok(())
                        });
                    }
                    net::STREAM_AUDIO => {
                        let mut hdr = [0u8; 6];
                        st.read_exact(&mut hdr).await?;
                        let rate = u32::from_le_bytes(hdr[0..4].try_into().unwrap());
                        let ch = u16::from_le_bytes(hdr[4..6].try_into().unwrap());
                        match audio::start_player(rate, ch, audio_device.clone()) {
                            Ok(mut player) => {
                                tasks.spawn(async move {
                                    let mut buf = Vec::new();
                                    while net::read_msg(&mut st, &mut buf).await? {
                                        player.push(&buf);
                                    }
                                    anyhow::Ok(())
                                });
                            }
                            Err(e) => log(format!("Áudio indisponível: {e:#}")),
                        }
                    }
                    k => log(format!("fluxo desconhecido: {k}")),
                }
            }
            Some(r) = tasks.join_next() => {
                break match r? {
                    Ok(()) => { log("O transmissor encerrou a transmissão."); Ok(()) }
                    Err(e) => Err(anyhow!("transmissão interrompida: {e:#}")),
                };
            }
            _ = child.wait() => { log("Janela de vídeo fechada."); break Ok(()); }
            _ = &mut control => {}
            _ = tick.tick() => {
                let (f, b) = (frames.load(Ordering::Relaxed), bytes.load(Ordering::Relaxed));
                let dt = last_t.elapsed().as_secs_f64().max(0.2);
                let ping = conn.stats().path.rtt.as_millis() as u32;
                state::view(|v| {
                    v.stats.fps = ((f - last_frames) as f64 / dt).round() as u32;
                    v.stats.mbps = (b - last_bytes) as f64 * 8.0 / 1e6 / dt;
                    v.stats.ping_ms = ping;
                });
                (last_frames, last_bytes, last_t) = (f, b, std::time::Instant::now());
            }
        }
    };
    conn.close(0u32.into(), b"desconectado");
    let _ = tokio::time::timeout(Duration::from_secs(1), ep.wait_idle()).await;
    result
}

/// Conta frames num pedaço de HEVC: cada NAL de vídeo com first_slice_segment_in_pic_flag começa um frame.
fn count_frames(b: &[u8]) -> u64 {
    let mut n = 0;
    let mut i = 0;
    while i + 5 < b.len() {
        if b[i] == 0 && b[i + 1] == 0 && b[i + 2] == 1 {
            let t = (b[i + 3] >> 1) & 0x3f;
            if t < 32 && b[i + 5] & 0x80 != 0 {
                n += 1;
            }
            i += 3;
        } else {
            i += 1;
        }
    }
    n
}

/// Sincroniza volume/mudo/janela entre o app e o painel dentro do mpv (IPC, JSON por linha).
/// Nunca termina enquanto funcionar; se o pipe falhar, fica parado sem derrubar a conexão.
async fn mpv_control(pipe: &str) {
    let client = {
        let mut tries = 0;
        loop {
            match ClientOptions::new().open(pipe) {
                Ok(c) => break c,
                Err(_) if tries < 50 => {
                    tries += 1;
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
                Err(e) => {
                    log(format!("[player] controles indisponíveis: {e}"));
                    return std::future::pending().await;
                }
            }
        }
    };
    let (rd, mut wr) = tokio::io::split(client);

    // O que o mpv já sabe. Compartilhado com o leitor para não devolver ao mpv o que veio dele.
    let synced = Arc::new(Mutex::new(None::<ViewCtl>));

    // Leitor: mudanças feitas no painel do vídeo → app (áudio + interface).
    {
        let synced = synced.clone();
        tokio::spawn(async move {
            let mut lines = tokio::io::BufReader::new(rd).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else { continue };
                if v["event"] != "property-change" {
                    continue;
                }
                let mut ctl = state::VIEW_CTL.lock().unwrap();
                match (v["name"].as_str(), &v["data"]) {
                    (Some("user-data/gs/volume"), d) if d.is_number() => {
                        ctl.volume = d.as_f64().unwrap_or(1.0) as f32;
                        audio::set_volume(ctl.volume);
                    }
                    (Some("user-data/gs/muted"), d) if d.is_boolean() => {
                        ctl.muted = d.as_bool().unwrap_or(false);
                        audio::set_muted(ctl.muted);
                    }
                    _ => continue,
                }
                let mut s = synced.lock().unwrap();
                if let Some(s) = s.as_mut() {
                    s.volume = ctl.volume;
                    s.muted = ctl.muted;
                }
            }
        });
    }

    let observe = [
        r#"{"command":["observe_property",1,"user-data/gs/volume"]}"#,
        r#"{"command":["observe_property",2,"user-data/gs/muted"]}"#,
    ];
    for cmd in observe {
        if wr.write_all(format!("{cmd}\n").as_bytes()).await.is_err() {
            return std::future::pending().await;
        }
    }

    // Escritor: mudanças feitas no app → mpv.
    let mut last_stats = String::new();
    loop {
        let line = state::VIEW.lock().unwrap().stats.line();
        if line != last_stats {
            let cmd = format!(
                r#"{{"command":["set_property","user-data/gs/stats",{}]}}"#,
                serde_json::to_string(&line).unwrap_or_default()
            );
            if wr.write_all(format!("{cmd}
").as_bytes()).await.is_err() {
                return std::future::pending().await;
            }
            last_stats = line;
        }
        let c = state::VIEW_CTL.lock().unwrap().clone();
        let prev = synced.lock().unwrap().clone();
        if prev.as_ref() != Some(&c) {
            let mut cmds = Vec::new();
            if prev.as_ref().map(|p| p.volume) != Some(c.volume) {
                cmds.push(format!(r#"{{"command":["set_property","user-data/gs/volume",{}]}}"#, c.volume));
            }
            if prev.as_ref().map(|p| p.muted) != Some(c.muted) {
                cmds.push(format!(r#"{{"command":["set_property","user-data/gs/muted",{}]}}"#, c.muted));
            }
            if prev.as_ref().map(|p| p.on_top) != Some(c.on_top) {
                cmds.push(format!(r#"{{"command":["set_property","ontop",{}]}}"#, c.on_top));
            }
            if prev.as_ref().is_some_and(|p| p.fullscreen_toggles != c.fullscreen_toggles) {
                cmds.push(r#"{"command":["cycle","fullscreen"]}"#.into());
            }
            for cmd in cmds {
                if wr.write_all(format!("{cmd}\n").as_bytes()).await.is_err() {
                    return std::future::pending().await;
                }
            }
            *synced.lock().unwrap() = Some(c);
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
