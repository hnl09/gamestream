//! Estado compartilhado entre o backend (tokio) e a interface (egui).

use std::sync::{LazyLock, Mutex};

#[derive(Default, Clone)]
pub struct HostStatus {
    pub encoder: String,
    /// Código curto derivado do certificado do host (para conferir por voz com quem assiste).
    pub security_code: String,
    pub mbps: f64,
    /// Bitrate alvo atual do encoder (muda sozinho no modo automático).
    pub bitrate: u32,
    pub viewers: Vec<String>,
    pub invite: Option<String>,
    /// Endereços pelos quais os espectadores podem se conectar (LAN, IPv6, UPnP...).
    pub reach: Vec<String>,
    /// Último erro (mostrado em destaque na interface).
    pub error: Option<String>,
    /// A janela de prévia (o host vendo a própria transmissão) está aberta.
    pub preview_open: bool,
}

#[derive(Default, Clone, Copy, PartialEq)]
pub enum ViewPhase {
    #[default]
    Idle,
    Connecting,
    Connected,
    Error,
}

#[derive(Default, Clone)]
pub struct ViewStatus {
    pub phase: ViewPhase,
    pub text: String,
    pub peer: String,
    pub code: String,
    pub verified: bool,
    pub stats: ViewStats,
}

/// Números da transmissão para quem assiste (atualizados a cada segundo).
#[derive(Default, Clone, PartialEq)]
pub struct ViewStats {
    pub ping_ms: u32,
    pub fps: u32,
    /// Perda de pacotes medida pelo host (%).
    pub loss: f64,
    /// Mbps chegando.
    pub mbps: f64,
    /// Bitrate alvo do host.
    pub target: u32,
}

impl ViewStats {
    pub fn line(&self) -> String {
        format!(
            "Ping {} ms  ·  {} FPS  ·  Perda {:.1}%  ·  {:.1} Mbps",
            self.ping_ms, self.fps, self.loss, self.mbps
        )
    }
}

/// Controles do player. Podem mudar tanto pela interface quanto pelo painel dentro da janela do vídeo;
/// o viewer mantém os dois lados sincronizados via IPC do mpv.
#[derive(Clone, PartialEq)]
pub struct ViewCtl {
    /// 1.0 = 100% (0..2).
    pub volume: f32,
    pub muted: bool,
    /// Incrementa a cada clique em "Tela cheia" na interface.
    pub fullscreen_toggles: u32,
    pub on_top: bool,
}

impl Default for ViewCtl {
    fn default() -> Self {
        ViewCtl { volume: 1.0, muted: false, fullscreen_toggles: 0, on_top: false }
    }
}

pub static LOG: LazyLock<Mutex<Vec<String>>> = LazyLock::new(|| Mutex::new(Vec::new()));
pub static HOST: LazyLock<Mutex<HostStatus>> = LazyLock::new(|| Mutex::new(HostStatus::default()));
pub static VIEW: LazyLock<Mutex<ViewStatus>> = LazyLock::new(|| Mutex::new(ViewStatus::default()));
pub static VIEW_CTL: LazyLock<Mutex<ViewCtl>> = LazyLock::new(|| Mutex::new(ViewCtl::default()));

pub fn log(msg: impl AsRef<str>) {
    let m = msg.as_ref();
    println!("{m}");
    let mut l = LOG.lock().unwrap();
    l.push(m.to_string());
    if l.len() > 500 {
        l.drain(..100);
    }
}

pub fn host<R>(f: impl FnOnce(&mut HostStatus) -> R) -> R {
    f(&mut HOST.lock().unwrap())
}

pub fn view<R>(f: impl FnOnce(&mut ViewStatus) -> R) -> R {
    f(&mut VIEW.lock().unwrap())
}

pub fn view_phase(phase: ViewPhase, text: impl Into<String>) {
    view(|v| {
        v.phase = phase;
        v.text = text.into();
    });
}
