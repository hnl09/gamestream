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
    /// Linhas descrevendo por onde os amigos conseguem chegar (LAN, IPv6, UPnP...).
    pub reach: Vec<String>,
}

#[derive(Default, Clone)]
pub struct ViewStatus {
    pub text: String,
}

pub static LOG: LazyLock<Mutex<Vec<String>>> = LazyLock::new(|| Mutex::new(Vec::new()));
pub static HOST: LazyLock<Mutex<HostStatus>> = LazyLock::new(|| Mutex::new(HostStatus::default()));
pub static VIEW: LazyLock<Mutex<ViewStatus>> = LazyLock::new(|| Mutex::new(ViewStatus::default()));

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

pub fn view_status(text: impl Into<String>) {
    VIEW.lock().unwrap().text = text.into();
}
