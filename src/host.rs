//! Host: captura a tela (DXGI, na GPU) + encoda HEVC no encoder de hardware via FFmpeg,
//! captura o áudio do sistema e distribui para todos os viewers via QUIC.
//! Bitrate automático: reduz quando algum viewer perde pacotes/fica para trás, sobe aos poucos quando está tudo limpo.

use std::{
    collections::HashMap,
    future::Future,
    net::SocketAddr,
    process::Stdio,
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

use anyhow::{bail, Context, Result};
use bytes::Bytes;
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, BufReader},
    process::Command,
    sync::{broadcast, watch},
};

use crate::{
    audio,
    control::{self, ChatMsg, CTL_KBM, CTL_PAD},
    invite, net, remote,
    sources::{self, Capture, Source},
    state::{self, log},
};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
pub const MIN_BITRATE: u32 = 3;

/// Canal para os códigos de resposta enquanto há uma transmissão ativa.
static PUNCH: std::sync::LazyLock<Mutex<Option<tokio::sync::mpsc::UnboundedSender<Vec<SocketAddr>>>>> =
    std::sync::LazyLock::new(|| Mutex::new(None));

/// Sala de espera e remoção de espectadores (comandos vindos da interface).
static NEXT_PENDING: AtomicU64 = AtomicU64::new(1);
static DECISIONS: std::sync::LazyLock<Mutex<HashMap<u64, tokio::sync::oneshot::Sender<bool>>>> =
    std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));
static CONNS: std::sync::LazyLock<Mutex<HashMap<String, quinn::Connection>>> =
    std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));
static BANNED: std::sync::LazyLock<Mutex<std::collections::HashSet<std::net::IpAddr>>> =
    std::sync::LazyLock::new(|| Mutex::new(std::collections::HashSet::new()));

/// Chat da transmissão atual (mensagens do transmissor e de todos os espectadores).
static CHAT: std::sync::LazyLock<Mutex<Option<broadcast::Sender<ChatMsg>>>> = std::sync::LazyLock::new(|| Mutex::new(None));
/// Tokens de reconexão (quem já foi aprovado volta sem passar pela sala de espera) → controle liberado.
static TOKENS: std::sync::LazyLock<Mutex<HashMap<String, u8>>> = std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));
/// Controle remoto de cada espectador conectado (chave = endereço).
static REMOTE: std::sync::LazyLock<Mutex<HashMap<String, RemoteSlot>>> = std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));

/// Quem está na transmissão (transmissor + espectadores aprovados), com avatar. Toda mudança vai
/// para todos pelo canal ROSTER.
struct MemberEntry {
    m: state::Member,
    avatar: Option<Arc<Vec<u8>>>,
}
static MEMBERS: std::sync::LazyLock<Mutex<Vec<MemberEntry>>> = std::sync::LazyLock::new(|| Mutex::new(Vec::new()));
static ROSTER: std::sync::LazyLock<Mutex<Option<broadcast::Sender<serde_json::Value>>>> = std::sync::LazyLock::new(|| Mutex::new(None));
static NEXT_MEMBER: AtomicU64 = AtomicU64::new(1);

fn roster_json() -> serde_json::Value {
    let list: Vec<serde_json::Value> = MEMBERS
        .lock()
        .unwrap()
        .iter()
        .map(|e| serde_json::json!({ "id": e.m.id, "name": e.m.name, "host": e.m.host, "ctl": e.m.ctl }))
        .collect();
    serde_json::json!({ "t": "roster", "list": list })
}

/// Quem está na sala e os avatares (para a coluna de avatares da prévia).
fn members_snapshot() -> (Vec<state::Member>, HashMap<String, Arc<Vec<u8>>>) {
    let m = MEMBERS.lock().unwrap();
    (m.iter().map(|e| e.m.clone()).collect(), m.iter().filter_map(|e| Some((e.m.id.clone(), e.avatar.clone()?))).collect())
}

fn publish_roster() {
    if let Some(tx) = ROSTER.lock().unwrap().as_ref() {
        let _ = tx.send(roster_json());
    }
}

fn avatar_msg(id: &str, png: &[u8]) -> serde_json::Value {
    serde_json::json!({ "t": "avatar", "id": id, "png": crate::avatar::to_b64(png) })
}

struct RemoteSlot {
    id: String,
    name: String,
    token: String,
    granted: Arc<AtomicU8>,
    /// Avisa a tarefa do espectador que a permissão mudou (com um aviso opcional para ele).
    notify: tokio::sync::mpsc::UnboundedSender<Option<String>>,
}

/// Envia uma mensagem no chat como transmissor.
pub fn send_chat(text: &str) {
    if let (Some(tx), Some(text)) = (CHAT.lock().unwrap().as_ref(), control::clean_text(text)) {
        let _ = tx.send(ChatMsg { id: "h".into(), from: crate::settings::nick(), text, react: false });
    }
}

/// Envia uma reação como transmissor.
pub fn send_react(r: &str) {
    if let (Some(tx), Some(r)) = (CHAT.lock().unwrap().as_ref(), control::clean_react(r)) {
        let _ = tx.send(ChatMsg { id: "h".into(), from: crate::settings::nick(), text: r, react: true });
    }
}

/// Libera (ou revoga, com 0) o controle remoto de um espectador. `bits` = CTL_PAD | CTL_KBM.
pub fn set_control(key: &str, bits: u8) {
    let remote = REMOTE.lock().unwrap();
    let Some(slot) = remote.get(key) else { return };
    slot.granted.store(bits, Ordering::Relaxed);
    TOKENS.lock().unwrap().insert(slot.token.clone(), bits);
    let _ = slot.notify.send(None);
    if let Some(e) = MEMBERS.lock().unwrap().iter_mut().find(|e| e.m.id == slot.id) {
        e.m.ctl = bits;
    }
    publish_roster();
    if bits == 0 {
        log(crate::tr!("[controle] controle de {} revogado", "[control] {}'s control revoked", slot.name));
    } else {
        log(crate::tr!("[controle] {} pode jogar com: {}", "[control] {} may play with: {}", slot.name, control::ctl_desc(bits)));
    }
}

/// Revoga o controle remoto de todos os espectadores.
pub fn revoke_all_control() {
    let keys: Vec<String> = REMOTE.lock().unwrap().iter().filter(|(_, s)| s.granted.load(Ordering::Relaxed) != 0).map(|(k, _)| k.clone()).collect();
    for k in keys {
        set_control(&k, 0);
    }
}

/// Aprova (true) ou recusa (false) um espectador na sala de espera.
pub fn decide(id: u64, approve: bool) {
    if let Some(tx) = DECISIONS.lock().unwrap().remove(&id) {
        let _ = tx.send(approve);
    }
}

/// Remove um espectador conectado e bloqueia o endereço dele até o fim da transmissão.
pub fn kick(key: &str) {
    if let Some(slot) = REMOTE.lock().unwrap().get(key) {
        slot.granted.store(0, Ordering::Relaxed);
        TOKENS.lock().unwrap().remove(&slot.token);
    }
    if let Some(conn) = CONNS.lock().unwrap().remove(key) {
        BANNED.lock().unwrap().insert(conn.remote_address().ip().to_canonical());
        conn.close(2u32.into(), b"removido pelo transmissor");
    }
}

/// Libera um espectador a partir do código de resposta dele (GS-R-...). Retorna quantos endereços.
pub fn release_viewer(code: &str) -> Result<usize> {
    let addrs = invite::decode_reply(code)?;
    let n = addrs.len();
    PUNCH
        .lock()
        .unwrap()
        .as_ref()
        .context(crate::i18n::t("nenhuma transmissão ativa"))?
        .send(addrs)
        .map_err(|_| anyhow::anyhow!(crate::i18n::t("nenhuma transmissão ativa")))?;
    Ok(n)
}

/// O host quer ver a própria transmissão (prévia sem som). A interface liga/desliga a qualquer momento.
static PREVIEW: AtomicBool = AtomicBool::new(true);

pub fn set_preview(on: bool) {
    PREVIEW.store(on, Ordering::Relaxed);
}

pub fn preview_wanted() -> bool {
    PREVIEW.load(Ordering::Relaxed)
}

/// Mantém (ou fecha) a janela de prévia conforme `PREVIEW`. Nunca termina.
async fn preview_loop(vtx: broadcast::Sender<VideoChunk>) {
    loop {
        while !preview_wanted() {
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        if let Err(e) = run_preview(&vtx).await {
            log(crate::tr!("[pré-visualização] {e:#}", "[preview] {e:#}"));
            set_preview(false);
        }
        state::host(|h| h.preview_open = false);
    }
}

/// Exibe o vídeo que sai do encoder (o mesmo recebido pelos espectadores) num mpv local, sem áudio.
async fn run_preview(vtx: &broadcast::Sender<VideoChunk>) -> Result<()> {
    let mpv = crate::viewer::find_mpv(None);
    let pipe = format!(r"\\.\pipe\gamestream-preview-{}", std::process::id());
    let mut child = Command::new(&mpv)
        .args(crate::viewer::mpv_args("preview", crate::i18n::t("GameStream - Pré-visualização (sem áudio)"), Some(&pipe)))
        .stdin(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .with_context(|| crate::tr!("não foi possível abrir o player ({})", "could not open the player ({})", mpv.display()))?;
    let mut stdin = child.stdin.take().unwrap();
    let mut rx = vtx.subscribe();
    let mut waiting_keyframe = true;
    // Chat na janela de prévia: mostra as mensagens e permite responder (Enter) e reagir (1 a 6).
    let roster_rx = ROSTER.lock().unwrap().as_ref().map(|tx| tx.subscribe());
    let bridge = CHAT.lock().unwrap().as_ref().map(|tx| tx.subscribe()).map(|chat_rx| {
        let (osd_tx, osd_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        let mut roster_rx = roster_rx;
        let send_members = {
            let osd_tx = osd_tx.clone();
            move || {
                let (list, avatars) = members_snapshot();
                let _ = osd_tx.send(crate::viewer::members_osd_cmd(&list, &avatars));
            }
        };
        send_members();
        let (ev_tx, mut ev_rx) = tokio::sync::mpsc::unbounded_channel::<crate::viewer::OverlayEv>();
        let pipe = pipe.clone();
        tokio::spawn(async move {
            let ipc = tokio::spawn(crate::viewer::overlay_bridge(pipe, osd_rx, ev_tx));
            let mut chat_rx = chat_rx;
            loop {
                tokio::select! {
                    m = chat_rx.recv() => match m {
                        Ok(m) => { let _ = osd_tx.send(crate::viewer::osd_chat_cmd(&m)); }
                        Err(broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(broadcast::error::RecvError::Closed) => break,
                    },
                    r = async { match roster_rx.as_mut() { Some(rx) => rx.recv().await, None => std::future::pending().await } } => match r {
                        Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => send_members(),
                        Err(broadcast::error::RecvError::Closed) => roster_rx = None,
                    },
                    e = ev_rx.recv() => match e {
                        Some(crate::viewer::OverlayEv::Chat(t)) => send_chat(&t),
                        Some(crate::viewer::OverlayEv::React(r)) => send_react(&r),
                        None => break,
                    },
                }
            }
            ipc.abort();
        })
    });
    state::host(|h| h.preview_open = true);
    log(crate::i18n::t("[pré-visualização] aberta (sem áudio)."));
    loop {
        tokio::select! {
            m = rx.recv() => match m {
                Ok(c) => {
                    let data = if waiting_keyframe {
                        let Some(off) = c.vps else { continue };
                        waiting_keyframe = false;
                        c.raw.slice(off..)
                    } else {
                        c.raw.clone()
                    };
                    if tokio::io::AsyncWriteExt::write_all(&mut stdin, &data).await.is_err() {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => waiting_keyframe = true,
                Err(broadcast::error::RecvError::Closed) => break,
            },
            _ = child.wait() => {
                // O usuário fechou a janela: entende como "não quero prévia".
                set_preview(false);
                break;
            }
            _ = async { while preview_wanted() { tokio::time::sleep(Duration::from_millis(200)).await; } } => break,
        }
    }
    if let Some(b) = bridge {
        b.abort();
    }
    log(crate::i18n::t("[pré-visualização] fechada."));
    Ok(())
}

pub struct HostOpts {
    pub port: u16,
    pub source: Source,
    /// Bitrate fixo, ou o teto quando `auto_bitrate`.
    pub bitrate_mbps: u32,
    pub auto_bitrate: bool,
    pub fps: u32,
    pub encoder: String,
    pub audio: bool,
    pub audio_device: audio::AudioSel,
    /// Microfone misturado ao áudio (None = desligado).
    pub mic: Option<String>,
    pub password: Option<String>,
    pub upnp: bool,
    /// Usar o servidor de encontro (código curto e conexão automática).
    pub room: bool,
    /// Sala de espera: cada espectador precisa ser aprovado antes de receber a transmissão.
    pub approval: bool,
    /// Áudio sem compressão (PCM, ~1,5 Mbps por espectador) em vez de Opus (~0,16 Mbps).
    pub audio_pcm: bool,
    pub ffmpeg: String,
}

#[derive(Clone)]
struct VideoChunk {
    raw: Bytes,
    framed: Bytes,
    /// Posição de um VPS (início de keyframe) dentro do chunk, se houver.
    vps: Option<usize>,
    /// Quando o encoder produziu este trecho (para medir o atraso de cada espectador).
    at: Instant,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Enc {
    Amf,
    Nvenc,
    Qsv,
    X265,
}

impl Enc {
    fn name(self) -> &'static str {
        match self {
            Enc::Amf => "AMD (hevc_amf)",
            Enc::Nvenc => "NVIDIA (hevc_nvenc)",
            Enc::Qsv => "Intel (hevc_qsv)",
            Enc::X265 => crate::i18n::t("CPU (libx265, desempenho reduzido)"),
        }
    }
}

struct Viewer {
    conn: quinn::Connection,
    /// Vezes em que o espectador ficou tão para trás que quadros foram descartados.
    lagged: Arc<AtomicU64>,
    /// Tempo (ms) em que o envio de vídeo ficou travado esperando a rede (buffer de envio cheio).
    stalled_ms: Arc<AtomicU64>,
    name: String,
    id: String,
    /// Controle remoto pedido / liberado (bits CTL_PAD | CTL_KBM).
    wants: Arc<AtomicU8>,
    granted: Arc<AtomicU8>,
}

type Viewers = Arc<Mutex<HashMap<SocketAddr, Viewer>>>;

fn ffmpeg_args(o: &HostOpts, cap: &Capture, enc: Enc, cpu_copy: bool, bitrate: u32, test: bool) -> Vec<String> {
    let br = format!("{bitrate}M");
    let buf = format!("{}k", bitrate * 1000 / 2);
    let mut graph = if let Some(h) = cap.hwnd {
        // Captura nativa da janela: segue a janela, funciona coberta, e escala se ela mudar de tamanho.
        format!(
            "gfxcapture=hwnd={h}:max_framerate={}:resize_mode=scale_aspect:width=-2:height=-2:capture_cursor=1",
            o.fps
        )
    } else {
        let mut g = format!("ddagrab=output_idx={}:framerate={}:draw_mouse=1", cap.output_idx, o.fps);
        if let Some((x, y, w, h)) = cap.crop {
            g.push_str(&format!(":offset_x={x}:offset_y={y}:video_size={w}x{h}"));
        }
        g
    };
    if let (None, Some(hm)) = (cap.hwnd, cap.hmonitor) {
        // Monitor inteiro pelo Windows Graphics Capture (compatível com mais jogos em tela cheia).
        graph = format!("gfxcapture=hmonitor={hm}:max_framerate={}:capture_cursor=1:width=-2:height=-2", o.fps);
    }
    if cpu_copy {
        graph.push_str(if enc == Enc::Qsv { ",hwdownload,format=bgra,format=nv12" } else { ",hwdownload,format=bgra" });
    }
    let mut a: Vec<String> = ["-hide_banner", "-loglevel", "error", "-nostdin", "-filter_complex", &graph]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let enc_args: &[&str] = match enc {
        Enc::Amf => &[
            "-c:v", "hevc_amf", "-usage", "ultralowlatency", "-latency", "1", "-async_depth", "1",
            "-rc", "cbr", "-header_insertion_mode", "idr", "-forced_idr", "1",
        ],
        Enc::Nvenc => &[
            "-c:v", "hevc_nvenc", "-preset", "p1", "-tune", "ull", "-rc", "cbr", "-zerolatency", "1",
            "-delay", "0", "-bf", "0", "-forced-idr", "1", "-bsf:v", "dump_extra=freq=keyframe",
        ],
        Enc::Qsv => &[
            "-c:v", "hevc_qsv", "-preset", "veryfast", "-async_depth", "1", "-look_ahead", "0",
            "-forced_idr", "1", "-bsf:v", "dump_extra=freq=keyframe",
        ],
        Enc::X265 => &[
            "-c:v", "libx265", "-preset", "ultrafast", "-tune", "zerolatency",
            "-x265-params", "repeat-headers=1:bframes=0", "-pix_fmt", "yuv420p",
        ],
    };
    a.extend(enc_args.iter().map(|s| s.to_string()));
    let gop = o.fps.to_string(); // 1 keyframe por segundo: viewer novo / perda de pacote se recupera rápido
    a.extend(["-b:v", &br, "-maxrate", &br, "-bufsize", &buf, "-g", &gop].iter().map(|s| s.to_string()));
    // Keyframe também por tempo: a captura de janela só gera frames quando a janela muda,
    // e quem entra no meio precisa de um keyframe para começar a ver.
    a.extend(["-force_key_frames", "expr:gte(t,n_forced*1)"].iter().map(|s| s.to_string()));
    if test {
        a.extend(["-frames:v", "3", "-f", "null", "-"].iter().map(|s| s.to_string()));
    } else {
        a.extend(["-f", "hevc", "-"].iter().map(|s| s.to_string()));
    }
    a
}

/// Testa cada combinação de encoder e escolhe a primeira que funciona nesta máquina.
async fn detect_encoder(o: &HostOpts, cap: &Capture) -> Result<(Enc, bool)> {
    let candidates: Vec<(Enc, bool)> = match o.encoder.as_str() {
        "amf" => vec![(Enc::Amf, false), (Enc::Amf, true)],
        "nvenc" => vec![(Enc::Nvenc, false), (Enc::Nvenc, true)],
        "qsv" => vec![(Enc::Qsv, true)],
        "cpu" => vec![(Enc::X265, true)],
        _ => vec![
            (Enc::Amf, false),
            (Enc::Nvenc, false),
            (Enc::Amf, true),
            (Enc::Nvenc, true),
            (Enc::Qsv, true),
            (Enc::X265, true),
        ],
    };
    for (enc, cpu_copy) in candidates {
        let fut = Command::new(&o.ffmpeg)
            .args(ffmpeg_args(o, cap, enc, cpu_copy, 10, true))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .kill_on_drop(true)
            .status();
        if let Ok(Ok(st)) = tokio::time::timeout(Duration::from_secs(10), fut).await {
            if st.success() {
                return Ok((enc, cpu_copy));
            }
        }
    }
    bail!(crate::i18n::t("nenhum encoder HEVC disponível. Verifique se o ffmpeg.exe está na pasta do GameStream."))
}

/// Transforma a escolha de áudio da interface numa fonte concreta (e uma descrição para o log).
fn resolve_audio(o: &HostOpts) -> (audio::CaptureSource, String) {
    resolve_audio_sel(&o.audio_device, &o.source)
}

pub fn resolve_audio_sel(sel: &audio::AudioSel, source: &Source) -> (audio::CaptureSource, String) {
    use audio::{AudioSel, CaptureSource};
    let except_discord = || match crate::procaudio::discord_root_pid() {
        Some(pid) => (CaptureSource::ProcessExcept(pid), crate::i18n::t("todo o áudio do sistema, exceto o Discord").to_string()),
        // Sem Discord aberto: tudo menos o próprio GameStream (não reenvia o que você está assistindo).
        None => (CaptureSource::ProcessExcept(std::process::id()), crate::i18n::t("todo o áudio do sistema (Discord não está em execução)").to_string()),
    };
    match sel {
        AudioSel::GameWindow => match source {
            Source::Window { hwnd, title } => match sources::window_pid(*hwnd) {
                Some(pid) => (CaptureSource::ProcessOnly(pid), crate::tr!("somente o áudio de \"{title}\"", "only the audio of \"{title}\"")),
                None => except_discord(),
            },
            Source::Monitor(_) => except_discord(),
        },
        AudioSel::AllExceptDiscord => except_discord(),
        AudioSel::App { pid, name } => {
            let pid = crate::procaudio::pid_by_exe(name).unwrap_or(*pid);
            (CaptureSource::ProcessOnly(pid), crate::tr!("somente o áudio de {name}", "only the audio of {name}"))
        }
        AudioSel::DefaultOutput => (CaptureSource::Output(None), crate::i18n::t("todo o áudio do sistema (inclui o Discord)").into()),
        AudioSel::Output(n) => (CaptureSource::Output(Some(n.clone())), crate::tr!("dispositivo de saída {n}", "output device {n}")),
        AudioSel::Input(n) => (CaptureSource::Input(n.clone()), crate::tr!("dispositivo de entrada {n}", "input device {n}")),
    }
}

/// Captura para uma fonte: janela com captura nativa (gfxcapture) quando disponível; senão, recorte.
fn capture_for(src: &Source, gfx: bool) -> Result<Capture> {
    match src {
        Source::Window { hwnd, .. } if gfx => {
            sources::resolve(src)?; // confere se a janela existe e não está minimizada
            Ok(Capture { output_idx: 0, crop: None, hwnd: Some(*hwnd), hmonitor: None })
        }
        _ => sources::resolve(src),
    }
}

fn source_desc(src: &Source) -> String {
    match src {
        Source::Monitor(i) => crate::tr!("monitor {}", "monitor {}", i + 1),
        Source::Window { title, .. } => crate::tr!("janela \"{title}\"", "window \"{title}\""),
    }
}

/// Troca a fonte (monitor ou janela) da transmissão em andamento, sem desconectar ninguém.
static SWITCH: std::sync::LazyLock<Mutex<Option<tokio::sync::mpsc::UnboundedSender<Source>>>> =
    std::sync::LazyLock::new(|| Mutex::new(None));

pub fn switch_source(src: Source) -> Result<()> {
    SWITCH
        .lock()
        .unwrap()
        .as_ref()
        .context(crate::i18n::t("nenhuma transmissão ativa"))?
        .send(src)
        .map_err(|_| anyhow::anyhow!("{}", crate::i18n::t("nenhuma transmissão ativa")))
}

/// O FFmpeg tem o filtro `gfxcapture` (Windows Graphics Capture)? Existe a partir do FFmpeg 8.
async fn has_gfxcapture(ffmpeg: &str) -> bool {
    let out = Command::new(ffmpeg)
        .args(["-hide_banner", "-h", "filter=gfxcapture"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .await;
    out.is_ok_and(|o| String::from_utf8_lossy(&o.stdout).contains("gfxcapture AVOptions"))
}

/// Acha um VPS HEVC (NAL tipo 32) — o encoder sempre manda VPS/SPS/PPS antes de cada keyframe.
fn find_vps(b: &[u8]) -> Option<usize> {
    let mut i = 0;
    while i + 3 < b.len() {
        if b[i] == 0 && b[i + 1] == 0 && b[i + 2] == 1 && (b[i + 3] >> 1) & 0x3f == 32 {
            return Some(if i > 0 && b[i - 1] == 0 { i - 1 } else { i });
        }
        i += 1;
    }
    None
}

/// Acompanha a janela transmitida: se ela mudar de lugar, tamanho ou monitor, publica o novo recorte
/// (só depois que parar de mexer, para não reiniciar o encoder a cada pixel do arraste).
async fn track_window(source: &Mutex<Source>, cap_tx: &watch::Sender<Capture>) {
    let mut pending: Option<Capture> = None;
    let mut warned = false;
    loop {
        tokio::time::sleep(Duration::from_millis(250)).await;
        let src = source.lock().unwrap().clone();
        let Source::Window { title, .. } = &src else {
            pending = None;
            continue;
        };
        let current = cap_tx.borrow().clone();
        if current.hwnd.is_some() {
            // A captura nativa já acompanha a janela sozinha.
            pending = None;
            continue;
        }
        match sources::resolve(&src) {
            Ok(c) if c != current => {
                warned = false;
                if pending.as_ref() == Some(&c) {
                    // Parou de mexer: aplica.
                    log(crate::tr!("[janela] \"{title}\" foi movida ou redimensionada; captura ajustada.", "[window] \"{title}\" was moved or resized; capture adjusted."));
                    let _ = cap_tx.send(c);
                    pending = None;
                } else {
                    pending = Some(c);
                }
            }
            Ok(_) => pending = None,
            Err(e) => {
                // Minimizada/fechada: mantém o último recorte e avisa uma vez.
                if !warned {
                    log(crate::tr!("[janela] {e:#}", "[window] {e:#}"));
                    warned = true;
                }
            }
        }
    }
}

/// Mantém o FFmpeg rodando; reinicia quando o bitrate (modo automático) ou o recorte da janela mudarem.
#[allow(clippy::too_many_arguments)]
async fn encoder_loop(
    o: &HostOpts,
    mut cap_rx: watch::Receiver<Capture>,
    enc: Enc,
    cpu_copy: bool,
    mut br_rx: watch::Receiver<u32>,
    vtx: broadcast::Sender<VideoChunk>,
    bytes_out: Arc<AtomicU64>,
    gfx: bool,
) -> Result<()> {
    // Reinícios seguidos por falha de captura (o contador zera quando o encoder fica estável).
    let mut failures: u32 = 0;
    // Captura alternativa (Windows Graphics Capture) adotada depois de falhas do ddagrab.
    let mut fallback: Option<Capture> = None;
    let mut tip_shown = false;
    loop {
        let br = *br_rx.borrow_and_update();
        let cap = fallback.clone().unwrap_or_else(|| cap_rx.borrow_and_update().clone());
        let started = Instant::now();
        let mut child = Command::new(&o.ffmpeg)
            .args(ffmpeg_args(o, &cap, enc, cpu_copy, br, false))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .creation_flags(CREATE_NO_WINDOW)
            .kill_on_drop(true)
            .spawn()
            .context(crate::i18n::t("não foi possível iniciar o ffmpeg.exe"))?;
        let mut stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(l)) = lines.next_line().await {
                log(format!("[ffmpeg] {l}"));
            }
        });
        // Termina (Ok) quando o FFmpeg para de enviar vídeo.
        let read = async {
            let mut buf = vec![0u8; 1 << 20];
            loop {
                let n = stdout.read(&mut buf).await?;
                if n == 0 {
                    return anyhow::Ok(());
                }
                bytes_out.fetch_add(n as u64, Ordering::Relaxed);
                let raw = Bytes::copy_from_slice(&buf[..n]);
                let _ = vtx.send(VideoChunk { vps: find_vps(&raw), framed: Bytes::from(net::frame(&raw)), raw, at: Instant::now() });
            }
        };
        tokio::select! {
            r = read => {
                r?;
                let _ = child.kill().await;
                // O Windows interrompe a captura quando um jogo entra/sai de tela cheia exclusiva ou muda a
                // resolução (DXGI_ERROR_ACCESS_LOST). Reinicia o encoder sem derrubar os espectadores; eles
                // retomam no próximo keyframe. Nunca desiste: a transmissão só termina quando o usuário parar.
                if started.elapsed() > Duration::from_secs(30) {
                    failures = 0;
                }
                failures += 1;
                log(crate::tr!(
                    "[encoder] captura interrompida (o jogo pode ter mudado de modo de tela); reiniciando (tentativa {})...",
                    "[encoder] capture interrupted (the game may have changed display mode); restarting (attempt {})...",
                    failures
                ));
                // Com alguns jogos em tela cheia exclusiva (e overlays como o RivaTuner) o Desktop Duplication
                // (ddagrab) falha sempre; troca a captura do monitor para o Windows Graphics Capture.
                // Se o Windows Graphics Capture também falhar seguidamente, alterna de volta (e assim por diante).
                let whole_monitor = cap.hwnd.is_none() && cap.crop.is_none();
                if gfx && whole_monitor && failures >= 2 && (failures - 2) % 3 == 0 {
                    if fallback.is_some() {
                        fallback = None;
                        log(crate::i18n::t("[encoder] Captura do monitor voltou para o Desktop Duplication."));
                    } else if let Some(hm) = sources::hmonitor_for(cap.output_idx) {
                        fallback = Some(Capture { hmonitor: Some(hm), ..cap.clone() });
                        log(crate::i18n::t("[encoder] Captura do monitor trocada para Windows Graphics Capture (compatível com mais jogos)."));
                    }
                }
                if failures == 3 && !tip_shown {
                    tip_shown = true;
                    log(crate::i18n::t("Dica: se a imagem continuar falhando, coloque o jogo em \"Tela cheia em janela\" (sem bordas) ou transmita a \"Janela específica\" do jogo."));
                }
                let wait = Duration::from_millis((500 * failures as u64).min(5000));
                tokio::select! {
                    _ = tokio::time::sleep(wait) => {}
                    _ = cap_rx.changed() => { fallback = None; failures = 0; }
                }
            }
            _ = br_rx.changed() => {
                let _ = child.kill().await;
            }
            _ = cap_rx.changed() => {
                let _ = child.kill().await;
                // Fonte nova escolhida pelo usuário: volta ao método de captura normal.
                fallback = None;
                failures = 0;
            }
        }
    }
}

/// Controle de bitrate AIMD: cai 30% quando algum viewer sofre (perda > 2% ou atraso),
/// sobe 15% depois de 20 s sem problemas, até o teto.
async fn bitrate_controller(start: u32, ceiling: u32, br_tx: watch::Sender<u32>, viewers: Viewers) {
    let mut cur = start;
    let mut last_change = Instant::now();
    let mut clean_since = Instant::now();
    // Bitrate em que a conexão não deu conta da última vez: chegar perto dele de novo exige mais tempo estável.
    let mut trouble_at: Option<u32> = None;
    // (pacotes perdidos, enviados, descartes, ms travado) da medição anterior, por espectador.
    let mut prev: HashMap<SocketAddr, (u64, u64, u64, u64)> = HashMap::new();
    loop {
        tokio::time::sleep(Duration::from_secs(2)).await;
        let mut bad: Option<String> = None;
        let n_viewers;
        {
            let v = viewers.lock().unwrap();
            n_viewers = v.len();
            prev.retain(|a, _| v.contains_key(a));
            for (addr, vw) in v.iter() {
                let s = vw.conn.stats();
                let now = (
                    s.path.lost_packets,
                    s.path.sent_packets,
                    vw.lagged.load(Ordering::Relaxed),
                    vw.stalled_ms.load(Ordering::Relaxed),
                );
                let (pl, ps, plg, pst) = prev.get(addr).copied().unwrap_or(now);
                let (dl, ds) = (now.0 - pl, now.1 - ps);
                // Só reduz por congestionamento de verdade: o espectador ficou para trás (quadros
                // descartados ou envio travado por mais de 15% do tempo) ou a perda é alta. Uma perda
                // pequena e constante não melhora baixando o bitrate: os pacotes são reenviados e o BBR
                // mantém a taxa.
                if now.2 > plg {
                    bad = Some(crate::tr!("{addr} com atraso no recebimento", "{addr} is lagging behind"));
                } else if now.3 - pst > 300 {
                    bad = Some(crate::tr!("{addr} não está recebendo na velocidade enviada", "{addr} cannot keep up with the sending rate"));
                } else if ds > 200 && dl * 100 > ds * 10 {
                    bad = Some(crate::tr!("{addr} com {:.1}% de perda de pacotes", "{addr} has {:.1}% packet loss", dl as f64 * 100.0 / ds as f64));
                }
                prev.insert(*addr, now);
            }
        }
        if let Some(why) = bad {
            clean_since = Instant::now();
            if last_change.elapsed() >= Duration::from_secs(4) && cur > MIN_BITRATE {
                trouble_at = Some(cur);
                cur = (cur * 7 / 10).max(MIN_BITRATE);
                log(crate::tr!("[auto] {why}: bitrate reduzido para {cur} Mbps", "[auto] {why}: bitrate lowered to {cur} Mbps"));
                let _ = br_tx.send(cur);
                last_change = Instant::now();
            }
        } else if n_viewers > 0 && cur < ceiling && {
            let next = (cur * 115 / 100).max(cur + 1).min(ceiling);
            // Longe do ponto onde houve problema: sobe a cada 10 s estáveis; perto dele, só após 60 s.
            let wait = if trouble_at.is_some_and(|t| next * 100 > t * 85) { 60 } else { 10 };
            clean_since.elapsed() >= Duration::from_secs(wait) && last_change.elapsed() >= Duration::from_secs(10)
        } {
            cur = (cur * 115 / 100).max(cur + 1).min(ceiling);
            if trouble_at.is_some_and(|t| cur >= t) {
                trouble_at = None; // passou do ponto antigo sem problemas: a conexão melhorou
            }
            log(crate::tr!("[auto] conexão estável: bitrate aumentado para {cur} Mbps", "[auto] stable connection: bitrate raised to {cur} Mbps"));
            let _ = br_tx.send(cur);
            last_change = Instant::now();
            clean_since = Instant::now();
        }
        if state::host(|h| std::mem::replace(&mut h.bitrate, cur)) != cur {
            for vw in viewers.lock().unwrap().values() {
                vw.conn.set_send_window(net::send_window_for(cur));
            }
        }
    }
}

pub async fn run(o: HostOpts, stop: impl Future<Output = ()>) -> Result<()> {
    state::host(|h| *h = state::HostStatus::default());
    let r = run_inner(o, stop).await;
    let error = r.as_ref().err().map(|e| format!("{e:#}"));
    if let Some(e) = &error {
        log(crate::tr!("Erro: {e}", "Error: {e}"));
    }
    state::host(|h| *h = state::HostStatus { error, ..Default::default() });
    log(crate::i18n::t("Transmissão encerrada."));
    r
}

async fn run_inner(mut o: HostOpts, stop: impl Future<Output = ()>) -> Result<()> {
    if o.ffmpeg == "ffmpeg" {
        // Usa o ffmpeg.exe da pasta do GameStream ou do PATH; se não houver, faz o download (uma única vez).
        o.ffmpeg = tokio::task::spawn_blocking(crate::updater::ensure_ffmpeg).await??;
    }
    let gfx = has_gfxcapture(&o.ffmpeg).await;
    let cap = capture_for(&o.source, gfx)?;
    if let Source::Window { .. } = &o.source {
        if gfx {
            log(crate::i18n::t("Captura nativa de janela ativa (Windows Graphics Capture)."));
        } else {
            log(crate::i18n::t("Esta versão do FFmpeg não oferece captura nativa de janela; usando recorte da tela. Atualize o ffmpeg.exe."));
        }
    }
    log(crate::i18n::t("Detectando encoder de vídeo..."));
    let (enc, cpu_copy) = detect_encoder(&o, &cap).await?;
    let start_br = if o.auto_bitrate { (o.bitrate_mbps * 7 / 10).max(MIN_BITRATE) } else { o.bitrate_mbps };
    let src_desc = match &o.source {
        Source::Monitor(i) => crate::tr!("monitor {}", "monitor {}", i + 1),
        Source::Window { title, .. } => crate::tr!("janela \"{title}\"", "window \"{title}\""),
    };
    log(format!(
        "Encoder: {}{} | {} | {} fps | bitrate {}",
        enc.name(),
        if cpu_copy { crate::i18n::t(" (cópia via CPU)") } else { crate::i18n::t(" (zero-copy na GPU)") },
        src_desc,
        o.fps,
        if o.auto_bitrate { crate::tr!("automático (até {} Mbps)", "automatic (up to {} Mbps)", o.bitrate_mbps) } else { format!("{} Mbps", o.bitrate_mbps) }
    ));
    state::host(|h| {
        h.encoder = enc.name().into();
        h.bitrate = start_br;
    });

    let (vtx, _) = broadcast::channel::<VideoChunk>(2048);
    let (atx, _) = broadcast::channel::<Bytes>(1024);

    let mut _audio_handle = None;
    let audio_fmt = if !o.audio {
        None
    } else {
        let (src, desc) = resolve_audio(&o);
        let mic = o.mic.clone().map(|d| (d, 1.0));
        let started = match audio::start_capture(atx.clone(), src, mic.clone(), !o.audio_pcm) {
            Ok(v) => Ok(v),
            Err(e) => {
                // Captura por processo indisponível (Windows antigo?): cai para o som do PC todo.
                log(crate::tr!("Áudio: captura por aplicativo indisponível ({e:#}); usando todo o áudio do sistema.", "Audio: per-application capture unavailable ({e:#}); using all system audio."));
                audio::start_capture(atx.clone(), audio::CaptureSource::Output(None), mic, !o.audio_pcm)
            }
        };
        match started {
            Ok((h, rate, ch)) => {
                log(crate::tr!("Áudio: {desc}{}", "Audio: {desc}{}", if o.mic.is_some() { crate::i18n::t(" + microfone") } else { "" }));
                _audio_handle = Some(h);
                Some((rate, ch))
            }
            Err(e) => {
                log(crate::tr!("Áudio desativado: {e:#}", "Audio disabled: {e:#}"));
                None
            }
        }
    };

    // Rede: identidade fixa + endpoint dual-stack (IPv4 e IPv6).
    let id = net::load_identity()?;
    let (ep, punch_sock) = net::server_endpoint(o.port, &id)?;
    let punch_sock = Arc::new(punch_sock);
    let (punch_tx, mut punch_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<SocketAddr>>();
    *PUNCH.lock().unwrap() = Some(punch_tx.clone());
    let code = net::security_code(&id.fingerprint);
    log(crate::tr!("Código de segurança deste host: {code}", "This host's security code: {code}"));
    state::host(|h| h.security_code = code);

    let mut addrs: Vec<(SocketAddr, invite::AddrKind, String)> = Vec::new();
    let mut mapping = None;
    if o.upnp {
        log(crate::i18n::t("Solicitando abertura de porta ao roteador (UPnP)..."));
        match tokio::time::timeout(Duration::from_secs(8), invite::map_port(o.port)).await {
            Ok(Ok((m, ext))) => {
                if invite::is_public_v4(ext) {
                    log(crate::tr!("UPnP: porta UDP {} aberta no roteador.", "UPnP: UDP port {} opened on the router.", o.port));
                    addrs.push((
                        SocketAddr::new(ext, o.port),
                        invite::AddrKind::PublicV4,
                        crate::i18n::t("IPv4 público (porta aberta via UPnP)").into(),
                    ));
                } else {
                    log(crate::tr!("UPnP: porta aberta, porém o roteador está atrás de CGNAT da operadora (IP externo {ext}). \
                         Conexões externas por IPv4 não são possíveis; utilize IPv6 ou uma VPN (Tailscale).", "UPnP: port opened, but the router is behind the ISP's CGNAT (external IP {ext}). External IPv4 connections are not possible; use IPv6 or a VPN (Tailscale)."
                    ));
                }
                mapping = Some(m);
            }
            Ok(Err(e)) => log(crate::tr!("UPnP indisponível: {e:#}", "UPnP unavailable: {e:#}")),
            Err(_) => log(crate::i18n::t("UPnP indisponível: o roteador não respondeu")),
        }
    }
    let mut v6 = 0;
    for (a, kind, label) in invite::local_addrs(o.port) {
        if kind == invite::AddrKind::PublicV6 {
            v6 += 1;
            if v6 > 2 {
                continue;
            }
        }
        if !addrs.iter().any(|(x, _, _)| *x == a) {
            addrs.push((a, kind, label));
        }
    }
    addrs.sort_by_key(|(_, kind, _)| *kind);
    let invite_code = invite::encode(&addrs.iter().map(|(a, _, _)| *a).collect::<Vec<_>>(), &id.fingerprint);
    let mut reach: Vec<String> = addrs.iter().map(|(a, _, label)| format!("{label}: {a}")).collect();
    if !addrs.iter().any(|(_, kind, _)| kind.is_internet()) {
        reach.push(crate::i18n::t("⚠ Nenhum endereço acessível pela internet foi encontrado; apenas conexões na rede local são possíveis. Consulte a seção \"Conexão pela internet\" do LEIA-ME.").into());
    } else if addrs.iter().any(|(_, kind, _)| *kind == invite::AddrKind::PublicV6) {
        reach.push(crate::i18n::t("Obs.: conexões IPv6 exigem que o espectador também tenha IPv6 e que o modem permita conexões de entrada.").into());
    }
    log(crate::tr!("Transmitindo na porta UDP {}. Convite:\n{invite_code}", "Streaming on UDP port {}. Invite:\n{invite_code}", o.port));
    for r in &reach {
        log(format!("  {r}"));
    }
    // Sala no servidor de encontro: código curto e conexão automática (sem código de resposta).
    let room_code = crate::room::code_from_fp(&id.fingerprint);
    let _room = match (o.room, crate::room::server()) {
        (true, Some(server)) => {
            log(crate::tr!(
                "Código da sala: {}",
                "Room code: {}",
                crate::room::display(&room_code)
            ));
            Some(crate::room::start_host(server, room_code.clone(), invite_code.clone(), crate::room::load_secret(), punch_tx.clone()))
        }
        _ => None,
    };
    state::host(|h| {
        h.invite = Some(invite_code);
        h.reach = reach;
        h.room = _room.as_ref().map(|_| crate::room::display(&room_code));
    });

    let viewers: Viewers = Arc::new(Mutex::new(HashMap::new()));
    let bytes_out = Arc::new(AtomicU64::new(0));
    let (br_tx, br_rx) = watch::channel(start_br);

    let (chat_tx, _) = broadcast::channel::<ChatMsg>(256);
    *CHAT.lock().unwrap() = Some(chat_tx.clone());
    let (roster_tx, _) = broadcast::channel::<serde_json::Value>(64);
    *ROSTER.lock().unwrap() = Some(roster_tx);
    let my_avatar = crate::avatar::load();
    *MEMBERS.lock().unwrap() = vec![MemberEntry {
        m: state::Member { id: "h".into(), name: crate::settings::nick(), host: true, ctl: 0 },
        avatar: my_avatar.clone(),
    }];
    state::host(|h| {
        h.avatars.clear();
        if let Some(a) = &my_avatar {
            h.avatars.insert("h".into(), a.clone());
        }
    });
    // Guarda o chat para a interface do transmissor.
    let chat_log = {
        let mut rx = chat_tx.subscribe();
        async move {
            loop {
                match rx.recv().await {
                    Ok(m) => state::host(|h| state::push_chat(&mut h.chat, m)),
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => std::future::pending::<()>().await,
                }
            }
        }
    };

    let (cap_tx, cap_rx) = watch::channel(cap.clone());
    let current_source = Mutex::new(o.source.clone());
    let tracker = track_window(&current_source, &cap_tx);
    // Troca de monitor/janela durante a transmissão: o encoder reinicia com a nova fonte e os
    // espectadores continuam conectados (retomam no próximo keyframe).
    let (switch_tx, mut switch_rx) = tokio::sync::mpsc::unbounded_channel::<Source>();
    *SWITCH.lock().unwrap() = Some(switch_tx);
    let switcher = async {
        while let Some(src) = switch_rx.recv().await {
            match capture_for(&src, gfx) {
                Ok(c) => {
                    log(crate::tr!("Fonte alterada para {}.", "Source changed to {}.", source_desc(&src)));
                    *current_source.lock().unwrap() = src;
                    let _ = cap_tx.send(c);
                }
                Err(e) => log(crate::tr!("Não foi possível trocar a fonte: {e:#}", "Could not change the source: {e:#}")),
            }
        }
    };
    let encoder = encoder_loop(&o, cap_rx, enc, cpu_copy, br_rx, vtx.clone(), bytes_out.clone(), gfx);
    let vtx_preview = vtx.clone();

    let controller = async {
        if o.auto_bitrate {
            bitrate_controller(start_br, o.bitrate_mbps, br_tx, viewers.clone()).await;
        } else {
            std::future::pending::<()>().await;
        }
    };

    let accept = {
        let ep = ep.clone();
        let viewers = viewers.clone();
        let password = o.password.clone();
        let (approval, opus) = (o.approval, !o.audio_pcm);
        async move {
            while let Some(inc) = ep.accept().await {
                let (vtx, atx, viewers, password) = (vtx.clone(), atx.clone(), viewers.clone(), password.clone());
                let chat_tx = chat_tx.clone();
                tokio::spawn(async move {
                    if let Err(e) = handle_viewer(inc, vtx, atx, chat_tx, audio_fmt, opus, approval, password, viewers).await {
                        log(crate::tr!("[espectador] {e:#}", "[viewer] {e:#}"));
                    }
                });
            }
        }
    };

    let stats = async {
        let mut last = 0u64;
        // Por espectador: (bytes enviados, pacotes perdidos, pacotes enviados) na medição anterior.
        let mut prev: HashMap<SocketAddr, (u64, u64, u64)> = HashMap::new();
        loop {
            tokio::time::sleep(Duration::from_secs(2)).await;
            let now = bytes_out.load(Ordering::Relaxed);
            let mbps = (now - last) as f64 * 8.0 / 2.0 / 1e6;
            last = now;
            let list: Vec<state::ViewerRow> = {
                let v = viewers.lock().unwrap();
                prev.retain(|a, _| v.contains_key(a));
                let mut rows: Vec<state::ViewerRow> = v
                    .iter()
                    .map(|(a, vw)| {
                        let s = vw.conn.stats();
                        let cur = (s.udp_tx.bytes, s.path.lost_packets, s.path.sent_packets);
                        let p = prev.insert(*a, cur).unwrap_or(cur);
                        let sent = cur.2 - p.2;
                        state::ViewerRow {
                            key: a.to_string(),
                            id: vw.id.clone(),
                            name: vw.name.clone(),
                            ip: a.ip().to_canonical().to_string(),
                            ping_ms: s.path.rtt.as_millis() as u32,
                            loss: if sent > 0 { (cur.1 - p.1) as f64 * 100.0 / sent as f64 } else { 0.0 },
                            mbps: (cur.0 - p.0) as f64 * 8.0 / 2.0 / 1e6,
                            wants: vw.wants.load(Ordering::Relaxed),
                            granted: vw.granted.load(Ordering::Relaxed),
                        }
                    })
                    .collect();
                rows.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
                rows
            };
            state::host(|h| {
                h.mbps = mbps;
                h.viewers = list;
            });
        }
    };

    let renew = async {
        loop {
            tokio::time::sleep(Duration::from_secs(25 * 60)).await;
            if let Some(m) = &mapping {
                let _ = m.renew().await;
            }
        }
    };

    let preview = preview_loop(vtx_preview);

    // Códigos de resposta (hole punching): abre o firewall deste lado para cada espectador liberado.
    let puncher = async {
        while let Some(addrs) = punch_rx.recv().await {
            let list: Vec<String> = addrs.iter().map(|a| a.ip().to_canonical().to_string()).collect();
            log(crate::tr!("[conexão] liberando espectador ({}) por 20 segundos...", "[connection] allowing viewer ({}) for 20 seconds...", list.join(", ")));
            let sock = punch_sock.clone();
            tokio::spawn(async move { net::punch(&sock, &addrs).await });
        }
    };

    let res = tokio::select! {
        r = encoder => r,
        _ = puncher => Ok(()),
        _ = switcher => Ok(()),
        _ = preview => Ok(()),
        _ = tracker => Ok(()),
        _ = accept => Ok(()),
        _ = stats => Ok(()),
        _ = chat_log => Ok(()),
        _ = controller => Ok(()),
        _ = renew => Ok(()),
        _ = stop => Ok(()),
    };
    *PUNCH.lock().unwrap() = None;
    *SWITCH.lock().unwrap() = None;
    DECISIONS.lock().unwrap().clear();
    CONNS.lock().unwrap().clear();
    BANNED.lock().unwrap().clear();
    *CHAT.lock().unwrap() = None;
    *ROSTER.lock().unwrap() = None;
    MEMBERS.lock().unwrap().clear();
    TOKENS.lock().unwrap().clear();
    REMOTE.lock().unwrap().clear();
    crate::viewer::cleanup_osd_avatars();
    ep.close(0u32.into(), b"transmissao encerrada pelo host");
    if let Some(m) = &mapping {
        m.remove().await;
    }
    res
}

/// Token aleatório para reconexão (identifica um espectador já aprovado nesta transmissão).
fn new_token() -> String {
    use std::hash::{BuildHasher, Hasher};
    let mut out = String::new();
    for i in 0..2u64 {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u64(i);
        h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0));
        out.push_str(&format!("{:016x}", h.finish()));
    }
    out
}

#[allow(clippy::too_many_arguments)]
async fn handle_viewer(
    inc: quinn::Incoming,
    vtx: broadcast::Sender<VideoChunk>,
    atx: broadcast::Sender<Bytes>,
    chat_tx: broadcast::Sender<ChatMsg>,
    audio_fmt: Option<(u32, u16)>,
    opus: bool,
    approval: bool,
    password: Option<String>,
    viewers: Viewers,
) -> Result<()> {
    // Tentativas paralelas do espectador que perderam a corrida terminam aqui, sem log.
    let Ok(conn) = inc.await else { return Ok(()) };
    let addr = conn.remote_address();
    let shown = addr.ip().to_canonical();
    if BANNED.lock().unwrap().contains(&shown) {
        conn.close(2u32.into(), b"removido pelo transmissor");
        return Ok(());
    }

    // Primeiro stream do viewer = senha (pode ser vazia). Trafega criptografada (TLS 1.3).
    let Ok(Ok(mut auth)) = tokio::time::timeout(Duration::from_secs(5), conn.accept_uni()).await else { return Ok(()) };
    let pw = auth.read_to_end(1024).await?;
    if let Some(p) = &password {
        if pw != p.as_bytes() {
            log(crate::tr!("[espectador] {shown} recusado: senha incorreta", "[viewer] {shown} rejected: incorrect password"));
            tokio::time::sleep(Duration::from_secs(1)).await; // freia tentativas de adivinhar a senha
            conn.close(1u32.into(), b"senha incorreta");
            return Ok(());
        }
    }

    // Canal de controle (nome, chat, controle remoto). Espectadores de versões antigas não o abrem.
    let mut ctl: Option<(quinn::SendStream, tokio::io::Lines<BufReader<quinn::RecvStream>>)> = None;
    let mut name = shown.to_string();
    let mut token_in: Option<String> = None;
    let mut avatar: Option<Arc<Vec<u8>>> = None;
    if let Ok(Ok((send, recv))) = tokio::time::timeout(Duration::from_secs(3), conn.accept_bi()).await {
        let mut lines = BufReader::new(recv).lines();
        if let Ok(Ok(Some(l))) = tokio::time::timeout(Duration::from_secs(3), lines.next_line()).await {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&l) {
                let n = control::clean_name(v["name"].as_str().unwrap_or(""));
                if !n.is_empty() {
                    name = n;
                }
                token_in = v["token"].as_str().map(|t| t.chars().take(64).collect());
                avatar = v["avatar"].as_str().and_then(crate::avatar::from_b64);
            }
        }
        ctl = Some((send, lines));
    }
    let mut who = if name == shown.to_string() { name.clone() } else { format!("{name} ({shown})") };
    if avatar.is_some() {
        who.push_str(crate::i18n::t(" [com foto]"));
    }

    // Quem já foi aprovado nesta transmissão e caiu volta direto (sem sala de espera).
    let returning = token_in.as_ref().and_then(|t| TOKENS.lock().unwrap().get(t).copied());
    let token = match (&token_in, returning) {
        (Some(t), Some(_)) => t.clone(),
        _ => new_token(),
    };

    // Sala de espera: aguarda o transmissor aprovar (até 2 minutos).
    if approval && returning.is_none() {
        let id = NEXT_PENDING.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = tokio::sync::oneshot::channel();
        DECISIONS.lock().unwrap().insert(id, tx);
        state::host(|h| h.pending.push((id, who.clone())));
        log(crate::tr!("[espectador] {who} aguardando aprovação", "[viewer] {who} waiting for approval"));
        let approved = tokio::select! {
            r = tokio::time::timeout(Duration::from_secs(120), rx) => matches!(r, Ok(Ok(true))),
            _ = conn.closed() => false,
        };
        DECISIONS.lock().unwrap().remove(&id);
        state::host(|h| h.pending.retain(|(p, _)| *p != id));
        if !approved {
            log(crate::tr!("[espectador] {who} não aprovado", "[viewer] {who} not approved"));
            conn.close(3u32.into(), b"entrada recusada pelo transmissor");
            return Ok(());
        }
    }
    let granted_bits = returning.unwrap_or(0);
    TOKENS.lock().unwrap().insert(token.clone(), granted_bits);
    let member_id = format!("v{}", NEXT_MEMBER.fetch_add(1, Ordering::Relaxed));
    MEMBERS.lock().unwrap().push(MemberEntry {
        m: state::Member { id: member_id.clone(), name: name.clone(), host: false, ctl: granted_bits },
        avatar: avatar.clone(),
    });
    if let Some(a) = &avatar {
        state::host(|h| {
            h.avatars.insert(member_id.clone(), a.clone());
        });
        if let Some(tx) = ROSTER.lock().unwrap().as_ref() {
            let _ = tx.send(avatar_msg(&member_id, a));
        }
    }
    publish_roster();

    let lagged = Arc::new(AtomicU64::new(0));
    let stalled_ms = Arc::new(AtomicU64::new(0));
    let wants = Arc::new(AtomicU8::new(0));
    let granted = Arc::new(AtomicU8::new(granted_bits));
    let (perm_tx, mut perm_rx) = tokio::sync::mpsc::unbounded_channel::<Option<String>>();
    conn.set_send_window(net::send_window_for(state::host(|h| h.bitrate)));
    viewers.lock().unwrap().insert(
        addr,
        Viewer {
            conn: conn.clone(),
            lagged: lagged.clone(),
            stalled_ms: stalled_ms.clone(),
            name: name.clone(),
            id: member_id.clone(),
            wants: wants.clone(),
            granted: granted.clone(),
        },
    );
    CONNS.lock().unwrap().insert(addr.to_string(), conn.clone());
    REMOTE.lock().unwrap().insert(
        addr.to_string(),
        RemoteSlot { id: member_id.clone(), name: name.clone(), token: token.clone(), granted: granted.clone(), notify: perm_tx.clone() },
    );
    if returning.is_some() {
        log(crate::tr!("[espectador] + {who} reconectado", "[viewer] + {who} reconnected"));
    } else {
        log(crate::tr!("[espectador] + {who} conectado", "[viewer] + {who} connected"));
    }

    // Entrada do espectador no PC do transmissor (só com permissão). Ao sair, solta tudo.
    let injector = Arc::new(Mutex::new(remote::Injector::default()));

    let video = {
        let conn = conn.clone();
        let mut rx = vtx.subscribe();
        async move {
            let mut s = conn.open_uni().await?;
            s.write_all(&[net::STREAM_VIDEO]).await?;
            let mut waiting_keyframe = true;
            loop {
                match rx.recv().await {
                    Ok(c) => {
                        // Este espectador está mais de 1 s atrás do encoder: descarta o que está na fila e
                        // recomeça no próximo keyframe (mantém a latência baixa; o bitrate automático reage).
                        if !waiting_keyframe && c.at.elapsed() > Duration::from_secs(1) {
                            while rx.try_recv().is_ok() {}
                            lagged.fetch_add(1, Ordering::Relaxed);
                            waiting_keyframe = true;
                            continue;
                        }
                        let t0 = Instant::now();
                        if waiting_keyframe {
                            let Some(off) = c.vps else { continue };
                            waiting_keyframe = false;
                            s.write_all(&net::frame(&c.raw[off..])).await?;
                        } else {
                            s.write_all(&c.framed).await?;
                        }
                        // Escrita demorada = buffer de envio cheio = a rede não está dando conta.
                        let ms = t0.elapsed().as_millis() as u64;
                        if ms > 20 {
                            stalled_ms.fetch_add(ms, Ordering::Relaxed);
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        lagged.fetch_add(1, Ordering::Relaxed);
                        waiting_keyframe = true;
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
            anyhow::Ok(())
        }
    };

    let audio = {
        let conn = conn.clone();
        let mut rx = atx.subscribe();
        async move {
            let Some((rate, ch)) = audio_fmt else {
                std::future::pending::<()>().await;
                unreachable!()
            };
            let mut s = conn.open_uni().await?;
            let mut hdr = vec![if opus { net::STREAM_AUDIO_OPUS } else { net::STREAM_AUDIO }];
            hdr.extend_from_slice(&rate.to_le_bytes());
            hdr.extend_from_slice(&ch.to_le_bytes());
            s.write_all(&hdr).await?;
            loop {
                match rx.recv().await {
                    Ok(b) => s.write_all(&b).await?,
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
            anyhow::Ok(())
        }
    };

    // Estatísticas para o viewer: perda de pacotes medida aqui (ele não tem como saber sozinho).
    let stats = {
        let conn = conn.clone();
        async move {
            let mut s = conn.open_uni().await?;
            s.write_all(&[net::STREAM_STATS]).await?;
            let mut prev = conn.stats().path;
            loop {
                tokio::time::sleep(Duration::from_secs(1)).await;
                let now = conn.stats().path;
                let (sent, lost) = (now.sent_packets - prev.sent_packets, now.lost_packets - prev.lost_packets);
                let loss = if sent > 0 { lost as f64 * 100.0 / sent as f64 } else { 0.0 };
                let target = state::host(|h| h.bitrate);
                s.write_all(format!("loss={loss:.2};target={target}\n").as_bytes()).await?;
                prev = now;
            }
            #[allow(unreachable_code)]
            anyhow::Ok(())
        }
    };

    // Mensagens do espectador (chat, reações, pedido de controle, teclas) e para ele (chat de todos,
    // mudanças de permissão).
    let control_task = {
        let (chat_tx, name, wants, granted, injector) = (chat_tx.clone(), name.clone(), wants.clone(), granted.clone(), injector.clone());
        let member_id = member_id.clone();
        async move {
            let Some((mut send, mut lines)) = ctl else {
                std::future::pending::<()>().await;
                unreachable!()
            };
            let mut roster_rx = ROSTER.lock().unwrap().as_ref().map(|tx| tx.subscribe());
            control::send(&mut send, &serde_json::json!({ "t": "welcome", "host": crate::settings::nick(), "token": token, "you": member_id })).await?;
            let avatars: Vec<serde_json::Value> =
                MEMBERS.lock().unwrap().iter().filter_map(|e| e.avatar.as_ref().map(|a| avatar_msg(&e.m.id, a))).collect();
            for a in avatars {
                control::send(&mut send, &a).await?;
            }
            control::send(&mut send, &roster_json()).await?;
            if granted_bits != 0 {
                control::send(&mut send, &serde_json::json!({ "t": "control", "pad": granted_bits & CTL_PAD != 0, "kbm": granted_bits & CTL_KBM != 0 })).await?;
            }
            let mut chat_rx = chat_tx.subscribe();
            let mut open = true;
            // Limite de mensagens: 8 a cada 10 s.
            let mut recent: std::collections::VecDeque<Instant> = Default::default();
            loop {
                tokio::select! {
                    l = lines.next_line(), if open => {
                        let Some(l) = l? else { open = false; continue };
                        let Ok(v) = serde_json::from_str::<serde_json::Value>(&l) else { continue };
                        let g = granted.load(Ordering::Relaxed);
                        match v["t"].as_str().unwrap_or("") {
                            t @ ("chat" | "react") => {
                                while recent.front().is_some_and(|t| t.elapsed() > Duration::from_secs(10)) {
                                    recent.pop_front();
                                }
                                if recent.len() >= 8 {
                                    continue;
                                }
                                let msg = if t == "chat" {
                                    control::clean_text(v["text"].as_str().unwrap_or("")).map(|text| ChatMsg { id: member_id.clone(), from: name.clone(), text, react: false })
                                } else {
                                    control::clean_react(v["r"].as_str().unwrap_or("")).map(|text| ChatMsg { id: member_id.clone(), from: name.clone(), text, react: true })
                                };
                                if let Some(m) = msg {
                                    recent.push_back(Instant::now());
                                    let _ = chat_tx.send(m);
                                }
                            }
                            "control_req" => {
                                let bits = (if v["pad"].as_bool() == Some(true) { CTL_PAD } else { 0 })
                                    | (if v["kbm"].as_bool() == Some(true) { CTL_KBM } else { 0 });
                                wants.store(bits, Ordering::Relaxed);
                                if bits == 0 {
                                    if g != 0 {
                                        set_control(&addr.to_string(), 0);
                                    }
                                    log(crate::tr!("[controle] {} parou de pedir controle", "[control] {} withdrew the control request", name));
                                } else {
                                    // Só mantém o que continua sendo pedido.
                                    if g & !bits != 0 {
                                        set_control(&addr.to_string(), g & bits);
                                    }
                                    log(crate::tr!("[controle] {} pediu para jogar ({})", "[control] {} asked to play ({})", name, control::ctl_desc(bits)));
                                }
                            }
                            "key" if g & CTL_KBM != 0 => {
                                let (sc, ext, down) = (v["sc"].as_u64().unwrap_or(0) as u16, v["ext"].as_bool() == Some(true), v["down"].as_bool() == Some(true));
                                injector.lock().unwrap().key(sc, ext, down);
                            }
                            "mbtn" if g & CTL_KBM != 0 => {
                                injector.lock().unwrap().button(v["b"].as_u64().unwrap_or(9) as u8, v["down"].as_bool() == Some(true));
                            }
                            "wheel" if g & CTL_KBM != 0 => {
                                injector.lock().unwrap().wheel(v["d"].as_i64().unwrap_or(0) as i32, v["h"].as_bool() == Some(true));
                            }
                            _ => {}
                        }
                    }
                    m = chat_rx.recv() => match m {
                        Ok(m) => control::send(&mut send, &m.to_json()).await?,
                        Err(broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(broadcast::error::RecvError::Closed) => std::future::pending::<()>().await,
                    },
                    r = async { match roster_rx.as_mut() { Some(rx) => rx.recv().await, None => std::future::pending().await } } => match r {
                        Ok(v) => control::send(&mut send, &v).await?,
                        Err(broadcast::error::RecvError::Lagged(_)) => control::send(&mut send, &roster_json()).await?,
                        Err(broadcast::error::RecvError::Closed) => roster_rx = None,
                    },
                    n = perm_rx.recv() => {
                        let g = granted.load(Ordering::Relaxed);
                        {
                            let mut inj = injector.lock().unwrap();
                            if g & CTL_KBM == 0 { inj.release_kbm(); }
                            if g & CTL_PAD == 0 { inj.release_pad(); }
                        }
                        control::send(&mut send, &serde_json::json!({ "t": "control", "pad": g & CTL_PAD != 0, "kbm": g & CTL_KBM != 0 })).await?;
                        if let Some(Some(text)) = n {
                            control::send(&mut send, &serde_json::json!({ "t": "notice", "text": text })).await?;
                        }
                    }
                }
            }
            #[allow(unreachable_code)]
            anyhow::Ok(())
        }
    };

    // Controle (gamepad) e movimento do mouse: datagramas (o mais recente vale).
    let datagrams = {
        let (conn, granted, injector, name, perm_tx) = (conn.clone(), granted.clone(), injector.clone(), name.clone(), perm_tx.clone());
        async move {
            loop {
                let d = conn.read_datagram().await?;
                let g = granted.load(Ordering::Relaxed);
                if let (true, Some(p)) = (g & CTL_PAD != 0, control::Pad::decode(&d)) {
                    let r = injector.lock().unwrap().pad(&p);
                    if let Err(e) = r {
                        // Sem o driver do controle virtual: revoga só o controle e avisa os dois lados.
                        log(crate::tr!("[controle] controle de {} indisponível: {e:#}", "[control] controller for {} unavailable: {e:#}", name));
                        granted.fetch_and(!CTL_PAD, Ordering::Relaxed);
                        let _ = perm_tx.send(Some(crate::i18n::t("o transmissor não tem o driver de controle virtual (ViGEmBus) instalado; use teclado e mouse ou peça para ele instalar").into()));
                    }
                } else if let (true, Some((dx, dy))) = (g & CTL_KBM != 0, control::decode_mouse(&d)) {
                    injector.lock().unwrap().mouse_move(dx, dy);
                }
            }
            #[allow(unreachable_code)]
            anyhow::Ok(())
        }
    };

    let res: Result<()> = tokio::select! {
        r = video => r,
        r = audio => r,
        r = stats => r,
        r = control_task => r,
        r = datagrams => r,
        e = conn.closed() => Err(e.into()),
    };
    viewers.lock().unwrap().remove(&addr);
    CONNS.lock().unwrap().remove(&addr.to_string());
    REMOTE.lock().unwrap().remove(&addr.to_string());
    MEMBERS.lock().unwrap().retain(|e| e.m.id != member_id);
    state::host(|h| {
        h.avatars.remove(&member_id);
    });
    publish_roster();
    {
        let mut inj = injector.lock().unwrap();
        inj.release_kbm();
        inj.release_pad();
    }
    match res {
        Err(e) => log(crate::tr!("[espectador] - {who} desconectado ({e})", "[viewer] - {who} disconnected ({e})")),
        Ok(()) => log(crate::tr!("[espectador] - {who} desconectado", "[viewer] - {who} disconnected")),
    }
    Ok(())
}
