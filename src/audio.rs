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
    /// O que toca na saída padrão do Windows (loopback).
    DefaultOutput,
    /// Loopback de uma saída específica (fone, caixa, monitor HDMI...).
    Output(String),
    /// Uma entrada (microfone, mixagem estéreo, placa de captura...).
    Input(String),
}

impl AudioSel {
    pub fn label(&self) -> String {
        match self {
            AudioSel::DefaultOutput => "Padrão do Windows (o que você está ouvindo)".into(),
            AudioSel::Output(n) => format!("🔊 {n}"),
            AudioSel::Input(n) => format!("🎤 {n}"),
        }
    }
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

fn find_output(name: Option<&str>) -> Result<cpal::Device> {
    let host = cpal::default_host();
    if let Some(n) = name {
        if let Some(d) = host.output_devices()?.find(|d| d.name().ok().as_deref() == Some(n)) {
            return Ok(d);
        }
        crate::state::log(format!("[áudio] \"{n}\" não encontrado; usando o dispositivo padrão"));
    }
    host.default_output_device().context("nenhum dispositivo de saída de áudio")
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

/// Começa a capturar o áudio do sistema. Cada pacote enviado já vem enquadrado (`[u32 len][pcm]`).
pub fn start_capture(tx: broadcast::Sender<Bytes>, sel: AudioSel) -> Result<(AudioHandle, u32, u16)> {
    spawn_stream(move || {
        let (dev, cfg) = match &sel {
            AudioSel::DefaultOutput => {
                let d = find_output(None)?;
                let c = d.default_output_config()?;
                (d, c)
            }
            AudioSel::Output(n) => {
                let d = find_output(Some(n))?;
                let c = d.default_output_config()?;
                (d, c)
            }
            AudioSel::Input(n) => {
                let d = cpal::default_host()
                    .input_devices()?
                    .find(|d| d.name().ok().as_deref() == Some(n.as_str()))
                    .with_context(|| format!("entrada de áudio \"{n}\" não encontrada"))?;
                let c = d.default_input_config()?;
                (d, c)
            }
        };
        let (rate, ch) = (cfg.sample_rate().0, cfg.channels());
        let scfg: cpal::StreamConfig = cfg.clone().into();
        // Em WASAPI, abrir um stream de ENTRADA num dispositivo de SAÍDA = loopback.
        let stream = match cfg.sample_format() {
            SampleFormat::F32 => build_capture::<f32>(&dev, &scfg, tx)?,
            SampleFormat::I16 => build_capture::<i16>(&dev, &scfg, tx)?,
            SampleFormat::I32 => build_capture::<i32>(&dev, &scfg, tx)?,
            f => bail!("formato de áudio não suportado: {f:?}"),
        };
        stream.play()?;
        Ok((stream, rate, ch))
    })
}

fn build_capture<T>(dev: &cpal::Device, cfg: &cpal::StreamConfig, tx: broadcast::Sender<Bytes>) -> Result<cpal::Stream>
where
    T: SizedSample,
    i16: FromSample<T>,
{
    Ok(dev.build_input_stream(
        cfg,
        move |data: &[T], _| {
            let mut out = Vec::with_capacity(4 + data.len() * 2);
            out.extend_from_slice(&((data.len() * 2) as u32).to_le_bytes());
            for &s in data {
                out.extend_from_slice(&<i16 as FromSample<T>>::from_sample_(s).to_le_bytes());
            }
            let _ = tx.send(Bytes::from(out));
        },
        |e| crate::state::log(format!("[áudio] erro na captura: {e}")),
        None,
    )?)
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
            f => bail!("formato de áudio não suportado: {f:?}"),
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
    let prime = per_ms * 40; // espera 40 ms de áudio antes de tocar
    let max = per_ms * 150; // acima de 150 ms de atraso, descarta
    let mut priming = true;
    Ok(dev.build_output_stream(
        cfg,
        move |out: &mut [T], _| {
            let mut q = q.lock().unwrap();
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
        |e| crate::state::log(format!("[áudio] erro na reprodução: {e}")),
        None,
    )?)
}
