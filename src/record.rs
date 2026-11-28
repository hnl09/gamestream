//! Gravação da transmissão e replay instantâneo (os últimos 30 s), sem codificar de novo.
//!
//! O vídeo gravado é exatamente o que os espectadores recebem (HEVC já comprimido pela placa de
//! vídeo), então gravar não pesa mais na GPU. Cada quadro ganha o horário em que saiu do encoder
//! (a captura de janela só manda quadros quando a imagem muda) e vai para um MPEG-TS com esses
//! horários; o áudio (PCM, contínuo) vai por um pipe. O FFmpeg junta os dois num MP4 (áudio em AAC).
//! Arquivos em Vídeos\GameStream.

use std::{
    collections::VecDeque,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{LazyLock, Mutex},
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use bytes::Bytes;
use tokio::{
    io::AsyncWriteExt,
    process::Command,
    sync::{broadcast, mpsc},
};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
/// Duração do replay instantâneo.
pub const REPLAY_SECS: u64 = 30;
/// Limite de memória do replay (vídeo), para bitrates muito altos.
const REPLAY_MAX_BYTES: usize = 220 << 20;

// ---------------------------------------------------------------------------------------------
// Quadros (access units) HEVC a partir dos pedaços que saem do encoder
// ---------------------------------------------------------------------------------------------

/// Um quadro completo, com o horário em que começou a sair do encoder.
#[derive(Clone)]
pub struct Au {
    pub data: Bytes,
    pub at: Instant,
    /// Quadro-chave com VPS/SPS/PPS (dá para começar um vídeo nele).
    pub key: bool,
}

/// Junta os pedaços (que cortam quadros em qualquer lugar) e separa quadro a quadro.
#[derive(Default)]
pub struct AuSplitter {
    cur: Vec<u8>,
    pos: usize,
    at: Option<Instant>,
    seen_vcl: bool,
    key: bool,
}

impl AuSplitter {
    pub fn reset(&mut self) {
        *self = AuSplitter::default();
    }

    pub fn push(&mut self, data: &[u8], at: Instant, out: &mut Vec<Au>) {
        if self.at.is_none() {
            self.at = Some(at);
        }
        self.cur.extend_from_slice(data);
        let mut i = self.pos;
        while i + 5 < self.cur.len() {
            let c = &self.cur;
            if c[i] == 0 && c[i + 1] == 0 && c[i + 2] == 1 {
                let t = (c[i + 3] >> 1) & 0x3f;
                // Começa um quadro novo: VPS/SPS/PPS/AUD/SEI de prefixo, ou a 1ª fatia de uma imagem.
                let starts = matches!(t, 32..=35 | 39) || (t < 32 && c[i + 5] & 0x80 != 0);
                if starts && self.seen_vcl {
                    let sc = if i > 0 && c[i - 1] == 0 { i - 1 } else { i };
                    let rest = self.cur.split_off(sc);
                    let au = std::mem::replace(&mut self.cur, rest);
                    out.push(Au { data: Bytes::from(au), at: self.at.unwrap_or(at), key: self.key });
                    self.at = Some(at);
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

// ---------------------------------------------------------------------------------------------
// MPEG-TS mínimo (um programa, um stream HEVC)
// ---------------------------------------------------------------------------------------------

const PMT_PID: u16 = 0x1000;
const VID_PID: u16 = 0x100;

fn crc32_mpeg(data: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for &b in data {
        crc ^= (b as u32) << 24;
        for _ in 0..8 {
            crc = if crc & 0x8000_0000 != 0 { (crc << 1) ^ 0x04c1_1db7 } else { crc << 1 };
        }
    }
    crc
}

#[derive(Default)]
pub struct TsWriter {
    cc_pat: u8,
    cc_pmt: u8,
    cc_vid: u8,
}

impl TsWriter {
    fn section_packet(pid: u16, cc: &mut u8, section: &[u8], out: &mut Vec<u8>) {
        let mut p = vec![0x47, 0x40 | ((pid >> 8) as u8 & 0x1f), pid as u8, 0x10 | (*cc & 0x0f), 0x00];
        *cc = cc.wrapping_add(1);
        p.extend_from_slice(section);
        p.resize(188, 0xff);
        out.extend_from_slice(&p);
    }

    fn tables(&mut self, out: &mut Vec<u8>) {
        // PAT: programa 1 -> PMT.
        let mut pat = vec![0x00, 0xb0, 0x0d, 0x00, 0x01, 0xc1, 0x00, 0x00, 0x00, 0x01, 0xe0 | (PMT_PID >> 8) as u8, PMT_PID as u8];
        let crc = crc32_mpeg(&pat);
        pat.extend_from_slice(&crc.to_be_bytes());
        Self::section_packet(0, &mut self.cc_pat, &pat, out);
        // PMT: um stream HEVC (tipo 0x24), que também leva o relógio (PCR).
        let mut pmt = vec![
            0x02, 0xb0, 0x12, 0x00, 0x01, 0xc1, 0x00, 0x00,
            0xe0 | (VID_PID >> 8) as u8, VID_PID as u8,
            0xf0, 0x00,
            0x24, 0xe0 | (VID_PID >> 8) as u8, VID_PID as u8, 0xf0, 0x00,
        ];
        let crc = crc32_mpeg(&pmt);
        pmt.extend_from_slice(&crc.to_be_bytes());
        Self::section_packet(PMT_PID, &mut self.cc_pmt, &pmt, out);
    }

    /// Escreve um quadro com o horário `pts` (relógio de 90 kHz).
    pub fn write_au(&mut self, au: &[u8], pts: u64, key: bool, out: &mut Vec<u8>) {
        if key {
            self.tables(out);
        }
        let mut pes = vec![0, 0, 1, 0xe0, 0, 0, 0x80, 0x80, 5];
        pes.push(0x21 | ((pts >> 29) as u8 & 0x0e));
        pes.push((pts >> 22) as u8);
        pes.push(((pts >> 14) as u8 & 0xfe) | 1);
        pes.push((pts >> 7) as u8);
        pes.push(((pts << 1) as u8 & 0xfe) | 1);
        pes.extend_from_slice(au);
        let pcr = pts.saturating_sub(9000); // 100 ms antes da exibição
        let mut rest = &pes[..];
        let mut first = true;
        while !rest.is_empty() {
            let mut af: Vec<u8> = Vec::new();
            if first {
                let flags = 0x10 | if key { 0x40 } else { 0 };
                af = vec![7, flags, (pcr >> 25) as u8, (pcr >> 17) as u8, (pcr >> 9) as u8, (pcr >> 1) as u8, ((pcr & 1) as u8) << 7 | 0x7e, 0];
            }
            let room = 184 - af.len();
            if rest.len() < room {
                // Último pedaço: completa o pacote com enchimento no campo de adaptação.
                let need = 184 - rest.len();
                if af.is_empty() {
                    af.push((need - 1) as u8);
                    if need > 1 {
                        af.push(0);
                        af.resize(need, 0xff);
                    }
                } else {
                    af.resize(need, 0xff);
                    af[0] = (need - 1) as u8;
                }
            }
            let afc = if af.is_empty() { 0x10 } else { 0x30 };
            out.push(0x47);
            out.push(if first { 0x40 } else { 0 } | ((VID_PID >> 8) as u8 & 0x1f));
            out.push(VID_PID as u8);
            out.push(afc | (self.cc_vid & 0x0f));
            self.cc_vid = self.cc_vid.wrapping_add(1);
            out.extend_from_slice(&af);
            let n = 184 - af.len();
            out.extend_from_slice(&rest[..n]);
            rest = &rest[n..];
            first = false;
        }
    }
}

// ---------------------------------------------------------------------------------------------
// MP4 pelo FFmpeg
// ---------------------------------------------------------------------------------------------

/// Pasta dos vídeos salvos: a escolhida pela pessoa ou Vídeos\GameStream.
pub fn videos_dir() -> PathBuf {
    // Testes automatizados gravam numa pasta própria.
    if let Ok(d) = std::env::var("GAMESTREAM_VIDEOS_DIR") {
        if !d.is_empty() {
            return PathBuf::from(d);
        }
    }
    // Pasta escolhida (se ainda existir ou der para criar, ex.: um HD externo desconectado não dá).
    if let Some(d) = crate::settings::videos_dir() {
        let p = PathBuf::from(&d);
        if std::fs::create_dir_all(&p).is_ok() {
            return p;
        }
        crate::state::log(crate::tr!(
            "[gravação] a pasta escolhida ({d}) não está disponível; salvando na pasta padrão.",
            "[recording] the chosen folder ({d}) is not available; saving to the default folder."
        ));
    }
    default_videos_dir()
}

/// Vídeos\GameStream.
pub fn display_dir() -> PathBuf {
    if let Ok(d) = std::env::var("GAMESTREAM_VIDEOS_DIR") {
        if !d.is_empty() {
            return PathBuf::from(d);
        }
    }
    crate::settings::videos_dir().map(PathBuf::from).unwrap_or_else(default_videos_dir)
}

/// A pasta padrão.
pub fn default_videos_dir() -> PathBuf {
    let base = unsafe {
        use windows::Win32::UI::Shell::{FOLDERID_Videos, SHGetKnownFolderPath, KF_FLAG_DEFAULT};
        SHGetKnownFolderPath(&FOLDERID_Videos, KF_FLAG_DEFAULT, None).ok().and_then(|p| {
            let s = p.to_string().ok();
            windows::Win32::System::Com::CoTaskMemFree(Some(p.0 as _));
            s
        })
    };
    let base = base
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var("USERPROFILE").unwrap_or_else(|_| ".".into())).join("Videos"));
    base.join("GameStream")
}

fn file_name(prefix: &str) -> PathBuf {
    let now = chrono_like_now();
    let dir = videos_dir();
    let _ = std::fs::create_dir_all(&dir);
    let mut p = dir.join(format!("{prefix} {now}.mp4"));
    let mut n = 2;
    while p.exists() {
        p = dir.join(format!("{prefix} {now} ({n}).mp4"));
        n += 1;
    }
    p
}

/// "2026-11-27 21-30-05" no horário local.
fn chrono_like_now() -> String {
    let t = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
    format!("{:04}-{:02}-{:02} {:02}-{:02}-{:02}", t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond)
}

/// Junta vídeo (MPEG-TS pelo stdin) e áudio (PCM por um pipe) num MP4.
pub struct Muxer {
    child: tokio::process::Child,
    video_tx: Option<mpsc::UnboundedSender<Vec<u8>>>,
    audio_tx: Option<mpsc::UnboundedSender<Vec<u8>>>,
    writers: Vec<tokio::task::JoinHandle<()>>,
    ts: TsWriter,
    t0: Option<Instant>,
    samples: u64,
    audio: Option<(u32, u16)>,
    pub path: PathBuf,
}

static PIPE_N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

impl Muxer {
    /// `fragmented`: MP4 que continua tocável mesmo se o PC desligar no meio (gravações longas).
    pub fn start(ffmpeg: &str, path: &Path, audio: Option<(u32, u16)>, fragmented: bool) -> Result<Muxer> {
        let mut args: Vec<String> = ["-hide_banner", "-loglevel", "error", "-y", "-f", "mpegts", "-i", "pipe:0"].map(String::from).to_vec();
        let mut pipe_server = None;
        if let Some((rate, ch)) = audio {
            let name = format!(r"\\.\pipe\gamestream-rec-{}-{}", std::process::id(), PIPE_N.fetch_add(1, std::sync::atomic::Ordering::Relaxed));
            let server = tokio::net::windows::named_pipe::ServerOptions::new()
                .first_pipe_instance(true)
                .access_inbound(false)
                .out_buffer_size(1 << 20)
                .create(&name)
                .context("pipe de áudio")?;
            pipe_server = Some(server);
            args.extend(["-f", "s16le", "-ar", &rate.to_string(), "-ac", &ch.to_string(), "-i", &name].map(String::from));
            args.extend(["-map", "0:v:0", "-map", "1:a:0", "-c:a", "aac", "-b:a", "192k"].map(String::from));
        } else {
            args.extend(["-map", "0:v:0"].map(String::from));
        }
        args.extend(["-c:v", "copy", "-tag:v", "hvc1"].map(String::from));
        let flags = if fragmented { "+frag_keyframe+empty_moov+default_base_moof" } else { "+faststart" };
        args.extend(["-movflags".to_string(), flags.to_string(), path.display().to_string()]);
        let mut child = Command::new(ffmpeg)
            .args(&args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .creation_flags(CREATE_NO_WINDOW)
            .kill_on_drop(true)
            .spawn()
            .context(crate::i18n::t("não foi possível iniciar o ffmpeg.exe"))?;
        if let Some(err) = child.stderr.take() {
            tokio::spawn(async move {
                use tokio::io::AsyncBufReadExt;
                let mut lines = tokio::io::BufReader::new(err).lines();
                while let Ok(Some(l)) = lines.next_line().await {
                    crate::state::log(format!("[gravação] {l}"));
                }
            });
        }
        let mut stdin = child.stdin.take().context("stdin")?;
        let (vtx, mut vrx) = mpsc::unbounded_channel::<Vec<u8>>();
        let mut writers = vec![tokio::spawn(async move {
            while let Some(b) = vrx.recv().await {
                if stdin.write_all(&b).await.is_err() {
                    break;
                }
            }
            let _ = stdin.shutdown().await;
        })];
        let mut audio_tx = None;
        if let Some(mut server) = pipe_server {
            let (atx, mut arx) = mpsc::unbounded_channel::<Vec<u8>>();
            audio_tx = Some(atx);
            writers.push(tokio::spawn(async move {
                if tokio::time::timeout(Duration::from_secs(30), server.connect()).await.is_err() {
                    return;
                }
                while let Some(b) = arx.recv().await {
                    if server.write_all(&b).await.is_err() {
                        break;
                    }
                }
                // Só fecha (sem DisconnectNamedPipe, que descartaria o que o FFmpeg ainda não leu).
                let _ = server.flush().await;
                drop(server);
            }));
        }
        Ok(Muxer { child, video_tx: Some(vtx), audio_tx, writers, ts: TsWriter::default(), t0: None, samples: 0, audio, path: path.to_path_buf() })
    }

    /// Escreve um quadro. Antes do primeiro quadro-chave, ignora (o vídeo começa num keyframe).
    pub fn video(&mut self, au: &Au) {
        if self.t0.is_none() {
            if !au.key {
                return;
            }
            self.t0 = Some(au.at);
        }
        let t0 = self.t0.unwrap();
        // 0,5 s de folga no início (o relógio PCR vem 100 ms antes).
        let pts = 45_000 + (au.at.saturating_duration_since(t0).as_micros() as u64 * 9 / 100);
        let mut out = Vec::with_capacity(au.data.len() + au.data.len() / 180 * 4 + 400);
        self.ts.write_au(&au.data, pts, au.key, &mut out);
        if let Some(tx) = &self.video_tx {
            let _ = tx.send(out);
        }
    }

    /// Escreve áudio (PCM 16 bits; `at` = quando o pacote terminou). Mantém o áudio alinhado ao vídeo:
    /// preenche buracos com silêncio e descarta o que sobrar.
    pub fn audio(&mut self, pcm: &[u8], at: Instant) {
        let (Some(t0), Some((rate, ch)), Some(tx)) = (self.t0, self.audio, &self.audio_tx) else { return };
        let frame = 2 * ch as usize;
        let frames = (pcm.len() / frame) as u64;
        let dur = Duration::from_secs_f64(frames as f64 / rate as f64);
        let Some(start) = at.checked_sub(dur) else { return };
        if start + dur <= t0 {
            return;
        }
        let (pcm, start) = if start < t0 {
            // Pacote que começa antes do vídeo: só a parte depois.
            let skip = ((t0 - start).as_secs_f64() * rate as f64) as usize * frame;
            (&pcm[skip.min(pcm.len())..], t0)
        } else {
            (pcm, start)
        };
        let expected = (start.saturating_duration_since(t0).as_secs_f64() * rate as f64) as u64;
        let mut out = Vec::new();
        if expected > self.samples + rate as u64 / 50 {
            out.resize(((expected - self.samples) as usize) * frame, 0);
            self.samples = expected;
        } else if self.samples > expected + rate as u64 / 10 {
            return; // áudio adiantado (relógios diferentes): descarta este pacote
        }
        out.extend_from_slice(pcm);
        self.samples += (pcm.len() / frame) as u64;
        let _ = tx.send(out);
    }

    /// Fecha as entradas e espera o FFmpeg terminar o arquivo.
    pub async fn finish(mut self) -> Result<PathBuf> {
        self.video_tx = None;
        self.audio_tx = None;
        for w in std::mem::take(&mut self.writers) {
            let _ = tokio::time::timeout(Duration::from_secs(120), w).await;
        }
        let st = tokio::time::timeout(Duration::from_secs(120), self.child.wait()).await.context("o FFmpeg não terminou o arquivo")??;
        anyhow::ensure!(st.success() && self.path.exists(), "o FFmpeg não conseguiu salvar o vídeo");
        Ok(self.path)
    }
}

// ---------------------------------------------------------------------------------------------
// Gravação e replay durante a transmissão
// ---------------------------------------------------------------------------------------------

enum Cmd {
    Toggle,
    Replay,
}

static CMD: LazyLock<Mutex<Option<mpsc::UnboundedSender<Cmd>>>> = LazyLock::new(|| Mutex::new(None));
/// Gravando agora (o encoder não pausa, mesmo sem ninguém assistindo).
static RECORDING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn recording() -> bool {
    RECORDING.load(std::sync::atomic::Ordering::Relaxed)
}

/// Começa/para de gravar (só durante uma transmissão).
pub fn toggle_record() {
    match CMD.lock().unwrap().as_ref() {
        Some(tx) => {
            let _ = tx.send(Cmd::Toggle);
        }
        None => crate::state::log(crate::i18n::t("[gravação] inicie uma transmissão para gravar.")),
    }
}

/// Salva os últimos 30 s (só durante uma transmissão).
pub fn save_replay() {
    match CMD.lock().unwrap().as_ref() {
        Some(tx) => {
            let _ = tx.send(Cmd::Replay);
        }
        None => crate::state::log(crate::i18n::t("[replay] inicie uma transmissão para usar o replay.")),
    }
}

fn saved(path: &Path, what: &str) {
    crate::state::log(crate::tr!("[{what}] salvo em {}", "[{what}] saved to {}", path.display()));
    crate::state::host(|h| h.last_saved = Some(path.display().to_string()));
    crate::sounds::play(crate::sounds::Sound::Saved);
}

/// Acompanha o vídeo e o áudio da transmissão: grava quando pedido e guarda o replay.
/// `video` = pedaços do encoder (raw, horário); `audio` = PCM contínuo (10 ms por pacote).
pub async fn run(
    ffmpeg: String,
    mut video: broadcast::Receiver<crate::host::VideoChunk>,
    mut audio: Option<broadcast::Receiver<Bytes>>,
    audio_fmt: Option<(u32, u16)>,
    fps: u32,
) {
    let (tx, mut cmd) = mpsc::unbounded_channel();
    // Horários dos quadros: o encoder às vezes entrega vários quadros de uma vez; espaça pelo
    // intervalo de um quadro (a captura nunca passa do FPS escolhido).
    let frame = Duration::from_secs_f64(1.0 / fps.max(1) as f64);
    let mut last_at: Option<Instant> = None;
    // Relógio do áudio: contínuo (10 ms por pacote); só reajusta se escorregar mais de 300 ms.
    let mut audio_clock: Option<(Instant, u64)> = None;
    *CMD.lock().unwrap() = Some(tx);
    let mut split = AuSplitter::default();
    let mut aus = Vec::new();
    let mut ring_v: VecDeque<Au> = VecDeque::new();
    let mut ring_bytes = 0usize;
    let mut ring_a: VecDeque<(Instant, Bytes)> = VecDeque::new();
    let mut rec: Option<Muxer> = None;
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    let keep = Duration::from_secs(REPLAY_SECS + 3);
    loop {
        tokio::select! {
            c = video.recv() => match c {
                Ok(c) => {
                    split.push(&c.raw, c.at, &mut aus);
                    let replay_on = crate::settings::replay();
                    for mut au in aus.drain(..) {
                        if let Some(prev) = last_at {
                            au.at = au.at.max(prev + frame);
                        }
                        last_at = Some(au.at);
                        if let Some(m) = rec.as_mut() {
                            m.video(&au);
                        }
                        if replay_on {
                            ring_bytes += au.data.len();
                            ring_v.push_back(au);
                        }
                    }
                    while ring_v.front().is_some_and(|a| a.at.elapsed() > keep) || ring_bytes > REPLAY_MAX_BYTES {
                        let Some(a) = ring_v.pop_front() else { break };
                        ring_bytes -= a.data.len();
                    }
                    if !replay_on && !ring_v.is_empty() {
                        ring_v.clear();
                        ring_a.clear();
                        ring_bytes = 0;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => split.reset(),
                Err(broadcast::error::RecvError::Closed) => break,
            },
            a = async { match audio.as_mut() { Some(rx) => rx.recv().await, None => std::future::pending().await } } => match a {
                Ok(pcm) => {
                    let now = Instant::now();
                    let (rate, ch) = audio_fmt.unwrap_or((48_000, 2));
                    let frames = (pcm.len() / (2 * ch as usize)) as u64;
                    let (base, done) = *audio_clock.get_or_insert((now, 0));
                    let mut at = base + Duration::from_secs_f64((done + frames) as f64 / rate as f64);
                    if at > now + Duration::from_millis(300) || now > at + Duration::from_millis(300) {
                        audio_clock = Some((now - Duration::from_secs_f64(frames as f64 / rate as f64), 0));
                        at = now;
                    }
                    if let Some((_, d)) = audio_clock.as_mut() {
                        *d += frames;
                    }
                    if let Some(m) = rec.as_mut() {
                        m.audio(&pcm, at);
                    }
                    if !ring_v.is_empty() {
                        ring_a.push_back((at, pcm));
                    }
                    while ring_a.front().is_some_and(|(t, _)| t.elapsed() > keep) {
                        ring_a.pop_front();
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => audio = None,
            },
            c = cmd.recv() => match c {
                Some(Cmd::Toggle) => {
                    if let Some(m) = rec.take() {
                        RECORDING.store(false, std::sync::atomic::Ordering::Relaxed);
                        crate::state::host(|h| h.recording = None);
                        crate::state::log(crate::i18n::t("[gravação] parando; terminando o arquivo..."));
                        tokio::spawn(async move {
                            match m.finish().await {
                                Ok(p) => saved(&p, crate::i18n::t("gravação")),
                                Err(e) => crate::state::log(crate::tr!("[gravação] erro: {e:#}", "[recording] error: {e:#}")),
                            }
                        });
                    } else {
                        let path = file_name(crate::i18n::t("Gravação"));
                        match Muxer::start(&ffmpeg, &path, audio_fmt, true) {
                            Ok(m) => {
                                crate::state::log(crate::tr!("[gravação] gravando em {} (começa no próximo quadro-chave)", "[recording] recording to {} (starts at the next keyframe)", path.display()));
                                let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                                crate::state::host(|h| h.recording = Some((Instant::now(), name)));
                                RECORDING.store(true, std::sync::atomic::Ordering::Relaxed);
                                rec = Some(m);
                            }
                            Err(e) => crate::state::log(crate::tr!("[gravação] erro: {e:#}", "[recording] error: {e:#}")),
                        }
                    }
                }
                Some(Cmd::Replay) => {
                    if !crate::settings::replay() {
                        crate::state::log(crate::i18n::t("[replay] o replay instantâneo está desligado (ligue no card da transmissão)."));
                        continue;
                    }
                    let start = Instant::now().checked_sub(Duration::from_secs(REPLAY_SECS)).unwrap_or_else(Instant::now);
                    // Começa no primeiro quadro-chave dos últimos 30 s (ou no mais antigo guardado).
                    let first = ring_v.iter().position(|a| a.key && a.at >= start).or_else(|| ring_v.iter().position(|a| a.key));
                    let Some(first) = first else {
                        crate::state::log(crate::i18n::t("[replay] ainda não há vídeo guardado (ninguém assistindo = codificação pausada)."));
                        continue;
                    };
                    let v: Vec<Au> = ring_v.iter().skip(first).cloned().collect();
                    let a: Vec<(Instant, Bytes)> = ring_a.iter().cloned().collect();
                    let path = file_name(crate::i18n::t("Replay"));
                    let ffmpeg = ffmpeg.clone();
                    crate::state::log(crate::i18n::t("[replay] salvando os últimos 30 segundos..."));
                    tokio::spawn(async move {
                        let r = async {
                            let mut m = Muxer::start(&ffmpeg, &path, audio_fmt, false)?;
                            for au in &v {
                                m.video(au);
                            }
                            for (at, pcm) in &a {
                                m.audio(pcm, *at);
                            }
                            m.finish().await
                        }
                        .await;
                        match r {
                            Ok(p) => saved(&p, "replay"),
                            Err(e) => crate::state::log(crate::tr!("[replay] erro: {e:#}", "[replay] error: {e:#}")),
                        }
                    });
                }
                None => break,
            },
            _ = tick.tick() => {
                let secs = match (ring_v.front(), ring_v.back()) {
                    (Some(a), Some(b)) => b.at.saturating_duration_since(a.at).as_secs().min(REPLAY_SECS) as u32,
                    _ => 0,
                };
                crate::state::host(|h| h.replay_secs = secs);
            }
        }
    }
    *CMD.lock().unwrap() = None;
    RECORDING.store(false, std::sync::atomic::Ordering::Relaxed);
    if let Some(m) = rec.take() {
        crate::state::host(|h| h.recording = None);
        match m.finish().await {
            Ok(p) => saved(&p, crate::i18n::t("gravação")),
            Err(e) => crate::state::log(crate::tr!("[gravação] erro: {e:#}", "[recording] error: {e:#}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_access_units_across_chunks() {
        // VPS SPS PPS IDR(1ª fatia) | TRAIL(1ª fatia) | TRAIL(1ª fatia) + TRAIL (2ª fatia)
        let au1 = [&[0u8, 0, 0, 1, 0x40, 1, 0xaa][..], &[0, 0, 1, 0x42, 1, 0xbb], &[0, 0, 1, 0x44, 1, 0xcc], &[0, 0, 1, 0x26, 1, 0x80, 9, 9]].concat();
        let au2 = [&[0u8, 0, 0, 1, 0x02, 1, 0x80, 7, 7][..]].concat();
        let au3 = [&[0u8, 0, 0, 1, 0x02, 1, 0x80, 5][..], &[0, 0, 1, 0x02, 1, 0x00, 6]].concat();
        let stream = [au1.clone(), au2.clone(), au3.clone(), vec![0, 0, 0, 1, 0x02, 1, 0x80, 1, 1, 1]].concat();
        let mut s = AuSplitter::default();
        let mut out = Vec::new();
        let t = Instant::now();
        for c in stream.chunks(3) {
            s.push(c, t, &mut out);
        }
        assert_eq!(out.len(), 3);
        assert_eq!(&out[0].data[..], &au1[..]);
        assert!(out[0].key);
        assert_eq!(&out[1].data[..], &au2[..]);
        assert!(!out[1].key);
        assert_eq!(&out[2].data[..], &au3[..]);
    }

    #[test]
    fn ts_packets_are_well_formed() {
        let mut w = TsWriter::default();
        let mut out = Vec::new();
        for (i, len) in [10usize, 183, 184, 400, 5000].iter().enumerate() {
            w.write_au(&vec![0x11; *len], 45_000 + i as u64 * 1500, i == 0, &mut out);
        }
        assert_eq!(out.len() % 188, 0);
        assert!(out.chunks(188).all(|p| p[0] == 0x47));
        // CRC conhecido da PAT de 1 programa (PMT 0x1000).
        assert_eq!(crc32_mpeg(&[0x00, 0xb0, 0x0d, 0x00, 0x01, 0xc1, 0x00, 0x00, 0x00, 0x01, 0xf0, 0x00]), 0x2ab104b2);
    }
}
