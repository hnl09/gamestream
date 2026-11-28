//! Sons discretos de aviso (alguém entrou, pediu para jogar, mandou mensagem). Gerados no próprio
//! app (sem arquivos) e tocados pelo Windows sem bloquear. Podem ser desligados no menu ⚙.

use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sound {
    /// Alguém entrou na sala (ou chegou na sala de espera).
    Join,
    /// Mensagem no chat.
    Chat,
    /// Pedido de controle / controle liberado.
    Request,
    /// Vídeo salvo (replay ou gravação) e modo privacidade ligado/desligado.
    Saved,
}

const RATE: u32 = 44_100;

/// Notas (frequência Hz, duração s) tocadas em sequência, com volume baixo e envelope suave.
fn notes(s: Sound) -> &'static [(f32, f32)] {
    match s {
        Sound::Join => &[(784.0, 0.09), (1046.5, 0.16)],   // sol -> dó (subindo)
        Sound::Chat => &[(1318.5, 0.07)],                  // "plim" curto
        Sound::Request => &[(880.0, 0.08), (0.0, 0.05), (880.0, 0.10)], // dois toques
        Sound::Saved => &[(1046.5, 0.06), (1568.0, 0.09)],                 // "clique" curto
    }
}

/// Arquivo WAV (PCM 16 bits, mono) em memória.
fn wav(s: Sound) -> Vec<u8> {
    let mut pcm: Vec<i16> = Vec::new();
    for &(freq, dur) in notes(s) {
        let n = (RATE as f32 * dur) as usize;
        for i in 0..n {
            let t = i as f32 / RATE as f32;
            // Ataque de 5 ms e queda exponencial: som de "sino", sem estalos.
            let env = (t / 0.005).min(1.0) * (-t * 18.0).exp();
            let v = if freq > 0.0 { (t * freq * std::f32::consts::TAU).sin() * env * 0.18 } else { 0.0 };
            pcm.push((v * i16::MAX as f32) as i16);
        }
    }
    let data_len = (pcm.len() * 2) as u32;
    let mut w = Vec::with_capacity(44 + data_len as usize);
    w.extend_from_slice(b"RIFF");
    w.extend_from_slice(&(36 + data_len).to_le_bytes());
    w.extend_from_slice(b"WAVEfmt ");
    w.extend_from_slice(&16u32.to_le_bytes());
    w.extend_from_slice(&1u16.to_le_bytes()); // PCM
    w.extend_from_slice(&1u16.to_le_bytes()); // mono
    w.extend_from_slice(&RATE.to_le_bytes());
    w.extend_from_slice(&(RATE * 2).to_le_bytes());
    w.extend_from_slice(&2u16.to_le_bytes());
    w.extend_from_slice(&16u16.to_le_bytes());
    w.extend_from_slice(b"data");
    w.extend_from_slice(&data_len.to_le_bytes());
    for v in pcm {
        w.extend_from_slice(&v.to_le_bytes());
    }
    w
}

static JOIN: LazyLock<Vec<u8>> = LazyLock::new(|| wav(Sound::Join));
static CHAT: LazyLock<Vec<u8>> = LazyLock::new(|| wav(Sound::Chat));
static REQUEST: LazyLock<Vec<u8>> = LazyLock::new(|| wav(Sound::Request));
static SAVED: LazyLock<Vec<u8>> = LazyLock::new(|| wav(Sound::Saved));
static LAST: LazyLock<Mutex<Option<(Sound, Instant)>>> = LazyLock::new(|| Mutex::new(None));

/// Toca um aviso (se os sons estiverem ligados). O mesmo som não repete em menos de 1,5 s.
pub fn play(s: Sound) {
    if !crate::settings::sounds() {
        return;
    }
    {
        let mut last = LAST.lock().unwrap();
        if last.is_some_and(|(k, t)| k == s && t.elapsed() < Duration::from_millis(1500)) {
            return;
        }
        *last = Some((s, Instant::now()));
    }
    let data: &'static [u8] = match s {
        Sound::Join => &JOIN,
        Sound::Chat => &CHAT,
        Sound::Request => &REQUEST,
        Sound::Saved => &SAVED,
    };
    use windows::Win32::Media::Audio::{PlaySoundW, SND_ASYNC, SND_MEMORY, SND_NODEFAULT};
    unsafe {
        // Os dados são estáticos: continuam válidos enquanto o Windows toca de forma assíncrona.
        let _ = PlaySoundW(windows::core::PCWSTR(data.as_ptr() as *const u16), None, SND_MEMORY | SND_ASYNC | SND_NODEFAULT);
    }
}

#[cfg(test)]
mod tests {
    /// Grava os sons em %TEMP% (para conferir o formato com o leitor do Windows).
    #[test]
    fn dump_wavs() {
        for (n, s) in [("join", super::Sound::Join), ("chat", super::Sound::Chat), ("request", super::Sound::Request)] {
            std::fs::write(std::env::temp_dir().join(format!("gamestream-sound-{n}.wav")), super::wav(s)).unwrap();
        }
    }

    #[test]
    fn wav_header() {
        let w = super::wav(super::Sound::Join);
        assert_eq!(&w[0..4], b"RIFF");
        assert_eq!(&w[8..12], b"WAVE");
        assert_eq!(u32::from_le_bytes(w[4..8].try_into().unwrap()) as usize, w.len() - 8);
    }
}
