//! Viewer: conecta no host via QUIC (tenta todos os endereços do convite ao mesmo tempo),
//! decodifica o HEVC na GPU (mpv, modo baixa latência) e toca o áudio com buffer curto.
//! O painel de controles (volume, zoom no scroll, tela cheia) roda dentro da janela do mpv
//! (assets/overlay.lua); volume/mudo são sincronizados com o app via IPC (named pipe).

use std::{
    future::Future,
    path::PathBuf,
    process::Stdio,
    sync::{Arc, Mutex},
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
        log("Aviso: conectando por IP direto, sem verificar a identidade do host. Prefira o convite GS-...");
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
    .map_err(|_| anyhow!("tempo esgotado: o host está transmitindo? Vocês estão na mesma VPN ou os dois têm IPv6?"))??;
    tries.abort_all();

    // Sem convite (IP direto) não há como saber se é mesmo o host: não manda a senha às cegas.
    if inv.fingerprint.is_none() && o.password.is_some() {
        conn.close(0u32.into(), b"");
        bail!("por segurança, a senha só é enviada quando você conecta pelo convite GS-... (que verifica a identidade do host)");
    }
    let mut auth = conn.open_uni().await?;
    auth.write_all(o.password.unwrap_or_default().as_bytes()).await?;
    auth.finish()?;
    let via = conn.remote_address().ip().to_canonical().to_string();
    let code = net::peer_security_code(&conn).unwrap_or_else(|| "?".into());
    let verified = inv.fingerprint.is_some();
    log(format!(
        "Conectado em {via} (criptografado, {}). Código de segurança do host: {code}",
        if verified { "identidade verificada pelo convite" } else { "identidade NÃO verificada" }
    ));
    state::view(|v| {
        v.phase = ViewPhase::Connected;
        v.text = "Assistindo".into();
        v.peer = via.clone();
        v.code = code.clone();
        v.verified = verified;
    });

    let pipe = format!(r"\\.\pipe\gamestream-mpv-{}", std::process::id());
    let mpv = find_mpv(o.mpv);
    let mut child = Command::new(&mpv)
        .args(mpv_args("viewer", &format!("GameStream - assistindo {via}"), Some(&pipe)))
        .stdin(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .with_context(|| format!("não achei o mpv ({}). Coloque o mpv.exe na mesma pasta do gamestream.exe", mpv.display()))?;
    let mut stdin = Some(child.stdin.take().unwrap());

    let mut tasks = JoinSet::new();
    let audio_device = o.audio_device;
    let control = mpv_control(&pipe);
    tokio::pin!(control);
    let result = loop {
        tokio::select! {
            st = conn.accept_uni() => {
                let mut st = match st {
                    Ok(s) => s,
                    Err(quinn::ConnectionError::ApplicationClosed(c)) => {
                        break Err(anyhow!("o host fechou a conexão: {}", String::from_utf8_lossy(&c.reason)));
                    }
                    Err(e) => break Err(anyhow!("conexão com o host caiu: {e}")),
                };
                let mut kind = [0u8; 1];
                st.read_exact(&mut kind).await?;
                match kind[0] {
                    net::STREAM_VIDEO => {
                        let Some(mut pipe_in) = stdin.take() else { bail!("host mandou dois streams de vídeo") };
                        tasks.spawn(async move {
                            let mut buf = Vec::new();
                            while net::read_msg(&mut st, &mut buf).await? {
                                pipe_in.write_all(&buf).await?;
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
                            Err(e) => log(format!("Sem áudio: {e:#}")),
                        }
                    }
                    k => log(format!("stream desconhecido: {k}")),
                }
            }
            Some(r) = tasks.join_next() => {
                break match r? {
                    Ok(()) => { log("O host encerrou a transmissão."); Ok(()) }
                    Err(e) => Err(anyhow!("transmissão interrompida: {e:#}")),
                };
            }
            _ = child.wait() => { log("Player fechado."); break Ok(()); }
            _ = &mut control => {}
        }
    };
    conn.close(0u32.into(), b"tchau");
    let _ = tokio::time::timeout(Duration::from_secs(1), ep.wait_idle()).await;
    result
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
    loop {
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
