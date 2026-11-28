//! Áudio: captura WASAPI loopback (tudo que toca no PC do host) e reprodução no viewer.
//! Formato na rede: PCM i16 intercalado (48 kHz estéreo ≈ 1,5 Mbps — irrelevante perto do vídeo).

use std::{
    collections::VecDeque,
    sync::{
        atomic::{AtomicBool, AtomicU32, Ordering},
        mpsc, Arc, Mutex,
    },
};

/// Volume do viewer (f32 em bits; 1.0 = 100%) e mudo — alterados pela interface em tempo real.
static VOLUME: AtomicU32 = AtomicU32::new(1.0f32.to_bits());
static MUTED: AtomicBool = AtomicBool::new(false);

pub fn set_volume(v: f32) {
    VOLUME.store(v.clamp(0.0, 2.0).to_bits(), Ordering::Relaxed);
}

pub fn set_muted(m: bool) {
    MUTED.store(m, Ordering::Relaxed);
}

fn gain() -> f32 {
    if MUTED.load(Ordering::Relaxed) {
        0.0
    } else {
        f32::from_bits(VOLUME.load(Ordering::Relaxed))
    }
}

/// De onde o host captura o áudio.
#[derive(Clone, Debug, PartialEq)]
pub enum AudioSel {
    /// Só o som do jogo (processo da janela transmitida).
    GameWindow,
    /// Todo o áudio do sistema, exceto o Discord (evita o retorno de voz para participantes da chamada).
    AllExceptDiscord,
    /// Só o som de um programa.
    App { pid: u32, name: String },
    /// Tudo que toca na saída padrão do Windows (inclui o Discord!).
    DefaultOutput,
    /// Tudo que toca numa saída específica (fone, caixa, monitor HDMI...).
    Output(String),
    /// Uma entrada (mixagem estéreo, placa de captura...).
    Input(String),
}

impl AudioSel {
    pub fn label(&self) -> String {
        match self {
            AudioSel::GameWindow => crate::i18n::t("🎮 Aplicativo transmitido (somente o áudio da janela)").into(),
            AudioSel::AllExceptDiscord => crate::i18n::t("Áudio do sistema, exceto Discord").into(),
            AudioSel::App { name, .. } => crate::tr!("Aplicativo: {name}", "Application: {name}"),
            AudioSel::DefaultOutput => crate::i18n::t("🔊 Áudio do sistema (inclui Discord)").into(),
            AudioSel::Output(n) => crate::tr!("🔊 Saída: {n}", "🔊 Output: {n}"),
            AudioSel::Input(n) => crate::tr!("🎤 Entrada: {n}", "🎤 Input: {n}"),
        }
    }
}

/// Fonte de áudio já resolvida (o host decide a partir de `AudioSel` e da fonte de vídeo).
#[derive(Clone, Debug)]
pub enum CaptureSource {
    /// Loopback de uma saída (None = padrão).
    Output(Option<String>),
    Input(String),
    /// Só este processo e filhos.
    ProcessOnly(u32),
    /// Tudo menos este processo e filhos.
    ProcessExcept(u32),
}

pub fn output_devices() -> Vec<String> {
    cpal::default_host()
        .output_devices()
        .map(|it| it.filter_map(|d| d.name().ok()).collect())
        .unwrap_or_default()
}

pub fn input_devices() -> Vec<String> {
    cpal::default_host()
        .input_devices()
        .map(|it| it.filter_map(|d| d.name().ok()).collect())
        .unwrap_or_default()
}

/// Microfone padrão do Windows.
pub fn default_input() -> Option<String> {
    cpal::default_host().default_input_device().and_then(|d| d.name().ok())
}

fn find_output(name: Option<&str>) -> Result<cpal::Device> {
    let host = cpal::default_host();
    if let Some(n) = name {
        if let Some(d) = host.output_devices()?.find(|d| d.name().ok().as_deref() == Some(n)) {
            return Ok(d);
        }
        crate::state::log(crate::tr!("[áudio] \"{n}\" não encontrado; usando o dispositivo padrão", "[audio] \"{n}\" not found; using the default device"));
    }
    host.default_output_device().context(crate::i18n::t("nenhum dispositivo de saída de áudio"))
}

use anyhow::{bail, Context, Result};
use bytes::Bytes;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample};
use tokio::sync::broadcast;

/// Mantém um stream do cpal vivo numa thread própria; ao ser descartado, encerra o stream.
pub struct AudioHandle {
    stop: Arc<AtomicBool>,
    thread: std::thread::Thread,
}

impl Drop for AudioHandle {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.thread.unpark();
    }
}

/// Roda `build` numa thread dedicada (streams do cpal não são `Send`) e devolve (handle, rate, canais).
fn spawn_stream(
    build: impl FnOnce() -> Result<(cpal::Stream, u32, u16)> + Send + 'static,
) -> Result<(AudioHandle, u32, u16)> {
    let (ready_tx, ready_rx) = mpsc::channel();
    let stop = Arc::new(AtomicBool::new(false));
    let stop2 = stop.clone();
    let t = std::thread::spawn(move || match build() {
        Ok((stream, rate, ch)) => {
            let _ = ready_tx.send(Ok((rate, ch)));
            while !stop2.load(Ordering::Relaxed) {
                std::thread::park();
            }
            drop(stream);
        }
        Err(e) => {
            let _ = ready_tx.send(Err(e));
        }
    });
    let (rate, ch) = ready_rx.recv()??;
    Ok((AudioHandle { stop, thread: t.thread().clone() }, rate, ch))
}

/// Converte qualquer taxa/nº de canais para 48 kHz estéreo (vizinho mais próximo; suficiente para voz/jogo).
struct Resampler {
    in_ch: usize,
    step: f64,
    pos: f64,
    out: Vec<f32>,
}

impl Resampler {
    fn new(in_rate: u32, in_ch: u16) -> Self {
        Resampler { in_ch: in_ch.max(1) as usize, step: in_rate as f64 / MIX_RATE as f64, pos: 0.0, out: Vec::new() }
    }

    fn process(&mut self, inp: &[f32]) -> &[f32] {
        let frames = inp.len() / self.in_ch;
        self.out.clear();
        while self.pos < frames as f64 {
            let i = self.pos as usize * self.in_ch;
            let (l, r) = if self.in_ch == 1 { (inp[i], inp[i]) } else { (inp[i], inp[i + 1]) };
            self.out.push(l);
            self.out.push(r);
            self.pos += self.step;
        }
        self.pos -= frames as f64;
        &self.out
    }
}

const MIX_RATE: u32 = 48_000;
const MIX_CH: u16 = 2;
type Queue = Arc<Mutex<VecDeque<f32>>>;

fn push_queue(q: &Queue, samples: &[f32]) {
    let mut q = q.lock().unwrap();
    q.extend(samples.iter().copied());
    // Nunca acumula mais que 200 ms (evita atraso crescente se um relógio andar mais rápido).
    let max = (MIX_RATE as usize * MIX_CH as usize) / 5;
    if q.len() > max {
        let drop = q.len() - max / 2;
        q.drain(..drop);
    }
}

/// Captura em andamento: ao ser descartada, para todas as fontes e o mixer.
pub struct CapturePipeline {
    stop: Arc<AtomicBool>,
    _streams: Vec<AudioHandle>,
}

impl Drop for CapturePipeline {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

fn start_source(src: &CaptureSource, q: Queue, stop: Arc<AtomicBool>) -> Result<Option<AudioHandle>> {
    match src {
        CaptureSource::ProcessOnly(pid) | CaptureSource::ProcessExcept(pid) => {
            let include = matches!(src, CaptureSource::ProcessOnly(_));
            let mut rs = Resampler::new(crate::procaudio::RATE, crate::procaudio::CHANNELS);
            crate::procaudio::spawn_process_capture(*pid, include, stop, move |s| push_queue(&q, rs.process(s)))?;
            Ok(None)
        }
        CaptureSource::Output(name) => {
            let name = name.clone();
            let (h, _, _) = spawn_stream(move || {
                let dev = find_output(name.as_deref())?;
                let cfg = dev.default_output_config()?;
                build_device_capture(&dev, cfg, q)
            })?;
            Ok(Some(h))
        }
        CaptureSource::Input(n) => {
            let n = n.clone();
            let (h, _, _) = spawn_stream(move || {
                let dev = cpal::default_host()
                    .input_devices()?
                    .find(|d| d.name().ok().as_deref() == Some(n.as_str()))
                    .with_context(|| crate::tr!("entrada de áudio \"{n}\" não encontrada", "audio input \"{n}\" not found"))?;
                let cfg = dev.default_input_config()?;
                build_device_capture(&dev, cfg, q)
            })?;
            Ok(Some(h))
        }
    }
}

fn build_device_capture(
    dev: &cpal::Device,
    cfg: cpal::SupportedStreamConfig,
    q: Queue,
) -> Result<(cpal::Stream, u32, u16)> {
    let (rate, ch) = (cfg.sample_rate().0, cfg.channels());
    let scfg: cpal::StreamConfig = cfg.clone().into();
    // Em WASAPI, abrir um stream de ENTRADA num dispositivo de SAÍDA = loopback.
    let stream = match cfg.sample_format() {
        SampleFormat::F32 => build_capture::<f32>(dev, &scfg, q)?,
        SampleFormat::I16 => build_capture::<i16>(dev, &scfg, q)?,
        SampleFormat::I32 => build_capture::<i32>(dev, &scfg, q)?,
        f => bail!("{}", crate::tr!("formato de áudio não suportado: {f:?}", "unsupported audio format: {f:?}")),
    };
    stream.play()?;
    Ok((stream, rate, ch))
}

fn build_capture<T>(dev: &cpal::Device, cfg: &cpal::StreamConfig, q: Queue) -> Result<cpal::Stream>
where
    T: SizedSample,
    f32: FromSample<T>,
{
    let mut rs = Resampler::new(cfg.sample_rate.0, cfg.channels);
    let mut tmp: Vec<f32> = Vec::new();
    Ok(dev.build_input_stream(
        cfg,
        move |data: &[T], _| {
            tmp.clear();
            tmp.extend(data.iter().map(|&s| <f32 as FromSample<T>>::from_sample_(s)));
            push_queue(&q, rs.process(&tmp));
        },
        |e| crate::state::log(crate::tr!("[áudio] erro na captura: {e}", "[audio] capture error: {e}")),
        None,
    )?)
}

/// Cópia do áudio misturado em PCM 16 bits (10 ms por pacote), para gravação e replay.
pub static PCM_TAP: std::sync::LazyLock<Mutex<Option<broadcast::Sender<Bytes>>>> = std::sync::LazyLock::new(|| Mutex::new(None));
/// Modo privacidade: manda silêncio (o relógio do áudio continua andando).
pub static MUTE: AtomicBool = AtomicBool::new(false);

/// Começa a capturar: a fonte principal e, opcionalmente, o microfone, misturados a cada 10 ms.
/// Envia pacotes já enquadrados (`[u32 len][pcm i16]`), 48 kHz estéreo. Retorna (pipeline, rate, canais).
/// Bitrate do Opus: 160 kbps estéreo, faixa considerada transparente em testes de audição.
pub const OPUS_BITRATE: i32 = 160_000;

/// `opus`: comprime cada pacote de 10 ms com Opus (~0,16 Mbps) em vez de PCM (~1,5 Mbps).
pub fn start_capture(
    tx: broadcast::Sender<Bytes>,
    main: CaptureSource,
    mic: Option<(String, f32)>,
    opus: bool,
) -> Result<(CapturePipeline, u32, u16)> {
    let stop = Arc::new(AtomicBool::new(false));
    let mut streams = Vec::new();
    let mut queues: Vec<(Queue, f32)> = Vec::new();

    let q: Queue = Arc::new(Mutex::new(VecDeque::new()));
    if let Some(h) = start_source(&main, q.clone(), stop.clone())? {
        streams.push(h);
    }
    queues.push((q, 1.0));

    if let Some((dev, gain)) = mic {
        let q: Queue = Arc::new(Mutex::new(VecDeque::new()));
        match start_source(&CaptureSource::Input(dev.clone()), q.clone(), stop.clone()) {
            Ok(h) => {
                streams.extend(h);
                queues.push((q, gain));
                crate::state::log(crate::tr!("[áudio] microfone ligado: {dev}", "[audio] microphone on: {dev}"));
            }
            Err(e) => crate::state::log(crate::tr!("[áudio] microfone indisponível: {e:#}", "[audio] microphone unavailable: {e:#}")),
        }
    }

    let stop2 = stop.clone();
    let encoder = if opus {
        let mut e = opus::Encoder::new(MIX_RATE, opus::Channels::Stereo, opus::Application::Audio)?;
        e.set_bitrate(opus::Bitrate::Bits(OPUS_BITRATE))?;
        Some(e)
    } else {
        None
    };
    std::thread::spawn(move || mixer(queues, tx, stop2, encoder));
    Ok((CapturePipeline { stop, _streams: streams }, MIX_RATE, MIX_CH))
}

/// Soma as fontes num relógio próprio (10 ms por pacote). Cada fonte só entra depois de juntar
/// 40 ms (buffer de jitter), e volta a esperar se esvaziar.
fn mixer(sources: Vec<(Queue, f32)>, tx: broadcast::Sender<Bytes>, stop: Arc<AtomicBool>, mut opus: Option<opus::Encoder>) {
    let mut packet = vec![0u8; 4000];
    const FRAMES: usize = 480; // 10 ms
    let samples = FRAMES * MIX_CH as usize;
    let prime = (MIX_RATE as usize * MIX_CH as usize) / 25; // 40 ms
    let start = std::time::Instant::now();
    let mut produced: u64 = 0;
    let mut primed = vec![false; sources.len()];
    let mut mix = vec![0f32; samples];
    while !stop.load(Ordering::Relaxed) {
        std::thread::sleep(std::time::Duration::from_millis(4));
        let due = (start.elapsed().as_secs_f64() * MIX_RATE as f64) as u64;
        while due.saturating_sub(produced) >= FRAMES as u64 {
            mix.iter_mut().for_each(|s| *s = 0.0);
            for (k, (q, g)) in sources.iter().enumerate() {
                let mut q = q.lock().unwrap();
                if !primed[k] {
                    if q.len() < prime {
                        continue;
                    }
                    primed[k] = true;
                }
                for s in mix.iter_mut() {
                    match q.pop_front() {
                        Some(v) => *s += v * g,
                        None => {
                            primed[k] = false;
                            break;
                        }
                    }
                }
            }
            if MUTE.load(Ordering::Relaxed) {
                mix.iter_mut().for_each(|s| *s = 0.0);
            }
            if let Some(tap) = PCM_TAP.lock().unwrap().as_ref().filter(|t| t.receiver_count() > 0) {
                let mut pcm = Vec::with_capacity(samples * 2);
                for &s in &mix {
                    pcm.extend_from_slice(&((s.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes());
                }
                let _ = tap.send(Bytes::from(pcm));
            }
            let out = if let Some(enc) = opus.as_mut() {
                mix.iter_mut().for_each(|s| *s = s.clamp(-1.0, 1.0));
                match enc.encode_float(&mix, &mut packet) {
                    Ok(n) => crate::net::frame(&packet[..n]),
                    Err(_) => {
                        produced += FRAMES as u64;
                        continue;
                    }
                }
            } else {
                let mut out = Vec::with_capacity(4 + samples * 2);
                out.extend_from_slice(&((samples * 2) as u32).to_le_bytes());
                for &s in &mix {
                    out.extend_from_slice(&((s.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes());
                }
                out
            };
            let _ = tx.send(Bytes::from(out));
            produced += FRAMES as u64;
        }
    }
}

/// Atraso extra do som de quem assiste (ms), para casar com a imagem: a imagem passa pela captura,
/// pela compressão e é bem mais pesada, então costuma chegar um pouco depois do som.
static AUDIO_DELAY_MS: AtomicU32 = AtomicU32::new(0);

pub fn set_audio_delay(ms: u32) {
    AUDIO_DELAY_MS.store(ms.min(500), Ordering::Relaxed);
}

pub fn audio_delay() -> u32 {
    AUDIO_DELAY_MS.load(Ordering::Relaxed)
}

/// Reprodutor com buffer de jitter pequeno (~40 ms) que descarta excesso para não acumular atraso.
pub struct Player {
    queue: Arc<Mutex<VecDeque<f32>>>,
    in_ch: usize,
    out_ch: usize,
    step: f64,
    pos: f64,
    scratch: Vec<f32>,
    _handle: AudioHandle,
}

pub fn start_player(in_rate: u32, in_ch: u16, device: Option<String>) -> Result<Player> {
    let queue = Arc::new(Mutex::new(VecDeque::<f32>::with_capacity(48_000)));
    let q = queue.clone();
    let (handle, out_rate, out_ch) = spawn_stream(move || {
        let dev = find_output(device.as_deref())?;
        let cfg = dev.default_output_config()?;
        let (rate, ch) = (cfg.sample_rate().0, cfg.channels());
        let scfg: cpal::StreamConfig = cfg.clone().into();
        let stream = match cfg.sample_format() {
            SampleFormat::F32 => build_output::<f32>(&dev, &scfg, q)?,
            SampleFormat::I16 => build_output::<i16>(&dev, &scfg, q)?,
            SampleFormat::I32 => build_output::<i32>(&dev, &scfg, q)?,
            f => bail!("{}", crate::tr!("formato de áudio não suportado: {f:?}", "unsupported audio format: {f:?}")),
        };
        stream.play()?;
        Ok((stream, rate, ch))
    })?;
    Ok(Player {
        queue,
        in_ch: in_ch.max(1) as usize,
        out_ch: out_ch as usize,
        step: in_rate as f64 / out_rate as f64,
        pos: 0.0,
        scratch: Vec::new(),
        _handle: handle,
    })
}

impl Player {
    /// Recebe PCM i16 LE intercalado do host; adapta taxa (vizinho mais próximo) e nº de canais.
    /// Recebe amostras f32 intercaladas (áudio Opus já decodificado).
    pub fn push_f32(&mut self, pcm: &[f32]) {
        let frames = pcm.len() / self.in_ch;
        if frames == 0 {
            return;
        }
        self.scratch.clear();
        while self.pos < frames as f64 {
            let i = self.pos as usize;
            for c in 0..self.out_ch {
                self.scratch.push(pcm[i * self.in_ch + c.min(self.in_ch - 1)]);
            }
            self.pos += self.step;
        }
        self.pos -= frames as f64;
        self.queue.lock().unwrap().extend(self.scratch.iter().copied());
    }

    pub fn push(&mut self, pcm: &[u8]) {
        let frames = pcm.len() / 2 / self.in_ch;
        if frames == 0 {
            return;
        }
        self.scratch.clear();
        while self.pos < frames as f64 {
            let i = self.pos as usize;
            for c in 0..self.out_ch {
                let src = i * self.in_ch + c.min(self.in_ch - 1);
                let s = i16::from_le_bytes([pcm[src * 2], pcm[src * 2 + 1]]);
                self.scratch.push(s as f32 / 32768.0);
            }
            self.pos += self.step;
        }
        self.pos -= frames as f64;
        self.queue.lock().unwrap().extend(self.scratch.iter().copied());
    }
}

fn build_output<T>(dev: &cpal::Device, cfg: &cpal::StreamConfig, q: Arc<Mutex<VecDeque<f32>>>) -> Result<cpal::Stream>
where
    T: SizedSample + FromSample<f32>,
{
    let per_ms = (cfg.sample_rate.0 as usize * cfg.channels as usize) / 1000;
    let mut priming = true;
    let mut applied_delay = 0usize;
    Ok(dev.build_output_stream(
        cfg,
        move |out: &mut [T], _| {
            let mut q = q.lock().unwrap();
            // Atraso para sincronizar com a imagem: mudou, ajusta na hora (silêncio a mais ou descarta).
            let delay = audio_delay() as usize;
            if delay != applied_delay {
                if delay > applied_delay {
                    for _ in 0..(delay - applied_delay) * per_ms {
                        q.push_front(0.0);
                    }
                } else {
                    let n = ((applied_delay - delay) * per_ms).min(q.len());
                    q.drain(..n);
                }
                applied_delay = delay;
            }
            let prime = per_ms * (40 + delay); // espera 40 ms de áudio (mais o atraso) antes de tocar
            let max = per_ms * (150 + delay); // acima disso, descarta (não acumula atraso)
            if q.len() > max {
                let drop = q.len() - prime;
                q.drain(..drop);
            }
            if priming && q.len() < prime {
                out.fill(T::EQUILIBRIUM);
                return;
            }
            priming = false;
            let g = gain();
            for s in out.iter_mut() {
                *s = match q.pop_front() {
                    Some(v) => <T as FromSample<f32>>::from_sample_((v * g).clamp(-1.0, 1.0)),
                    None => {
                        priming = true;
                        T::EQUILIBRIUM
                    }
                };
            }
        },
        |e| crate::state::log(crate::tr!("[áudio] erro na reprodução: {e}", "[audio] playback error: {e}")),
        None,
    )?)
}
