//! Webcam de quem quiser (transmissor e espectadores), como numa chamada de vídeo do Discord.
//!
//! Captura: FFmpeg lê a câmera (DirectShow) e comprime em H.264 pequeno (até 240 linhas, 15 fps,
//! ~0,35 Mbps). Rede: o espectador envia a câmera dele ao transmissor (stream QUIC STREAM_CAM) e o
//! transmissor repassa cada câmera a todos os outros. Exibição: cada câmera numa janelinha do mpv
//! (sempre visível), inclusive a sua própria (espelho). As janelinhas ficam numa grade invisível
//! (ver `camgrid`): podem ser arrastadas e redimensionadas com a roda do mouse, sem se sobrepor.
//! Desligada por padrão; só liga quando a pessoa pede.

use std::{
    collections::BTreeSet,
    process::Stdio,
    sync::{LazyLock, Mutex},
};

use anyhow::{Context, Result};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::{Child, ChildStdin, Command},
};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

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

/// Câmeras que você desativou ("Desativar vídeo", como no Discord): somem só para você.
/// Valem até fechar o app; o card "📷 Câmera" lista e permite mostrar de novo.
static HIDDEN: LazyLock<Mutex<BTreeSet<String>>> = LazyLock::new(|| Mutex::new(BTreeSet::new()));

pub fn hide(name: &str) {
    if HIDDEN.lock().unwrap().insert(name.to_string()) {
        crate::state::log(crate::tr!("[câmera] vídeo de {name} desativado (só para você).", "[camera] {name}'s video turned off (only for you)."));
    }
}

pub fn unhide(name: &str) {
    HIDDEN.lock().unwrap().remove(name);
}

pub fn is_hidden(name: &str) -> bool {
    HIDDEN.lock().unwrap().contains(name)
}

pub fn hidden_list() -> Vec<String> {
    HIDDEN.lock().unwrap().iter().cloned().collect()
}

/// Exibe a câmera de alguém respeitando o "Desativar vídeo": sem janela enquanto estiver
/// desativada, e reabre (a partir do próximo quadro-chave) quando a pessoa pedir para mostrar.
pub struct CamView {
    name: String,
    win: Option<CamWindow>,
    waiting_key: bool,
}

impl CamView {
    pub fn new(name: &str) -> CamView {
        CamView { name: name.to_string(), win: None, waiting_key: true }
    }

    /// Entrega um pedaço do vídeo (H.264). Abre/fecha a janelinha conforme o estado.
    pub async fn feed(&mut self, data: &[u8], key: bool) {
        if is_hidden(&self.name) {
            self.win = None;
            self.waiting_key = true;
            return;
        }
        if self.waiting_key {
            if !key {
                return;
            }
            self.waiting_key = false;
        }
        if self.win.is_none() {
            match CamWindow::open(&self.name) {
                Ok(w) => self.win = Some(w),
                Err(_) => {
                    // Sem player: não adianta tentar a cada quadro.
                    hide(&self.name);
                    return;
                }
            }
        }
        let w = self.win.as_mut().unwrap();
        if !w.write(data).await || w.closed() {
            // Fechou a janelinha: é o mesmo que "Desativar vídeo".
            self.win = None;
            self.waiting_key = true;
            hide(&self.name);
        }
    }

    /// Perdeu pedaços do vídeo: espera o próximo quadro-chave (sem fechar a janela).
    pub fn resync(&mut self) {
        self.waiting_key = true;
    }

    pub fn showing(&self) -> bool {
        self.win.is_some()
    }
}

/// Janelinha de câmera (mpv). Fecha sozinha ao sair de escopo.
pub struct CamWindow {
    child: Child,
    stdin: Option<ChildStdin>,
    grid_id: u64,
    ipc: Option<tokio::task::JoinHandle<()>>,
}

impl CamWindow {
    pub fn open(name: &str) -> Result<CamWindow> {
        let (grid_id, geometry) = crate::camgrid::reserve();
        let title = crate::tr!("📷 {name}", "📷 {name}");
        let pipe = format!(r"\\.\pipe\gamestream-cam-{}-{grid_id}", std::process::id());
        let mut args = crate::viewer::mpv_args("cam", &title, Some(&pipe));
        // Vídeo da câmera é H.264 (o da transmissão é HEVC).
        for a in args.iter_mut() {
            if a == "--demuxer-lavf-format=hevc" {
                *a = "--demuxer-lavf-format=h264".into();
            }
        }
        let extra = [
            format!("--geometry={}", geometry.unwrap_or_else(|| "312x232-16+16".into())),
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
        let child = Command::new(&mpv)
            .args(args)
            .stdin(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .with_context(|| crate::tr!("não foi possível abrir a janela da câmera ({})", "could not open the camera window ({})", mpv.display()));
        let mut child = match child {
            Ok(c) => c,
            Err(e) => {
                crate::camgrid::release(grid_id);
                return Err(e);
            }
        };
        if let Some(pid) = child.id() {
            crate::camgrid::attach(grid_id, pid);
        }
        let stdin = child.stdin.take();
        let ipc = Some(tokio::spawn(window_requests(pipe, grid_id, name.to_string())));
        Ok(CamWindow { child, stdin, grid_id, ipc })
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
        if let Some(t) = self.ipc.take() {
            t.abort();
        }
        crate::camgrid::release(self.grid_id);
    }
}

/// Pedidos da janelinha pela IPC do mpv: roda do mouse ("gs-cam-size +1/-1") e o menu do botão
/// direito ("gs-cam-hide" = Desativar vídeo).
async fn window_requests(pipe: String, grid_id: u64, name: String) {
    use tokio::io::AsyncBufReadExt;
    let Some(client) = crate::viewer::open_ipc(&pipe).await else { return };
    let mut lines = tokio::io::BufReader::new(client).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else { continue };
        if v["event"] != "client-message" {
            continue;
        }
        if v["args"][0] == "gs-cam-hide" {
            hide(&name);
        } else if v["args"][0] == "gs-cam-size" {
            if let Some(d) = v["args"][1].as_str().and_then(|s| s.parse::<i32>().ok()) {
                crate::camgrid::resize(grid_id, d);
            }
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
