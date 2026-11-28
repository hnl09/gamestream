//! Viewer: conecta no host via QUIC (tenta todos os endereços do convite ao mesmo tempo),
//! decodifica o HEVC na GPU (mpv, modo baixa latência) e toca o áudio com buffer curto.
//! O painel de controles (volume, zoom no scroll, tela cheia) roda dentro da janela do mpv
//! (assets/overlay.lua); volume/mudo são sincronizados com o app via IPC (named pipe).

use std::{
    future::Future,
    net::SocketAddr,
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
        let lang = if crate::settings::is_en() { "en" } else { "pt" };
        a.push(format!("--script-opts=gs-mode={mode},gs-lang={lang}"));
    }
    if let Some(pipe) = ipc {
        a.push(format!("--input-ipc-server={pipe}"));
    }
    // Opções extras do mpv (para testes automatizados, ex.: "--vo=null" para rodar sem janela).
    if let Ok(extra) = std::env::var("GAMESTREAM_MPV_ARGS") {
        a.extend(extra.split_whitespace().map(String::from));
    }
    a.push("-".into());
    a
}

/// Ações do usuário na janela do vídeo (chat e reações digitados no próprio vídeo).
#[derive(Debug, Clone)]
pub enum OverlayEv {
    Chat(String),
    React(String),
    /// Apontar na tela (posição no vídeo, 0..1).
    Ping(f32, f32),
}

/// O que a interface (ou o terminal) manda para o transmissor.
#[derive(Debug, Clone)]
enum Outgoing {
    Chat(String),
    React(String),
    ControlReq { pad: bool, kbm: bool },
    Camera(Option<String>),
}

static OUT: std::sync::LazyLock<Mutex<Option<tokio::sync::mpsc::UnboundedSender<Outgoing>>>> =
    std::sync::LazyLock::new(|| Mutex::new(None));

fn out(m: Outgoing) {
    if let Some(tx) = OUT.lock().unwrap().as_ref() {
        let _ = tx.send(m);
    }
}

/// Câmera que este espectador quer ligada (sobrevive às reconexões). Some("") = a primeira.
static WANT_CAM: std::sync::LazyLock<Mutex<Option<String>>> = std::sync::LazyLock::new(|| Mutex::new(None));

/// Liga (Some(câmera)) ou desliga (None) a sua câmera na transmissão que você está assistindo.
pub fn set_camera(device: Option<String>) {
    *WANT_CAM.lock().unwrap() = device.clone();
    out(Outgoing::Camera(device));
}

/// Envia uma mensagem no chat da transmissão que você está assistindo.
pub fn send_chat(text: &str) {
    if let Some(t) = crate::control::clean_text(text) {
        out(Outgoing::Chat(t));
    }
}

pub fn send_react(r: &str) {
    if let Some(r) = crate::control::clean_react(r) {
        out(Outgoing::React(r));
    }
}

/// Pede (ou desiste de) jogar no PC do transmissor.
pub fn request_control(pad: bool, kbm: bool) {
    out(Outgoing::ControlReq { pad, kbm });
}

/// Comando IPC que mostra uma mensagem/reação na janela do vídeo.
pub fn osd_chat_cmd(m: &crate::control::ChatMsg) -> String {
    if let Some((x, y)) = m.ping {
        return serde_json::json!({ "command": ["script-message", "gs-ping", m.from, x.to_string(), y.to_string()] }).to_string();
    }
    let kind = if m.react { "gs-react" } else { "gs-chat" };
    serde_json::json!({ "command": ["script-message", kind, m.from, m.text] }).to_string()
}

/// Lado do avatar na janela do vídeo (pixels).
const OSD_AVATAR: u32 = 40;

/// Grava o avatar redondo em BGRA pré-multiplicado (formato do `overlay-add` do mpv) numa pasta
/// temporária e devolve o caminho. O nome do arquivo depende do conteúdo, então só é gravado uma vez.
fn avatar_bgra_file(png: &[u8]) -> Option<PathBuf> {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    png.hash(&mut h);
    let path = std::env::temp_dir().join(format!("gamestream-av-{}-{:016x}.bgra", std::process::id(), h.finish()));
    if path.exists() {
        return Some(path);
    }
    if !crate::avatar::valid(png) {
        return None;
    }
    let img = image::load_from_memory_with_format(png, image::ImageFormat::Png)
        .ok()?
        .resize_exact(OSD_AVATAR, OSD_AVATAR, image::imageops::FilterType::Lanczos3)
        .to_rgba8();
    let c = (OSD_AVATAR as f32 - 1.0) / 2.0;
    let r = OSD_AVATAR as f32 / 2.0;
    let mut out = Vec::with_capacity((OSD_AVATAR * OSD_AVATAR * 4) as usize);
    for (x, y, p) in img.enumerate_pixels() {
        let d = ((x as f32 - c).powi(2) + (y as f32 - c).powi(2)).sqrt();
        let a = p[3] as f32 / 255.0 * (r - d).clamp(0.0, 1.0);
        out.extend_from_slice(&[(p[2] as f32 * a) as u8, (p[1] as f32 * a) as u8, (p[0] as f32 * a) as u8, (a * 255.0) as u8]);
    }
    std::fs::write(&path, out).ok()?;
    Some(path)
}

/// Apaga os avatares temporários deste processo.
pub fn cleanup_osd_avatars() {
    let prefix = format!("gamestream-av-{}-", std::process::id());
    if let Ok(rd) = std::fs::read_dir(std::env::temp_dir()) {
        for e in rd.flatten() {
            if e.file_name().to_string_lossy().starts_with(&prefix) {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
}

/// Comando IPC com quem está na sala (nome, avatar, transmissor, jogando) para a coluna de avatares
/// da janela do vídeo (como no Discord: passe o mouse para ver o nome).
pub fn members_osd_cmd(list: &[state::Member], avatars: &std::collections::HashMap<String, Arc<Vec<u8>>>) -> String {
    let arr: Vec<serde_json::Value> = list
        .iter()
        .take(16)
        .map(|m| {
            let file = avatars.get(&m.id).and_then(|a| avatar_bgra_file(a)).map(|p| p.display().to_string());
            serde_json::json!({ "id": m.id, "name": m.name, "host": m.host, "ctl": m.ctl, "file": file, "size": OSD_AVATAR })
        })
        .collect();
    serde_json::json!({ "command": ["script-message", "gs-members", serde_json::Value::Array(arr).to_string()] }).to_string()
}

fn osd_status_cmd(text: &str) -> String {
    serde_json::json!({ "command": ["script-message", "gs-status", text] }).to_string()
}

/// Evento "client-message" do mpv vindo do painel (chat/reação digitados no vídeo).
fn overlay_event(v: &serde_json::Value) -> Option<OverlayEv> {
    if v["event"] != "client-message" {
        return None;
    }
    let args = v["args"].as_array()?;
    let text = args.get(1)?.as_str()?.to_string();
    match args.first()?.as_str()? {
        "gs-out-chat" => Some(OverlayEv::Chat(text)),
        "gs-out-react" => Some(OverlayEv::React(text)),
        "gs-out-ping" => {
            let (x, y) = (text.parse::<f32>().ok()?, args.get(2)?.as_str()?.parse::<f32>().ok()?);
            ((0.0..=1.0).contains(&x) && (0.0..=1.0).contains(&y)).then_some(OverlayEv::Ping(x, y))
        }
        _ => None,
    }
}

pub async fn open_ipc(pipe: &str) -> Option<tokio::net::windows::named_pipe::NamedPipeClient> {
    for _ in 0..50 {
        if let Ok(c) = ClientOptions::new().open(pipe) {
            return Some(c);
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    None
}

/// Ponte simples entre o chat e uma janela do mpv (usada na pré-visualização do transmissor).
pub async fn overlay_bridge(
    pipe: String,
    mut osd_rx: tokio::sync::mpsc::UnboundedReceiver<String>,
    ev_tx: tokio::sync::mpsc::UnboundedSender<OverlayEv>,
) {
    let Some(client) = open_ipc(&pipe).await else { return };
    let (rd, mut wr) = tokio::io::split(client);
    tokio::spawn(async move {
        let mut lines = tokio::io::BufReader::new(rd).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if let Some(ev) = serde_json::from_str::<serde_json::Value>(&line).ok().as_ref().and_then(overlay_event) {
                let _ = ev_tx.send(ev);
            }
        }
    });
    while let Some(cmd) = osd_rx.recv().await {
        if wr.write_all(format!("{cmd}\n").as_bytes()).await.is_err() {
            break;
        }
    }
}

pub async fn run(o: ViewOpts, stop: impl Future<Output = ()>) -> Result<()> {
    state::view(|v| *v = state::ViewStatus::default());
    let r = tokio::select! {
        r = run_inner(o) => r,
        _ = stop => Ok(()),
    };
    *OUT.lock().unwrap() = None;
    crate::remote::stop_kbm_capture();
    cleanup_osd_avatars();
    state::view(|v| {
        v.roster.clear();
        v.avatars.clear();
        v.chat.clear();
    });
    state::view(|v| {
        v.reconnecting = None;
        v.ctl_granted = 0;
        v.kbm_active = false;
    });
    match &r {
        Ok(()) => {
            let notice = state::view(|v| v.notice.clone());
            view_phase(ViewPhase::Idle, notice.unwrap_or_else(|| crate::i18n::t("Desconectado.").into()));
        }
        Err(e) => {
            log(crate::tr!("Erro: {e:#}", "Error: {e:#}"));
            view_phase(ViewPhase::Error, format!("{e:#}"));
        }
    }
    r
}

/// Erro que não adianta tentar de novo (senha errada, removido, entrada recusada).
#[derive(Debug)]
struct Fatal(String);

impl std::fmt::Display for Fatal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Fatal {}

fn is_fatal(e: &anyhow::Error) -> bool {
    e.downcast_ref::<Fatal>().is_some()
}

/// O transmissor encerrou a transmissão de propósito (botão "Parar"): não adianta reconectar.
#[derive(Debug)]
struct HostEnded;

impl std::fmt::Display for HostEnded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(crate::i18n::t("o transmissor encerrou a transmissão"))
    }
}

impl std::error::Error for HostEnded {}

/// Erro ao encerrar a conexão: códigos 1 (senha), 2 (removido) e 3 (recusado) são definitivos.
fn closed_error(e: quinn::ConnectionError) -> anyhow::Error {
    match e {
        quinn::ConnectionError::ApplicationClosed(c) => {
            let reason = String::from_utf8_lossy(&c.reason).to_string();
            // Código 5 (versões novas) ou o motivo enviado pelas versões antigas (código 0).
            if u64::from(c.error_code) == 5 || reason == "transmissao encerrada pelo host" {
                return anyhow!(HostEnded);
            }
            let why = crate::i18n::t_dyn(&reason);
            let msg = crate::tr!("o transmissor encerrou a conexão: {}", "the streamer closed the connection: {}", why);
            if matches!(u64::from(c.error_code), 1..=4) {
                anyhow!(Fatal(msg))
            } else {
                anyhow!(msg)
            }
        }
        e => anyhow!("{}", crate::tr!("conexão com o transmissor perdida: {e}", "connection to the streamer lost: {e}")),
    }
}

/// Conexão pronta (aprovada pelo transmissor), antes de começar a receber.
struct Joined {
    ep: quinn::Endpoint,
    conn: quinn::Connection,
    first: quinn::RecvStream,
    ctl: Option<(quinn::SendStream, quinn::RecvStream)>,
    /// Código da sala deste transmissor (para a lista de salas recentes).
    room_code: Option<String>,
    via: String,
}

/// Janela do vídeo (mpv). Sobrevive às reconexões: a imagem volta na mesma janela.
struct Player {
    child: tokio::process::Child,
    stdin: Arc<tokio::sync::Mutex<tokio::process::ChildStdin>>,
    pid: u32,
    osd_tx: tokio::sync::mpsc::UnboundedSender<String>,
    ev_rx: tokio::sync::mpsc::UnboundedReceiver<OverlayEv>,
    ipc: tokio::task::JoinHandle<()>,
}

impl Drop for Player {
    fn drop(&mut self) {
        self.ipc.abort();
    }
}

enum End {
    /// O usuário fechou a janela do vídeo.
    Closed,
    /// O transmissor encerrou a transmissão (botão "Parar").
    HostEnded,
    /// Não adianta reconectar.
    Fatal(anyhow::Error),
    /// A conexão caiu: tenta voltar.
    Lost(anyhow::Error),
}

async fn run_inner(o: ViewOpts) -> Result<()> {
    let (out_tx, mut out_rx) = tokio::sync::mpsc::unbounded_channel::<Outgoing>();
    *OUT.lock().unwrap() = Some(out_tx);
    let mut token: Option<String> = None;
    let mut player: Option<Player> = None;
    let mut lost_since: Option<std::time::Instant> = None;
    let mut attempt = 0u32;
    loop {
        let osd = player.as_ref().map(|p| p.osd_tx.clone());
        let joined = connect(&o, token.as_deref(), lost_since.is_some(), osd.as_ref()).await;
        let j = match joined {
            Ok(j) => j,
            Err(e) if lost_since.is_none() || is_fatal(&e) => return Err(e),
            Err(e) if e.downcast_ref::<HostEnded>().is_some() => {
                // Encerrou enquanto tentávamos voltar.
                let name = state::view(|v| if v.host_name.is_empty() { v.peer.clone() } else { v.host_name.clone() });
                let msg = crate::tr!("📴 {name} encerrou a transmissão.", "📴 {name} ended the stream.");
                log(&msg);
                state::view(|v| v.notice = Some(msg));
                return Ok(());
            }
            Err(e) => {
                if lost_since.is_some_and(|t| t.elapsed() > Duration::from_secs(180)) {
                    bail!("{}", crate::tr!("não foi possível reconectar em 3 minutos: {e:#}", "could not reconnect within 3 minutes: {e:#}"));
                }
                attempt += 1;
                let text = crate::tr!(
                    "Conexão perdida. Reconectando (tentativa {})...",
                    "Connection lost. Reconnecting (attempt {})...",
                    attempt
                );
                state::view(|v| v.reconnecting = Some(text.clone()));
                if let Some(p) = &player {
                    let _ = p.osd_tx.send(osd_status_cmd(&text));
                }
                let wait = Duration::from_secs((attempt as u64 * 2).min(5));
                match player.as_mut() {
                    Some(p) => {
                        tokio::select! {
                            _ = tokio::time::sleep(wait) => {}
                            _ = p.child.wait() => { log(crate::i18n::t("Janela de vídeo fechada.")); return Ok(()); }
                        }
                    }
                    None => tokio::time::sleep(wait).await,
                }
                continue;
            }
        };
        if lost_since.take().is_some() {
            log(crate::i18n::t("Reconectado."));
        }
        attempt = 0;
        state::view(|v| v.reconnecting = None);
        match session(j, &o, &mut player, &mut out_rx, &mut token).await {
            End::Closed => return Ok(()),
            End::HostEnded => {
                let name = state::view(|v| if v.host_name.is_empty() { v.peer.clone() } else { v.host_name.clone() });
                let msg = crate::tr!("📴 {name} encerrou a transmissão.", "📴 {name} ended the stream.");
                log(&msg);
                state::view(|v| v.notice = Some(msg.clone()));
                // Mostra o aviso na janela do vídeo por alguns segundos e fecha.
                if let Some(p) = player.as_mut() {
                    let _ = p.osd_tx.send(osd_status_cmd(&msg));
                    tokio::select! {
                        _ = tokio::time::sleep(Duration::from_secs(4)) => {}
                        _ = p.child.wait() => {}
                    }
                }
                return Ok(());
            }
            End::Fatal(e) => return Err(e),
            End::Lost(e) => {
                log(crate::tr!("{e:#}. Tentando reconectar...", "{e:#}. Trying to reconnect..."));
                lost_since = Some(std::time::Instant::now());
                let text = crate::i18n::t("Conexão perdida. Reconectando...").to_string();
                state::view(|v| {
                    v.phase = ViewPhase::Connecting;
                    v.reconnecting = Some(text.clone());
                    v.ctl_granted = 0;
                    v.kbm_active = false;
                });
                if let Some(p) = &player {
                    let _ = p.osd_tx.send(osd_status_cmd(&text));
                }
            }
        }
    }
}

/// Conecta (pelo código da sala ou pelo convite), envia a senha e o nome, e espera a aprovação.
async fn connect(
    o: &ViewOpts,
    token: Option<&str>,
    reconnecting: bool,
    osd: Option<&tokio::sync::mpsc::UnboundedSender<String>>,
) -> Result<Joined> {
    // Na reconexão, mostra a etapa atual (no app e na janela do vídeo).
    let step = |text: String| {
        if reconnecting {
            state::view(|v| v.reconnecting = Some(text.clone()));
            if let Some(tx) = osd {
                let _ = tx.send(osd_status_cmd(&text));
            }
        }
    };
    if !reconnecting {
        view_phase(ViewPhase::Connecting, crate::i18n::t("Conectando..."));
    }
    let mut ep = net::client_bind()?;
    let local_port = ep.local_addr()?.port();
    let reply_addrs: Vec<SocketAddr> = invite::local_addrs(local_port)
        .into_iter()
        .filter(|(_, kind, _)| kind.is_internet())
        .map(|(a, _, _)| a)
        .collect();

    // Código de sala (ex.: KX7-92B): o servidor de encontro devolve o convite do transmissor e o avisa
    // para liberar a conexão. O código é conferido contra a identidade recebida, então o servidor não
    // consegue redirecionar para outro transmissor.
    let server = crate::room::server();
    let mut room_notified = false;
    let room_code;
    let inv = if let Some(code) = crate::room::parse_code(&o.invite) {
        let server = server.clone().context(crate::i18n::t("servidor de encontro não configurado; use o convite GS-..."))?;
        log(crate::tr!("Procurando a sala {}...", "Looking up room {}...", crate::room::display(&code)));
        step(crate::tr!("Conexão perdida. Procurando a sala {}...", "Connection lost. Looking up room {}...", crate::room::display(&code)));
        let (s, c, r) = (server, code.clone(), reply_addrs.clone());
        let invite_text = tokio::task::spawn_blocking(move || crate::room::join(&s, &c, &r)).await??;
        let inv = invite::decode(&invite_text, net::DEFAULT_PORT).await?;
        let fp = inv.fingerprint.context(crate::i18n::t("convite inválido"))?;
        if crate::room::code_from_fp(&fp) != code {
            bail!(Fatal(crate::i18n::t("o convite recebido não corresponde ao código da sala").into()));
        }
        room_notified = true;
        room_code = Some(code);
        inv
    } else {
        let inv = invite::decode(&o.invite, net::DEFAULT_PORT).await?;
        room_code = inv.fingerprint.map(|fp| crate::room::code_from_fp(&fp));
        // Convite completo: avisa o transmissor pela sala dele (se estiver aberta), em segundo plano.
        if let (Some(server), Some(fp)) = (server.clone(), inv.fingerprint) {
            let (code, r) = (crate::room::code_from_fp(&fp), reply_addrs.clone());
            let ok = tokio::task::spawn_blocking(move || crate::room::join(&server, &code, &r).is_ok());
            room_notified = tokio::time::timeout(Duration::from_secs(3), ok).await.is_ok_and(|r| r.unwrap_or(false));
        }
        inv
    };
    if inv.fingerprint.is_none() && !reconnecting {
        log(crate::i18n::t("Aviso: conexão por IP direto, sem verificação da identidade do transmissor. Recomenda-se usar o convite GS-..."));
    }
    if room_notified {
        log(crate::i18n::t("[sala] transmissor avisado; liberando a conexão automaticamente."));
    }
    log(crate::tr!("Conectando ({} endereço(s))...", "Connecting ({} address(es))...", inv.addrs.len()));
    step(crate::i18n::t("Conexão perdida. Conectando de novo ao transmissor...").to_string());
    ep.set_default_client_config(net::client_config(inv.fingerprint)?);

    // Tenta todos os endereços em paralelo, repetindo a cada 2 s pela mesma porta local. Se a conexão
    // direta não responder, exibe o código de resposta: quando o transmissor o usa ("Liberar
    // espectador"), o firewall dele passa a aceitar estas tentativas (hole punching). Com a sala, a
    // liberação é pedida automaticamente pelo servidor (é a mesma coisa), então não há código.
    let reply_code = (!reply_addrs.is_empty() && !reconnecting && !room_notified).then(|| invite::encode_reply(&reply_addrs));
    let show_reply_after = Duration::from_secs(if room_notified { 10 } else { 3 });
    let wait_total = if reconnecting {
        15
    } else if inv.fingerprint.is_some() && reply_code.is_some() {
        180
    } else {
        8
    };
    let start = std::time::Instant::now();
    let mut tries = JoinSet::new();
    let mut next_round = tokio::time::Instant::now();
    let mut shown = false;
    let mut last_err: Option<anyhow::Error> = None;
    let conn = loop {
        if tokio::time::Instant::now() >= next_round {
            for a in inv.addrs.clone() {
                let ep = ep.clone();
                tries.spawn(async move {
                    let c = ep.connect(a, "gamestream")?.await?;
                    anyhow::Ok(c)
                });
            }
            next_round += Duration::from_secs(2);
        }
        if !shown && start.elapsed() >= show_reply_after {
            if let Some(code) = &reply_code {
                shown = true;
                log(crate::tr!("A conexão direta não respondeu. Envie este código de resposta ao transmissor \
                     (ele deve usar \"Liberar espectador\"):\n{code}", "The direct connection did not respond. Send this reply code to the streamer (they must use \"Allow viewer\"):\n{code}"
                ));
                state::view(|v| {
                    v.reply_code = Some(code.clone());
                    v.text = crate::i18n::t("Aguardando o transmissor liberar a conexão...").into();
                });
            }
        }
        if start.elapsed() >= Duration::from_secs(wait_total) {
            let detail = last_err.map(|e| format!(" ({e:#})")).unwrap_or_default();
            bail!(
                "{}", crate::tr!("tempo de conexão esgotado{detail}. Verifique se a transmissão está ativa e se há um caminho de rede \
                 disponível (código de resposta, VPN, IPv6 ou porta aberta).", "connection timed out{detail}. Check that the stream is active and that a network path is available (reply code, VPN, IPv6 or open port)."
            ));
        }
        tokio::select! {
            r = tries.join_next(), if !tries.is_empty() => match r {
                Some(Ok(Ok(c))) => break c,
                Some(Ok(Err(e))) => last_err = Some(e),
                Some(Err(e)) => last_err = Some(e.into()),
                None => {}
            },
            _ = tokio::time::sleep_until(next_round) => {}
        }
    };
    state::view(|v| v.reply_code = None);
    tries.abort_all();

    // Sem convite (IP direto) não há como saber se é mesmo o host: não manda a senha às cegas.
    if inv.fingerprint.is_none() && o.password.is_some() {
        conn.close(0u32.into(), b"");
        bail!(Fatal(crate::i18n::t("por segurança, a senha só é enviada em conexões pelo convite GS-..., que verifica a identidade do transmissor").into()));
    }
    let mut auth = conn.open_uni().await?;
    auth.write_all(o.password.clone().unwrap_or_default().as_bytes()).await?;
    auth.finish()?;
    // Canal de controle: nome (e token de reconexão), chat e controle remoto.
    let ctl = match conn.open_bi().await {
        Ok((mut send, recv)) => {
            let hello = serde_json::json!({
                "t": "hello",
                "name": crate::settings::nick(),
                "token": token,
                "v": env!("CARGO_PKG_VERSION"),
                "p": crate::control::PROTOCOL,
                "vk": inv.fingerprint.map(|fp| crate::control::viewer_key(&fp)),
                "avatar": crate::avatar::load().map(|a| crate::avatar::to_b64(&a)),
            });
            match crate::control::send(&mut send, &hello).await {
                Ok(()) => Some((send, recv)),
                Err(_) => None,
            }
        }
        Err(_) => None,
    };
    let via = conn.remote_address().ip().to_canonical().to_string();
    let code = net::peer_security_code(&conn).unwrap_or_else(|| "?".into());
    let verified = inv.fingerprint.is_some();
    log(crate::tr!("Conectado a {via} (criptografado, {}). Código de segurança do transmissor: {code}", "Connected to {via} (encrypted, {}). Streamer security code: {code}",
        if verified { crate::i18n::t("identidade verificada pelo convite") } else { crate::i18n::t("identidade não verificada") }
    ));
    state::view(|v| {
        if !reconnecting {
            v.text = crate::i18n::t("Aguardando aprovação do transmissor...").into();
        }
        v.peer = via.clone();
        v.code = code.clone();
        v.verified = verified;
        v.reply_code = None;
    });
    // O primeiro fluxo só chega quando o transmissor aprova a entrada (sala de espera). Se ele
    // reiniciou a transmissão, quem volta passa de novo pela sala de espera.
    step(crate::i18n::t("Reconectado ao transmissor; aguardando ele aprovar a sua volta (sala de espera)...").to_string());
    let first = conn.accept_uni().await.map_err(closed_error)?;
    Ok(Joined { ep, conn, first, ctl, room_code, via })
}

/// Inicia a janela do vídeo (mpv) e a ponte de controle (volume, estatísticas, chat).
fn start_player(o: &ViewOpts, via: &str) -> Result<Player> {
    let pipe = format!(r"\\.\pipe\gamestream-mpv-{}", std::process::id());
    let mpv = find_mpv(o.mpv.clone());
    let mut child = Command::new(&mpv)
        .args(mpv_args("viewer", &format!("GameStream - {via}"), Some(&pipe)))
        .stdin(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .with_context(|| crate::tr!("player não encontrado ({}). O mpv.exe deve estar na mesma pasta do gamestream.exe", "player not found ({}). mpv.exe must be in the same folder as gamestream.exe", mpv.display()))?;
    let stdin = Arc::new(tokio::sync::Mutex::new(child.stdin.take().context("stdin do player")?));
    let pid = child.id().unwrap_or(0);
    let (osd_tx, osd_rx) = tokio::sync::mpsc::unbounded_channel();
    let (ev_tx, ev_rx) = tokio::sync::mpsc::unbounded_channel();
    let ipc = tokio::spawn(async move { mpv_control(&pipe, osd_rx, ev_tx).await });
    Ok(Player { child, stdin, pid, osd_tx, ev_rx, ipc })
}

/// Texto de status do controle remoto exibido na janela do vídeo.
fn control_status(granted: u8, capturing: bool) -> String {
    let mut parts = Vec::new();
    if granted & crate::control::CTL_KBM != 0 {
        parts.push(if capturing {
            crate::i18n::t("🎮 Teclado e mouse controlando o jogo do transmissor  ·  F8 para soltar").to_string()
        } else {
            crate::i18n::t("Teclado e mouse liberados: pressione F8 para jogar").to_string()
        });
    }
    if granted & crate::control::CTL_PAD != 0 {
        parts.push(crate::i18n::t("🎮 Seu controle está ligado ao jogo do transmissor").to_string());
    }
    parts.join("\\N")
}

async fn session(
    j: Joined,
    o: &ViewOpts,
    player: &mut Option<Player>,
    out_rx: &mut tokio::sync::mpsc::UnboundedReceiver<Outgoing>,
    token: &mut Option<String>,
) -> End {
    let Joined { ep, conn, first, ctl, room_code, via } = j;
    state::view(|v| {
        v.phase = ViewPhase::Connected;
        v.text = crate::i18n::t("Conectado").into();
        v.has_control = false;
    });
    if player.as_mut().is_some_and(|p| p.child.try_wait().ok().flatten().is_some()) {
        *player = None; // a janela foi fechada durante a reconexão
    }
    if player.is_none() {
        match start_player(o, &via) {
            Ok(p) => *player = Some(p),
            Err(e) => return End::Fatal(e),
        }
    }
    let p = player.as_mut().unwrap();
    let _ = p.osd_tx.send(osd_status_cmd(""));
    let end = run_session(&conn, first, ctl, room_code, p, out_rx, token, o).await;
    crate::remote::stop_kbm_capture();
    conn.close(0u32.into(), b"desconectado");
    let _ = tokio::time::timeout(Duration::from_secs(1), ep.wait_idle()).await;
    end
}

/// Evento do canal de controle (lido numa tarefa separada).
enum CtlEv {
    Msg(serde_json::Value),
    Closed,
}

#[allow(clippy::too_many_arguments)]
async fn run_session(
    conn: &quinn::Connection,
    first: quinn::RecvStream,
    ctl: Option<(quinn::SendStream, quinn::RecvStream)>,
    room_code: Option<String>,
    p: &mut Player,
    out_rx: &mut tokio::sync::mpsc::UnboundedReceiver<Outgoing>,
    token: &mut Option<String>,
    o: &ViewOpts,
) -> End {
    let mut tasks: JoinSet<Result<()>> = JoinSet::new();
    let frames = Arc::new(AtomicU64::new(0));
    let bytes = Arc::new(AtomicU64::new(0));
    // Soma dos quadrados (f32 em bits u64 via acumulação inteira) e nº de amostras de áudio recebidas.
    let audio_sq = Arc::new(AtomicU64::new(0));
    let audio_n = Arc::new(AtomicU64::new(0));
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let (mut last_frames, mut last_bytes, mut last_t) = (0u64, 0u64, std::time::Instant::now());
    let mut pending_first = Some(first);
    let mut have_video = false;
    let connected_at = std::time::Instant::now();
    state::view(|s| s.host_version = None);

    // Canal de controle: leitura numa tarefa (para não perder mensagens no select), escrita aqui.
    let (ctl_tx, mut ctl_rx) = tokio::sync::mpsc::unbounded_channel::<CtlEv>();
    let mut ctl_send = None;
    let mut reader_task = None;
    if let Some((send, recv)) = ctl {
        ctl_send = Some(send);
        reader_task = Some(tokio::spawn(async move {
            let mut lines = tokio::io::BufReader::new(recv).lines();
            while let Ok(Some(l)) = lines.next_line().await {
                if let Ok(v) = serde_json::from_str(&l) {
                    if ctl_tx.send(CtlEv::Msg(v)).is_err() {
                        return;
                    }
                }
            }
            let _ = ctl_tx.send(CtlEv::Closed);
        }));
    }
    struct AbortOnDrop(Option<tokio::task::JoinHandle<()>>);
    impl Drop for AbortOnDrop {
        fn drop(&mut self) {
            if let Some(h) = self.0.take() {
                h.abort();
            }
        }
    }
    let _reader_guard = AbortOnDrop(reader_task);

    // Sua câmera (religa sozinha depois de uma reconexão).
    let mut own_cam = AbortOnDrop(None);
    if let Some(dev) = WANT_CAM.lock().unwrap().clone() {
        own_cam.0 = Some(tokio::spawn(own_camera(conn.clone(), dev)));
    }

    // Controle remoto.
    let mut granted = 0u8;
    let mut pad_task = AbortOnDrop(None);
    let (kbm_tx, mut kbm_rx) = tokio::sync::mpsc::unbounded_channel::<crate::remote::InputEv>();
    let mut last_status = String::new();
    // Pedido de controle feito antes (ex.: antes de uma reconexão) é refeito automaticamente.
    let asked = state::view(|v| v.ctl_asked);
    let mut pending_out: Vec<serde_json::Value> = Vec::new();
    if asked != 0 {
        pending_out.push(serde_json::json!({ "t": "control_req", "pad": asked & crate::control::CTL_PAD != 0, "kbm": asked & crate::control::CTL_KBM != 0 }));
    }

    let result = loop {
        // Mensagens para o transmissor (pedidos de controle, teclas...).
        if !pending_out.is_empty() {
            if let Some(send) = ctl_send.as_mut() {
                for m in pending_out.drain(..) {
                    if crate::control::send(send, &m).await.is_err() {
                        break;
                    }
                }
            } else {
                pending_out.clear();
            }
        }
        // Status do controle remoto na janela do vídeo.
        let status = control_status(granted, crate::remote::kbm_capturing());
        if status != last_status {
            let _ = p.osd_tx.send(osd_status_cmd(&status));
            last_status = status;
        }

        let mut st = if let Some(s) = pending_first.take() {
            s
        } else {
            tokio::select! {
                st = conn.accept_uni() => match st {
                    Ok(s) => s,
                    Err(e) => {
                        let e = closed_error(e);
                        break if e.downcast_ref::<HostEnded>().is_some() {
                            End::HostEnded
                        } else if is_fatal(&e) {
                            End::Fatal(e)
                        } else {
                            End::Lost(e)
                        };
                    }
                },
                Some(r) = tasks.join_next() => {
                    match r {
                        Ok(Ok(())) => continue,
                        Ok(Err(e)) => break End::Lost(anyhow!("{}", crate::tr!("transmissão interrompida: {e:#}", "stream interrupted: {e:#}"))),
                        Err(e) => break End::Lost(e.into()),
                    }
                }
                _ = p.child.wait() => { log(crate::i18n::t("Janela de vídeo fechada.")); break End::Closed; }
                ev = ctl_rx.recv() => {
                    match ev {
                        Some(CtlEv::Msg(v)) => {
                            match v["t"].as_str().unwrap_or("") {
                                "welcome" => {
                                    let host = crate::control::clean_name(v["host"].as_str().unwrap_or(""));
                                    if let Some(t) = v["token"].as_str() {
                                        *token = Some(t.chars().take(64).collect());
                                    }
                                    let you: String = v["you"].as_str().unwrap_or("").chars().take(16).collect();
                                    let hv: String = v["v"].as_str().unwrap_or("?").chars().take(16).collect();
                                    if v["p"].as_u64().unwrap_or(1) < crate::control::MIN_PROTOCOL as u64 {
                                        break End::Fatal(anyhow!(Fatal(crate::tr!(
                                            "o transmissor usa a versão {hv}, antiga demais para esta. Peça para ele abrir o GameStream de novo para atualizar.",
                                            "the streamer uses version {hv}, too old for this one. Ask them to reopen GameStream to update.",
                                        ))));
                                    }
                                    state::view(|s| {
                                        s.host_name = host.clone();
                                        s.has_control = true;
                                        s.my_id = you;
                                        s.host_version = Some(hv);
                                    });
                                    if let Some(code) = &room_code {
                                        let name = if host.is_empty() { state::view(|s| s.peer.clone()) } else { host.clone() };
                                        crate::settings::add_recent(code, &name);
                                    }
                                }
                                "ping" => {
                                    if let Some(m) = crate::control::ChatMsg::from_json(&v) {
                                        let _ = p.osd_tx.send(osd_chat_cmd(&m));
                                    }
                                }
                                "chat" | "react" => {
                                    if let Some(m) = crate::control::ChatMsg::from_json(&v) {
                                        if m.react {
                                            log(format!("[chat] {} {}", m.from, m.text));
                                        } else {
                                            log(format!("[chat] {}: {}", m.from, m.text));
                                        }
                                        let _ = p.osd_tx.send(osd_chat_cmd(&m));
                                        if !m.react && state::view(|s| s.my_id != m.id) {
                                            crate::sounds::play(crate::sounds::Sound::Chat);
                                        }
                                        state::view(|s| state::push_chat(&mut s.chat, m));
                                    }
                                }
                                "control" => {
                                    let new = (if v["pad"].as_bool() == Some(true) { crate::control::CTL_PAD } else { 0 })
                                        | (if v["kbm"].as_bool() == Some(true) { crate::control::CTL_KBM } else { 0 });
                                    if new != granted {
                                        if new != 0 && granted == 0 {
                                            crate::sounds::play(crate::sounds::Sound::Request);
                                        }
                                        if new != 0 {
                                            log(crate::tr!("[controle] o transmissor liberou: {}", "[control] the streamer allowed: {}", crate::control::ctl_desc(new)));
                                        } else if granted != 0 {
                                            log(crate::i18n::t("[controle] o transmissor encerrou o seu controle."));
                                            // Revogado pelo transmissor: não pede de novo sozinho (nem ao reconectar).
                                            state::view(|s| s.ctl_asked = 0);
                                        }
                                    }
                                    if new & crate::control::CTL_KBM != 0 && granted & crate::control::CTL_KBM == 0 {
                                        crate::remote::start_kbm_capture(kbm_tx.clone(), p.pid);
                                    } else if new & crate::control::CTL_KBM == 0 {
                                        crate::remote::stop_kbm_capture();
                                    }
                                    if new & crate::control::CTL_PAD != 0 && pad_task.0.is_none() {
                                        pad_task.0 = Some(spawn_pad_sender(conn.clone()));
                                    } else if new & crate::control::CTL_PAD == 0 {
                                        if let Some(h) = pad_task.0.take() { h.abort(); }
                                    }
                                    granted = new;
                                    state::view(|s| {
                                        s.ctl_granted = new;
                                        s.kbm_active = crate::remote::kbm_capturing();
                                    });
                                }
                                "roster" => {
                                    let list: Vec<state::Member> = v["list"]
                                        .as_array()
                                        .map(|a| {
                                            a.iter()
                                                .take(64)
                                                .map(|m| state::Member {
                                                    id: m["id"].as_str().unwrap_or("").chars().take(16).collect(),
                                                    name: crate::control::clean_name(m["name"].as_str().unwrap_or("")),
                                                    host: m["host"].as_bool() == Some(true),
                                                    ctl: m["ctl"].as_u64().unwrap_or(0) as u8,
                                                    cam: m["cam"].as_bool() == Some(true),
                                                })
                                                .collect()
                                        })
                                        .unwrap_or_default();
                                    let names: Vec<&str> = list.iter().map(|m| m.name.as_str()).collect();
                                    let before = state::view(|s| s.roster.len());
                                    if state::view(|s| s.roster.iter().map(|m| m.name.as_str()).collect::<Vec<_>>() != names) {
                                        log(crate::tr!("[sala] {} na sala: {}", "[room] {} in the room: {}", names.len(), names.join(", ")));
                                    }
                                    if before > 0 && names.len() > before {
                                        crate::sounds::play(crate::sounds::Sound::Join);
                                    }
                                    let cmd = state::view(|s| {
                                        s.avatars.retain(|id, _| list.iter().any(|m| &m.id == id));
                                        s.roster = list;
                                        members_osd_cmd(&s.roster, &s.avatars)
                                    });
                                    let _ = p.osd_tx.send(cmd);
                                }
                                "avatar" => {
                                    let id: String = v["id"].as_str().unwrap_or("").chars().take(16).collect();
                                    if let Some(png) = v["png"].as_str().and_then(crate::avatar::from_b64) {
                                        if let (true, Some(code)) = (id == "h", &room_code) {
                                            crate::settings::save_recent_avatar(code, &png);
                                        }
                                        let cmd = state::view(|s| {
                                            if s.avatars.len() < 64 {
                                                s.avatars.insert(id, png);
                                            }
                                            members_osd_cmd(&s.roster, &s.avatars)
                                        });
                                        let _ = p.osd_tx.send(cmd);
                                    }
                                }
                                "notice" => {
                                    if let Some(t) = v["text"].as_str() {
                                        log(format!("[{}] {}", crate::i18n::t("transmissor"), crate::i18n::t_dyn(t)));
                                    }
                                }
                                _ => {}
                            }
                        }
                        Some(CtlEv::Closed) | None => {}
                    }
                    continue;
                }
                m = out_rx.recv() => {
                    if let Some(m) = m {
                        let v = match m {
                            Outgoing::Chat(text) => serde_json::json!({ "t": "chat", "text": text }),
                            Outgoing::React(r) => serde_json::json!({ "t": "react", "r": r }),
                            Outgoing::Camera(dev) => {
                                if let Some(h) = own_cam.0.take() {
                                    h.abort();
                                    state::view(|s| s.cam_on = false);
                                }
                                if let Some(dev) = dev {
                                    own_cam.0 = Some(tokio::spawn(own_camera(conn.clone(), dev)));
                                }
                                continue;
                            }
                            Outgoing::ControlReq { pad, kbm } => {
                                let bits = (if pad { crate::control::CTL_PAD } else { 0 }) | (if kbm { crate::control::CTL_KBM } else { 0 });
                                state::view(|s| s.ctl_asked = bits);
                                serde_json::json!({ "t": "control_req", "pad": pad, "kbm": kbm })
                            }
                        };
                        if ctl_send.is_none() {
                            log(crate::i18n::t("O transmissor usa uma versão sem chat/controle remoto. Peça para ele atualizar o GameStream."));
                        }
                        pending_out.push(v);
                    }
                    continue;
                }
                ev = p.ev_rx.recv() => {
                    match ev {
                        Some(OverlayEv::Chat(t)) => if let Some(t) = crate::control::clean_text(&t) { pending_out.push(serde_json::json!({ "t": "chat", "text": t })) },
                        Some(OverlayEv::React(r)) => if let Some(r) = crate::control::clean_react(&r) { pending_out.push(serde_json::json!({ "t": "react", "r": r })) },
                        Some(OverlayEv::Ping(x, y)) => pending_out.push(serde_json::json!({ "t": "ping", "x": x, "y": y })),
                        None => {}
                    }
                    continue;
                }
                ev = kbm_rx.recv() => {
                    use crate::remote::InputEv;
                    match ev {
                        Some(InputEv::Move { dx, dy }) => {
                            let _ = conn.send_datagram(bytes::Bytes::copy_from_slice(&crate::control::encode_mouse(dx, dy)));
                        }
                        Some(InputEv::Key { sc, ext, down }) => pending_out.push(serde_json::json!({ "t": "key", "sc": sc, "ext": ext, "down": down })),
                        Some(InputEv::Button { b, down }) => pending_out.push(serde_json::json!({ "t": "mbtn", "b": b, "down": down })),
                        Some(InputEv::Wheel { delta, horizontal }) => pending_out.push(serde_json::json!({ "t": "wheel", "d": delta, "h": horizontal })),
                        Some(InputEv::Capturing(on)) => state::view(|s| s.kbm_active = on),
                        None => {}
                    }
                    continue;
                }
                _ = tick.tick() => {
                    let (f, b) = (frames.load(Ordering::Relaxed), bytes.load(Ordering::Relaxed));
                    let dt = last_t.elapsed().as_secs_f64().max(0.2);
                    let ping = conn.stats().path.rtt.as_millis() as u32;
                    let (sq, n) = (audio_sq.swap(0, Ordering::Relaxed), audio_n.swap(0, Ordering::Relaxed));
                    let audio_db = (n > 0).then(|| {
                        let rms = (((sq as f64) * 256.0) / n as f64).sqrt() / 32768.0;
                        if rms > 0.0 { (20.0 * rms.log10()).max(-90.0) as f32 } else { -90.0 }
                    });
                    if connected_at.elapsed() > Duration::from_secs(5) && state::view(|s| s.host_version.is_none()) {
                        log(crate::i18n::t("O transmissor usa uma versão antiga do GameStream: chat, lista da sala e jogar junto ficam indisponíveis. Peça para ele abrir o app de novo para atualizar."));
                        state::view(|s| s.host_version = Some(String::new()));
                    }
                    state::view(|v| {
                        v.stats.audio_db = audio_db;
                        v.stats.fps = ((f - last_frames) as f64 / dt).round() as u32;
                        v.stats.mbps = (b - last_bytes) as f64 * 8.0 / 1e6 / dt;
                        v.stats.ping_ms = ping;
                        let s = state::Sample { ping: ping as f32, loss: v.stats.loss as f32, mbps: v.stats.mbps as f32, fps: v.stats.fps as f32 };
                        state::push_sample(&mut v.history, s);
                        v.pad_found = crate::remote::read_pad().is_some();
                    });
                    (last_frames, last_bytes, last_t) = (f, b, std::time::Instant::now());
                    continue;
                }
            }
        };
        let mut kind = [0u8; 1];
        if let Err(e) = st.read_exact(&mut kind).await {
            break End::Lost(e.into());
        }
        match kind[0] {
            net::STREAM_VIDEO => {
                if have_video {
                    break End::Lost(anyhow!(crate::i18n::t("o transmissor enviou dois fluxos de vídeo")));
                }
                have_video = true;
                let (frames, bytes, stdin) = (frames.clone(), bytes.clone(), p.stdin.clone());
                tasks.spawn(async move {
                    let mut pipe_in = stdin.lock().await;
                    let mut buf = Vec::new();
                    while net::read_msg(&mut st, &mut buf).await? {
                        frames.fetch_add(count_frames(&buf), Ordering::Relaxed);
                        bytes.fetch_add(buf.len() as u64, Ordering::Relaxed);
                        pipe_in.write_all(&buf).await?;
                    }
                    Err(anyhow!(crate::i18n::t("o vídeo parou de chegar")))
                });
            }
            net::STREAM_CAM => {
                // Câmera de outra pessoa: [tamanho do id][id] e depois o vídeo.
                let mut n = [0u8; 1];
                if st.read_exact(&mut n).await.is_err() {
                    continue;
                }
                let mut id = vec![0u8; n[0] as usize];
                if st.read_exact(&mut id).await.is_err() {
                    continue;
                }
                let id = String::from_utf8_lossy(&id).to_string();
                if !crate::settings::show_cams() {
                    continue;
                }
                tokio::spawn(async move {
                    // A câmera pode chegar antes da lista da sala (quem acabou de entrar): espera o nome.
                    let mut name = String::new();
                    for _ in 0..30 {
                        if let Some(n) = state::view(|s| s.roster.iter().find(|m| m.id == id).map(|m| m.name.clone())) {
                            name = n;
                            break;
                        }
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }
                    if name.is_empty() {
                        name = "?".into();
                    }
                    // Continua recebendo mesmo com o vídeo desativado (só para você), para poder
                    // mostrar de novo na hora pelo card "📷 Câmera".
                    let mut view = crate::cam::CamView::new(&name);
                    let mut announced = false;
                    let mut buf = Vec::new();
                    while let Ok(true) = net::read_msg(&mut st, &mut buf).await {
                        view.feed(&buf, crate::cam::has_sps(&buf)).await;
                        if view.showing() && !announced {
                            announced = true;
                            log(crate::tr!("[câmera] mostrando a câmera de {name}.", "[camera] showing {name}'s camera."));
                        }
                    }
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
                if let Err(e) = st.read_exact(&mut hdr).await {
                    break End::Lost(e.into());
                }
                let rate = u32::from_le_bytes(hdr[0..4].try_into().unwrap());
                let ch = u16::from_le_bytes(hdr[4..6].try_into().unwrap());
                match audio::start_player(rate, ch, o.audio_device.clone()) {
                    Ok(mut player) => {
                        let (asq, an) = (audio_sq.clone(), audio_n.clone());
                        tasks.spawn(async move {
                            let mut buf = Vec::new();
                            while net::read_msg(&mut st, &mut buf).await? {
                                let mut sq = 0u64;
                                for c in buf.chunks_exact(2) {
                                    let v = i16::from_le_bytes([c[0], c[1]]) as i64;
                                    sq += (v * v) as u64;
                                }
                                asq.fetch_add(sq >> 8, Ordering::Relaxed);
                                an.fetch_add((buf.len() / 2) as u64, Ordering::Relaxed);
                                player.push(&buf);
                            }
                            anyhow::Ok(())
                        });
                    }
                    Err(e) => log(crate::tr!("Áudio indisponível: {e:#}", "Audio unavailable: {e:#}")),
                }
            }
            net::STREAM_AUDIO_OPUS => {
                let mut hdr = [0u8; 6];
                if let Err(e) = st.read_exact(&mut hdr).await {
                    break End::Lost(e.into());
                }
                let rate = u32::from_le_bytes(hdr[0..4].try_into().unwrap());
                let ch = u16::from_le_bytes(hdr[4..6].try_into().unwrap());
                let channels = if ch == 1 { opus::Channels::Mono } else { opus::Channels::Stereo };
                match (opus::Decoder::new(rate, channels), audio::start_player(rate, ch, o.audio_device.clone())) {
                    (Ok(mut dec), Ok(mut player)) => {
                        let (asq, an) = (audio_sq.clone(), audio_n.clone());
                        tasks.spawn(async move {
                            let mut buf = Vec::new();
                            let mut pcm = vec![0f32; 5760 * ch.max(1) as usize];
                            while net::read_msg(&mut st, &mut buf).await? {
                                if let Ok(n) = dec.decode_float(&buf, &mut pcm, false) {
                                    let s = &pcm[..n * ch.max(1) as usize];
                                    let sq: f64 = s.iter().map(|x| (*x as f64 * 32767.0).powi(2)).sum();
                                    asq.fetch_add((sq as u64) >> 8, Ordering::Relaxed);
                                    an.fetch_add(s.len() as u64, Ordering::Relaxed);
                                    player.push_f32(s);
                                }
                            }
                            anyhow::Ok(())
                        });
                    }
                    (Err(e), _) => log(crate::tr!("Áudio indisponível: {e:#}", "Audio unavailable: {e:#}")),
                    (_, Err(e)) => log(crate::tr!("Áudio indisponível: {e:#}", "Audio unavailable: {e:#}")),
                }
            }
            k => log(crate::tr!("fluxo desconhecido: {k}", "unknown stream: {k}")),
        }
    };
    tasks.abort_all();
    result
}

/// Sua câmera: captura, envia ao transmissor e mostra para você (espelho).
async fn own_camera(conn: quinn::Connection, device: String) {
    let r = async {
        let ffmpeg = match crate::updater::ffmpeg_present() {
            Some(f) => f,
            None => {
                log(crate::i18n::t("Baixando o FFmpeg (~90 MB) para a câmera. Isso acontece só uma vez."));
                tokio::task::spawn_blocking(crate::updater::ensure_ffmpeg).await??
            }
        };
        let device = if device.is_empty() {
            let f = ffmpeg.clone();
            tokio::task::spawn_blocking(move || crate::cam::list(&f)).await?.into_iter().next().context(crate::i18n::t("nenhuma câmera encontrada"))?
        } else {
            device
        };
        let mut child = crate::cam::spawn_capture(&ffmpeg, &device)?;
        let mut s = conn.open_uni().await?;
        s.write_all(&[net::STREAM_CAM]).await?;
        let me = crate::settings::nick();
        let mut mirror = crate::cam::CamView::new(&me);
        state::view(|v| {
            v.cam_on = true;
            v.cam_error = None;
        });
        log(crate::tr!("[câmera] sua câmera está ligada ({device}).", "[camera] your camera is on ({device})."));
        let mut buf = vec![0u8; 256 * 1024];
        let mut got = false;
        while let Some(n) = crate::cam::read_chunk(&mut child, &mut buf).await {
            got = true;
            s.write_all(&net::frame(&buf[..n])).await?;
            // Espelho: fechar/desativar só esconde para você; a câmera continua para os outros.
            mirror.feed(&buf[..n], crate::cam::has_sps(&buf[..n])).await;
        }
        let _ = s.finish();
        if !got {
            anyhow::bail!("{}", crate::i18n::t("a câmera não respondeu (pode estar em uso por outro programa)"));
        }
        anyhow::Ok(())
    }
    .await;
    state::view(|v| {
        v.cam_on = false;
        if let Err(e) = &r {
            v.cam_error = Some(format!("{e:#}"));
        }
    });
    match r {
        Ok(()) => log(crate::i18n::t("[câmera] sua câmera foi desligada.")),
        Err(e) => log(crate::tr!("[câmera] {e:#}", "[camera] {e:#}")),
    }
}

/// Envia o estado do controle (XInput) ao transmissor: a cada mudança e, parado, a cada 100 ms.
fn spawn_pad_sender(conn: quinn::Connection) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut iv = tokio::time::interval(Duration::from_millis(8));
        iv.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut last: Option<crate::control::Pad> = None;
        let mut last_sent = std::time::Instant::now();
        loop {
            iv.tick().await;
            let p = crate::remote::read_pad();
            let p = match (p, last) {
                (Some(p), _) => p,
                (None, Some(_)) => crate::control::Pad::default(), // controle desconectado: solta tudo
                (None, None) => continue,
            };
            if Some(p) != last || last_sent.elapsed() >= Duration::from_millis(100) {
                if conn.send_datagram(bytes::Bytes::copy_from_slice(&p.encode())).is_err() {
                    return;
                }
                last = if p == crate::control::Pad::default() && crate::remote::read_pad().is_none() { None } else { Some(p) };
                last_sent = std::time::Instant::now();
            }
        }
    })
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
async fn mpv_control(
    pipe: &str,
    mut osd_rx: tokio::sync::mpsc::UnboundedReceiver<String>,
    ev_tx: tokio::sync::mpsc::UnboundedSender<OverlayEv>,
) {
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
                    log(crate::tr!("[player] controles indisponíveis: {e}", "[player] controls unavailable: {e}"));
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
                if let Some(ev) = overlay_event(&v) {
                    let _ = ev_tx.send(ev);
                    continue;
                }
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
        while let Ok(cmd) = osd_rx.try_recv() {
            if wr.write_all(format!("{cmd}\n").as_bytes()).await.is_err() {
                return std::future::pending().await;
            }
        }
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
