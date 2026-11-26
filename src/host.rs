//! Host: captura a tela (DXGI, na GPU) + encoda HEVC no encoder de hardware via FFmpeg,
//! captura o áudio do sistema e distribui para todos os viewers via QUIC.
//! Bitrate automático: reduz quando algum viewer perde pacotes/fica para trás, sobe aos poucos quando está tudo limpo.

use std::{
    collections::HashMap,
    future::Future,
    net::SocketAddr,
    process::Stdio,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
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
    audio, invite, net,
    sources::{self, Capture, Source},
    state::{self, log},
};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
pub const MIN_BITRATE: u32 = 3;

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
            log(format!("[prévia] {e:#}"));
            set_preview(false);
        }
        state::host(|h| h.preview_open = false);
    }
}

/// Mostra o vídeo que sai do encoder (o mesmo que os amigos recebem) num mpv local, sem áudio.
async fn run_preview(vtx: &broadcast::Sender<VideoChunk>) -> Result<()> {
    let mpv = crate::viewer::find_mpv(None);
    let mut child = Command::new(&mpv)
        .args(crate::viewer::mpv_args("preview", "GameStream - sua transmissão (prévia, sem som)", None))
        .stdin(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .with_context(|| format!("não consegui abrir o mpv ({})", mpv.display()))?;
    let mut stdin = child.stdin.take().unwrap();
    let mut rx = vtx.subscribe();
    let mut waiting_keyframe = true;
    state::host(|h| h.preview_open = true);
    log("[prévia] aberta (sem som).");
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
    log("[prévia] fechada.");
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
    pub password: Option<String>,
    pub upnp: bool,
    pub ffmpeg: String,
}

#[derive(Clone)]
struct VideoChunk {
    raw: Bytes,
    framed: Bytes,
    /// Posição de um VPS (início de keyframe) dentro do chunk, se houver.
    vps: Option<usize>,
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
            Enc::X265 => "CPU (libx265 — lento!)",
        }
    }
}

struct Viewer {
    conn: quinn::Connection,
    lagged: Arc<AtomicU64>,
}

type Viewers = Arc<Mutex<HashMap<SocketAddr, Viewer>>>;

fn ffmpeg_args(o: &HostOpts, cap: &Capture, enc: Enc, cpu_copy: bool, bitrate: u32, test: bool) -> Vec<String> {
    let br = format!("{bitrate}M");
    let buf = format!("{}k", bitrate * 1000 / 2);
    let mut graph = format!("ddagrab=output_idx={}:framerate={}:draw_mouse=1", cap.output_idx, o.fps);
    if let Some((x, y, w, h)) = cap.crop {
        graph.push_str(&format!(":offset_x={x}:offset_y={y}:video_size={w}x{h}"));
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
            "-rc", "cbr", "-header_insertion_mode", "idr",
        ],
        Enc::Nvenc => &[
            "-c:v", "hevc_nvenc", "-preset", "p1", "-tune", "ull", "-rc", "cbr", "-zerolatency", "1",
            "-delay", "0", "-bf", "0", "-bsf:v", "dump_extra=freq=keyframe",
        ],
        Enc::Qsv => &[
            "-c:v", "hevc_qsv", "-preset", "veryfast", "-async_depth", "1", "-look_ahead", "0",
            "-bsf:v", "dump_extra=freq=keyframe",
        ],
        Enc::X265 => &[
            "-c:v", "libx265", "-preset", "ultrafast", "-tune", "zerolatency",
            "-x265-params", "repeat-headers=1:bframes=0", "-pix_fmt", "yuv420p",
        ],
    };
    a.extend(enc_args.iter().map(|s| s.to_string()));
    let gop = o.fps.to_string(); // 1 keyframe por segundo: viewer novo / perda de pacote se recupera rápido
    a.extend(["-b:v", &br, "-maxrate", &br, "-bufsize", &buf, "-g", &gop].iter().map(|s| s.to_string()));
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
    bail!("nenhum encoder HEVC funcionou. O ffmpeg.exe está na pasta do GameStream?")
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

/// Mantém o FFmpeg rodando; reinicia com o novo bitrate quando o controle automático mudar.
async fn encoder_loop(
    o: &HostOpts,
    cap: &Capture,
    enc: Enc,
    cpu_copy: bool,
    mut br_rx: watch::Receiver<u32>,
    vtx: broadcast::Sender<VideoChunk>,
    bytes_out: Arc<AtomicU64>,
) -> Result<()> {
    loop {
        let br = *br_rx.borrow_and_update();
        let mut child = Command::new(&o.ffmpeg)
            .args(ffmpeg_args(o, cap, enc, cpu_copy, br, false))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .creation_flags(CREATE_NO_WINDOW)
            .kill_on_drop(true)
            .spawn()
            .context("não consegui iniciar o ffmpeg.exe")?;
        let mut stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(l)) = lines.next_line().await {
                log(format!("[ffmpeg] {l}"));
            }
        });
        let read = async {
            let mut buf = vec![0u8; 1 << 20];
            loop {
                let n = stdout.read(&mut buf).await?;
                if n == 0 {
                    bail!("o encoder (ffmpeg) parou. Se a fonte for uma janela, ela foi fechada/minimizada?");
                }
                bytes_out.fetch_add(n as u64, Ordering::Relaxed);
                let raw = Bytes::copy_from_slice(&buf[..n]);
                let _ = vtx.send(VideoChunk { vps: find_vps(&raw), framed: Bytes::from(net::frame(&raw)), raw });
            }
        };
        tokio::select! {
            r = read => return r,
            _ = br_rx.changed() => {
                let _ = child.kill().await;
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
    let mut prev: HashMap<SocketAddr, (u64, u64, u64)> = HashMap::new();
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
                let now = (s.path.lost_packets, s.path.sent_packets, vw.lagged.load(Ordering::Relaxed));
                let (pl, ps, plg) = prev.get(addr).copied().unwrap_or(now);
                let (dl, ds) = (now.0 - pl, now.1 - ps);
                if now.2 > plg {
                    bad = Some(format!("{addr} ficou para trás"));
                } else if ds > 200 && dl * 100 > ds * 2 {
                    bad = Some(format!("{addr} perdendo {:.1}% dos pacotes", dl as f64 * 100.0 / ds as f64));
                }
                prev.insert(*addr, now);
            }
        }
        if let Some(why) = bad {
            clean_since = Instant::now();
            if last_change.elapsed() >= Duration::from_secs(4) && cur > MIN_BITRATE {
                cur = (cur * 7 / 10).max(MIN_BITRATE);
                log(format!("[auto] {why} → bitrate reduzido para {cur} Mbps"));
                let _ = br_tx.send(cur);
                last_change = Instant::now();
            }
        } else if n_viewers > 0
            && cur < ceiling
            && clean_since.elapsed() >= Duration::from_secs(20)
            && last_change.elapsed() >= Duration::from_secs(20)
        {
            cur = (cur * 115 / 100).max(cur + 1).min(ceiling);
            log(format!("[auto] conexão estável → bitrate aumentado para {cur} Mbps"));
            let _ = br_tx.send(cur);
            last_change = Instant::now();
            clean_since = Instant::now();
        }
        state::host(|h| h.bitrate = cur);
    }
}

pub async fn run(o: HostOpts, stop: impl Future<Output = ()>) -> Result<()> {
    state::host(|h| *h = state::HostStatus::default());
    let r = run_inner(o, stop).await;
    let error = r.as_ref().err().map(|e| format!("{e:#}"));
    if let Some(e) = &error {
        log(format!("Erro: {e}"));
    }
    state::host(|h| *h = state::HostStatus { error, ..Default::default() });
    log("Transmissão encerrada.");
    r
}

async fn run_inner(mut o: HostOpts, stop: impl Future<Output = ()>) -> Result<()> {
    if o.ffmpeg == "ffmpeg" {
        if let Ok(exe) = std::env::current_exe() {
            let local = exe.with_file_name("ffmpeg.exe");
            if local.exists() {
                o.ffmpeg = local.to_string_lossy().into_owned();
            }
        }
    }
    let cap = sources::resolve(&o.source)?;
    log("Detectando encoder de vídeo...");
    let (enc, cpu_copy) = detect_encoder(&o, &cap).await?;
    let start_br = if o.auto_bitrate { (o.bitrate_mbps * 7 / 10).max(MIN_BITRATE) } else { o.bitrate_mbps };
    let src_desc = match &o.source {
        Source::Monitor(i) => format!("monitor {}", i + 1),
        Source::Window { title, .. } => format!("janela \"{title}\""),
    };
    log(format!(
        "Encoder: {}{} | {} | {} fps | bitrate {}",
        enc.name(),
        if cpu_copy { " (cópia via CPU)" } else { " (zero-copy na GPU)" },
        src_desc,
        o.fps,
        if o.auto_bitrate { format!("automático (até {} Mbps)", o.bitrate_mbps) } else { format!("{} Mbps", o.bitrate_mbps) }
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
        match audio::start_capture(atx.clone(), o.audio_device.clone()) {
            Ok((h, rate, ch)) => {
                log(format!("Áudio: {} ({rate} Hz, {ch} canais)", o.audio_device.label()));
                _audio_handle = Some(h);
                Some((rate, ch))
            }
            Err(e) => {
                log(format!("Áudio desativado: {e:#}"));
                None
            }
        }
    };

    // Rede: identidade fixa + endpoint dual-stack (IPv4 e IPv6).
    let id = net::load_identity()?;
    let ep = net::server_endpoint(o.port, &id)?;
    let code = net::security_code(&id.fingerprint);
    log(format!("Código de segurança deste host: {code}"));
    state::host(|h| h.security_code = code);

    let mut addrs: Vec<(SocketAddr, String)> = Vec::new();
    let mut mapping = None;
    if o.upnp {
        log("Pedindo ao roteador para abrir a porta (UPnP)...");
        match tokio::time::timeout(Duration::from_secs(8), invite::map_port(o.port)).await {
            Ok(Ok((m, ext))) => {
                if invite::is_public_v4(ext) {
                    log(format!("UPnP: porta UDP {} aberta no roteador.", o.port));
                    addrs.push((SocketAddr::new(ext, o.port), "IPv4 público (porta aberta via UPnP)".into()));
                } else {
                    log(format!(
                        "UPnP abriu a porta, mas o roteador está atrás de CGNAT da operadora (IP externo {ext}): \
                         amigos de fora não chegam por IPv4. Use IPv6 ou Tailscale."
                    ));
                }
                mapping = Some(m);
            }
            Ok(Err(e)) => log(format!("UPnP indisponível: {e:#}")),
            Err(_) => log("UPnP indisponível: o roteador não respondeu"),
        }
    }
    let locals = invite::local_addrs(o.port);
    let rank = |k: &str| {
        if k.starts_with("IPv4") {
            0
        } else if k.starts_with("IPv6") {
            1
        } else if k.starts_with("VPN") {
            2
        } else {
            3
        }
    };
    let mut v6 = 0;
    for (a, k) in locals {
        if k.starts_with("IPv6") {
            v6 += 1;
            if v6 > 2 {
                continue;
            }
        }
        if !addrs.iter().any(|(x, _)| *x == a) {
            addrs.push((a, k));
        }
    }
    addrs.sort_by_key(|(_, k)| rank(k));
    let invite_code = invite::encode(&addrs.iter().map(|(a, _)| *a).collect::<Vec<_>>(), &id.fingerprint);
    let mut reach: Vec<String> = addrs.iter().map(|(a, k)| format!("{k}: {a}")).collect();
    if !addrs.iter().any(|(_, k)| k.starts_with("IPv4") || k.starts_with("IPv6") || k.starts_with("VPN")) {
        reach.push("⚠ Nenhum caminho pela internet encontrado — só a rede local funciona. Veja \"Sem acesso ao modem\" no LEIA-ME (Tailscale).".into());
    } else if addrs.iter().any(|(_, k)| k.starts_with("IPv6")) {
        reach.push("Obs.: o IPv6 só funciona se o amigo também tiver IPv6 e o modem não bloquear conexões de entrada.".into());
    }
    log(format!("Transmitindo na porta UDP {}. Convite:\n{invite_code}", o.port));
    for r in &reach {
        log(format!("  {r}"));
    }
    state::host(|h| {
        h.invite = Some(invite_code);
        h.reach = reach;
    });

    let viewers: Viewers = Arc::new(Mutex::new(HashMap::new()));
    let bytes_out = Arc::new(AtomicU64::new(0));
    let (br_tx, br_rx) = watch::channel(start_br);

    let encoder = encoder_loop(&o, &cap, enc, cpu_copy, br_rx, vtx.clone(), bytes_out.clone());
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
        async move {
            while let Some(inc) = ep.accept().await {
                let (vtx, atx, viewers, password) = (vtx.clone(), atx.clone(), viewers.clone(), password.clone());
                tokio::spawn(async move {
                    if let Err(e) = handle_viewer(inc, vtx, atx, audio_fmt, password, viewers).await {
                        log(format!("[viewer] {e:#}"));
                    }
                });
            }
        }
    };

    let stats = async {
        let mut last = 0u64;
        loop {
            tokio::time::sleep(Duration::from_secs(2)).await;
            let now = bytes_out.load(Ordering::Relaxed);
            let mbps = (now - last) as f64 * 8.0 / 2.0 / 1e6;
            last = now;
            let list: Vec<String> = viewers.lock().unwrap().iter().map(|(a, v)| {
                let rtt = v.conn.stats().path.rtt.as_millis();
                format!("{}  (ping {rtt} ms)", a.ip().to_canonical())
            }).collect();
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

    let res = tokio::select! {
        r = encoder => r,
        _ = preview => Ok(()),
        _ = accept => Ok(()),
        _ = stats => Ok(()),
        _ = controller => Ok(()),
        _ = renew => Ok(()),
        _ = stop => Ok(()),
    };
    ep.close(0u32.into(), b"host encerrou");
    if let Some(m) = &mapping {
        m.remove().await;
    }
    res
}

async fn handle_viewer(
    inc: quinn::Incoming,
    vtx: broadcast::Sender<VideoChunk>,
    atx: broadcast::Sender<Bytes>,
    audio_fmt: Option<(u32, u16)>,
    password: Option<String>,
    viewers: Viewers,
) -> Result<()> {
    let conn = inc.await?;
    let addr = conn.remote_address();
    let shown = addr.ip().to_canonical();

    // Primeiro stream do viewer = senha (pode ser vazia). Trafega criptografada (TLS 1.3).
    let mut auth = tokio::time::timeout(Duration::from_secs(5), conn.accept_uni()).await??;
    let pw = auth.read_to_end(1024).await?;
    if let Some(p) = &password {
        if pw != p.as_bytes() {
            log(format!("[viewer] {shown} recusado: senha incorreta"));
            tokio::time::sleep(Duration::from_secs(1)).await; // freia tentativas de adivinhar a senha
            conn.close(1u32.into(), b"senha incorreta");
            return Ok(());
        }
    }

    let lagged = Arc::new(AtomicU64::new(0));
    viewers.lock().unwrap().insert(addr, Viewer { conn: conn.clone(), lagged: lagged.clone() });
    log(format!("[viewer] + {shown} conectou"));

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
                        if waiting_keyframe {
                            let Some(off) = c.vps else { continue };
                            waiting_keyframe = false;
                            s.write_all(&net::frame(&c.raw[off..])).await?;
                        } else {
                            s.write_all(&c.framed).await?;
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
            let mut hdr = vec![net::STREAM_AUDIO];
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

    let res: Result<()> = tokio::select! {
        r = video => r,
        r = audio => r,
        e = conn.closed() => Err(e.into()),
    };
    viewers.lock().unwrap().remove(&addr);
    match res {
        Err(e) => log(format!("[viewer] - {shown} saiu ({e})")),
        Ok(()) => log(format!("[viewer] - {shown} saiu")),
    }
    Ok(())
}
