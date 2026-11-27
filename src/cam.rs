//! Webcam de quem quiser (transmissor e espectadores), como numa chamada de vídeo do Discord.
//!
//! Captura: FFmpeg lê a câmera (DirectShow) e comprime em H.264 pequeno (até 240 linhas, 15 fps,
//! ~0,35 Mbps). Rede: o espectador envia a câmera dele ao transmissor (stream QUIC STREAM_CAM) e o
//! transmissor repassa cada câmera a todos os outros. Exibição: cada câmera numa janelinha do mpv
//! (canto superior direito, sempre visível, pode ser arrastada), inclusive a sua própria (espelho).
//! Desligada por padrão; só liga quando a pessoa pede.

use std::{
    process::Stdio,
    sync::{LazyLock, Mutex},
};

use anyhow::{Context, Result};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::{Child, ChildStdin, Command},
};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Lado das janelinhas de câmera (pixels).
pub const WIN_W: u32 = 320;
pub const WIN_H: u32 = 240;

/// Teste sem câmera física: GAMESTREAM_CAM_TEST=1 usa uma imagem gerada.
fn test_source() -> bool {
    std::env::var("GAMESTREAM_CAM_TEST").is_ok_and(|v| !v.is_empty() && v != "0")
}

/// Trecho de H.264 (Annex B) que começa um quadro-chave (tem SPS, NAL tipo 7).
pub fn has_sps(b: &[u8]) -> bool {
    let mut i = 0;
    while i + 3 < b.len() {
        if b[i] == 0 && b[i + 1] == 0 && b[i + 2] == 1 && b[i + 3] & 0x1f == 7 {
            return true;
        }
        i += 1;
    }
    false
}

/// Câmeras disponíveis (nomes do DirectShow). Vazio se não houver FFmpeg ainda.
pub fn list(ffmpeg: &str) -> Vec<String> {
    if test_source() {
        return vec!["Câmera de teste".into()];
    }
    use std::os::windows::process::CommandExt;
    let Ok(out) = std::process::Command::new(ffmpeg)
        .args(["-hide_banner", "-list_devices", "true", "-f", "dshow", "-i", "dummy"])
        .stdin(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .output()
    else {
        return vec![];
    };
    String::from_utf8_lossy(&out.stderr)
        .lines()
        .filter(|l| l.contains("(video)"))
        .filter_map(|l| {
            let a = l.find('"')?;
            let b = l[a + 1..].find('"')? + a + 1;
            Some(l[a + 1..b].to_string())
        })
        .collect()
}

/// Inicia a captura da câmera; a saída padrão do processo é o vídeo H.264.
pub fn spawn_capture(ffmpeg: &str, device: &str) -> Result<Child> {
    let mut args: Vec<String> = vec!["-hide_banner".into(), "-loglevel".into(), "error".into(), "-nostdin".into()];
    if test_source() {
        args.extend(["-re", "-f", "lavfi", "-i", "testsrc2=size=320x240:rate=15"].map(String::from));
    } else {
        args.extend(["-f", "dshow", "-rtbufsize", "32M", "-i"].map(String::from));
        args.push(format!("video={device}"));
    }
    args.extend(
        [
            "-an",
            "-vf",
            "scale=-2:240:flags=fast_bilinear,fps=15,format=yuv420p",
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-tune",
            "zerolatency",
            "-profile:v",
            "baseline",
            "-g",
            "15",
            "-b:v",
            "350k",
            "-maxrate",
            "450k",
            "-bufsize",
            "300k",
            "-x264-params",
            "repeat-headers=1",
            "-f",
            "h264",
            "-",
        ]
        .map(String::from),
    );
    Command::new(ffmpeg)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .kill_on_drop(true)
        .spawn()
        .context(crate::i18n::t("não foi possível iniciar a câmera (FFmpeg)"))
}

/// Lê pedaços do vídeo da câmera (ou None quando a captura terminou).
pub async fn read_chunk(child: &mut Child, buf: &mut [u8]) -> Option<usize> {
    let out = child.stdout.as_mut()?;
    match out.read(buf).await {
        Ok(0) | Err(_) => None,
        Ok(n) => Some(n),
    }
}

/// Posições ocupadas pelas janelinhas (para empilhar no canto da tela).
static SLOTS: LazyLock<Mutex<Vec<bool>>> = LazyLock::new(|| Mutex::new(Vec::new()));

/// Janelinha de câmera (mpv). Fecha sozinha ao sair de escopo.
pub struct CamWindow {
    child: Child,
    stdin: Option<ChildStdin>,
    slot: usize,
}

impl CamWindow {
    pub fn open(name: &str) -> Result<CamWindow> {
        let slot = {
            let mut s = SLOTS.lock().unwrap();
            let i = s.iter().position(|used| !used).unwrap_or(s.len());
            if i == s.len() {
                s.push(true);
            } else {
                s[i] = true;
            }
            i
        };
        let title = crate::tr!("📷 {name}", "📷 {name}");
        let mut args = crate::viewer::mpv_args("cam", &title, None);
        // Vídeo da câmera é H.264 (o da transmissão é HEVC).
        for a in args.iter_mut() {
            if a == "--demuxer-lavf-format=hevc" {
                *a = "--demuxer-lavf-format=h264".into();
            }
        }
        let y = 16 + slot as u32 * (WIN_H + 12);
        let extra = [
            format!("--geometry={WIN_W}x{WIN_H}-16+{y}"),
            "--ontop".into(),
            "--border=no".into(),
            "--keepaspect-window=no".into(),
            format!("--script-opts-append=gs-name={}", name.replace([',', '='], " ")),
        ];
        let last = args.len() - 1; // o último argumento é "-" (ler do stdin)
        for (i, e) in extra.into_iter().enumerate() {
            args.insert(last + i, e);
        }
        let mpv = crate::viewer::find_mpv(None);
        let mut child = Command::new(&mpv)
            .args(args)
            .stdin(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .with_context(|| crate::tr!("não foi possível abrir a janela da câmera ({})", "could not open the camera window ({})", mpv.display()))?;
        let stdin = child.stdin.take();
        Ok(CamWindow { child, stdin, slot })
    }

    /// Envia vídeo para a janela. `false` se ela foi fechada (o usuário fechou a janelinha).
    pub async fn write(&mut self, data: &[u8]) -> bool {
        match self.stdin.as_mut() {
            Some(s) => s.write_all(data).await.is_ok(),
            None => false,
        }
    }

    pub fn closed(&mut self) -> bool {
        self.child.try_wait().ok().flatten().is_some()
    }
}

impl Drop for CamWindow {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
        if let Some(s) = SLOTS.lock().unwrap().get_mut(self.slot) {
            *s = false;
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn sps() {
        assert!(super::has_sps(&[0, 0, 0, 1, 0x67, 0x42]));
        assert!(!super::has_sps(&[0, 0, 0, 1, 0x65, 0x88]));
    }
}
