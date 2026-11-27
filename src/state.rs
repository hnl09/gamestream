//! Estado compartilhado entre o backend (tokio) e a interface (egui).

use std::{
    collections::HashMap,
    sync::{Arc, LazyLock, Mutex},
};

/// Alguém na transmissão (transmissor ou espectador), como todos veem na lista "Assistindo agora".
#[derive(Default, Clone, PartialEq, Debug)]
pub struct Member {
    pub id: String,
    pub name: String,
    pub host: bool,
    /// Controle remoto liberado (bits CTL_PAD | CTL_KBM): mostra quem está jogando.
    pub ctl: u8,
}

#[derive(Default, Clone)]
pub struct HostStatus {
    pub encoder: String,
    /// Código curto derivado do certificado do host (para conferir por voz com quem assiste).
    pub security_code: String,
    pub mbps: f64,
    /// Bitrate alvo atual do encoder (muda sozinho no modo automático).
    pub bitrate: u32,
    /// Espectadores conectados, com as estatísticas de cada um.
    pub viewers: Vec<ViewerRow>,
    /// Sala de espera: (id, "nome (endereço)").
    pub pending: Vec<(u64, String)>,
    /// Chat e reações da transmissão (mais antigos primeiro).
    pub chat: Vec<crate::control::ChatMsg>,
    /// Avatares (PNG) por id ("h" = você).
    pub avatars: HashMap<String, Arc<Vec<u8>>>,
    pub invite: Option<String>,
    /// Endereços pelos quais os espectadores podem se conectar (LAN, IPv6, UPnP...).
    pub reach: Vec<String>,
    /// Último erro (mostrado em destaque na interface).
    pub error: Option<String>,
    /// A janela de prévia (o host vendo a própria transmissão) está aberta.
    pub preview_open: bool,
    /// Código da sala no servidor de encontro (ex.: "KX7-92B") e se ela está conectada.
    pub room: Option<String>,
    pub room_online: bool,
}

/// Um espectador conectado, visto pelo transmissor.
#[derive(Default, Clone, PartialEq)]
pub struct ViewerRow {
    /// Chave para remover/dar controle (endereço completo).
    pub key: String,
    /// Id na lista de quem está assistindo (para o avatar).
    pub id: String,
    pub name: String,
    pub ip: String,
    pub ping_ms: u32,
    /// Perda de pacotes (%) nos últimos 2 segundos.
    pub loss: f64,
    /// Mbps enviados a este espectador.
    pub mbps: f64,
    /// Controle remoto pedido pelo espectador e liberado pelo transmissor (bits CTL_PAD/CTL_KBM).
    pub wants: u8,
    pub granted: u8,
    /// Versão do GameStream do espectador (vazio = versão antiga, sem chat/controle).
    pub version: String,
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
    /// Código de resposta (GS-R-...) exibido quando a conexão direta não responde.
    pub reply_code: Option<String>,
    /// Nome do transmissor (informado por ele ao conectar).
    pub host_name: String,
    /// Chat e reações da transmissão.
    pub chat: Vec<crate::control::ChatMsg>,
    /// A conexão caiu e o app está tentando voltar sozinho (texto com a tentativa atual).
    pub reconnecting: Option<String>,
    /// Controle remoto: pedido feito por este espectador e o que o transmissor liberou (bits).
    pub ctl_asked: u8,
    pub ctl_granted: u8,
    /// Modo de controle por teclado e mouse ligado (F8).
    pub kbm_active: bool,
    /// Controle (gamepad) detectado neste PC.
    pub pad_found: bool,
    /// O transmissor tem uma versão com chat e controle remoto.
    pub has_control: bool,
    /// Quem está na transmissão (o transmissor primeiro) e os avatares por id.
    pub roster: Vec<Member>,
    pub avatars: HashMap<String, Arc<Vec<u8>>>,
    /// O seu id na lista.
    pub my_id: String,
    /// Versão do GameStream do transmissor; Some("") = versão antiga (sem chat/controle).
    pub host_version: Option<String>,
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
    /// Nível do áudio recebido (dBFS) no último segundo; None = nenhum pacote de áudio chegou.
    pub audio_db: Option<f32>,
}

impl ViewStats {
    pub fn line(&self) -> String {
        crate::tr!("Ping {} ms  ·  {} FPS  ·  Perda {:.1}%  ·  {:.1} Mbps", "Ping {} ms  ·  {} FPS  ·  Loss {:.1}%  ·  {:.1} Mbps",
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

/// Guarda uma mensagem de chat (no máximo as últimas 200).
pub fn push_chat(list: &mut Vec<crate::control::ChatMsg>, m: crate::control::ChatMsg) {
    list.push(m);
    if list.len() > 200 {
        list.drain(..list.len() - 200);
    }
}

pub fn view_phase(phase: ViewPhase, text: impl Into<String>) {
    view(|v| {
        v.phase = phase;
        v.text = text.into();
    });
}
