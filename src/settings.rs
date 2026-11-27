//! Preferências do usuário (idioma, tema, nome e salas recentes), salvas em
//! %APPDATA%\GameStream\settings.json.

use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicU8, Ordering},
        LazyLock, Mutex, Once,
    },
};

static EN: AtomicBool = AtomicBool::new(false);
static THEME: AtomicU8 = AtomicU8::new(0);
static INIT: Once = Once::new();

/// Sala em que o usuário já entrou (para reconectar com um clique).
#[derive(Clone, Debug, PartialEq)]
pub struct Recent {
    /// Código da sala (ex.: "KX792B", sem hífen).
    pub code: String,
    /// Nome do transmissor (ou o endereço, se ele não informou um nome).
    pub name: String,
    /// Última vez (segundos desde 1970).
    pub last: u64,
}

#[derive(Default)]
struct Extra {
    nick: String,
    recent: Vec<Recent>,
    /// Últimas escolhas da interface (fonte, áudio, qualidade...). Nunca inclui a senha.
    prefs: serde_json::Value,
}

static EXTRA: LazyLock<Mutex<Extra>> = LazyLock::new(|| Mutex::new(Extra::default()));
const MAX_RECENT: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Theme {
    Default,
    Agent,
    Biohazard,
}

impl Theme {
    pub const ALL: [Theme; 3] = [Theme::Default, Theme::Agent, Theme::Biohazard];

    pub fn id(self) -> &'static str {
        match self {
            Theme::Default => "default",
            Theme::Agent => "agent",
            Theme::Biohazard => "biohazard",
        }
    }

    pub fn label(self) -> &'static str {
        match (self, is_en()) {
            (Theme::Default, false) => "Padrão",
            (Theme::Default, true) => "Default",
            (Theme::Agent, false) => "Agente",
            (Theme::Agent, true) => "Agent",
            (Theme::Biohazard, false) => "Bioameaça",
            (Theme::Biohazard, true) => "Biohazard",
        }
    }

    fn from_id(s: &str) -> Theme {
        Theme::ALL.into_iter().find(|t| t.id() == s).unwrap_or(Theme::Default)
    }
}

fn path() -> PathBuf {
    PathBuf::from(std::env::var("APPDATA").unwrap_or_else(|_| ".".into())).join("GameStream").join("settings.json")
}

/// Português se o idioma do Windows OU o formato regional for português (muitos usuários no Brasil
/// usam o Windows em inglês com a região "Português (Brasil)"); caso contrário, inglês.
fn system_is_pt() -> bool {
    const LANG_PORTUGUESE: u16 = 0x16;
    let (ui, region) = unsafe {
        (windows::Win32::Globalization::GetUserDefaultUILanguage(), windows::Win32::Globalization::GetUserDefaultLangID())
    };
    (ui & 0x3ff) == LANG_PORTUGUESE || (region & 0x3ff) == LANG_PORTUGUESE
}

fn init() {
    INIT.call_once(|| {
        let saved: serde_json::Value = std::fs::read_to_string(path())
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        // Quem já usava o GameStream (versões anteriores eram só em português) continua em português;
        // instalações novas seguem o idioma/região do Windows.
        let existing_user = path().with_file_name("host-cert.der").exists();
        let en = match saved["lang"].as_str() {
            Some("en") => true,
            Some("pt") => false,
            _ => !(existing_user || system_is_pt()),
        };
        EN.store(en, Ordering::Relaxed);
        let theme = Theme::from_id(saved["theme"].as_str().unwrap_or("default"));
        THEME.store(Theme::ALL.iter().position(|t| *t == theme).unwrap_or(0) as u8, Ordering::Relaxed);
        let mut x = EXTRA.lock().unwrap();
        x.nick = crate::control::clean_name(saved["nick"].as_str().unwrap_or(""));
        x.prefs = saved["prefs"].clone();
        x.recent = saved["recent"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|r| {
                        Some(Recent {
                            code: r["code"].as_str()?.to_string(),
                            name: r["name"].as_str().unwrap_or("").to_string(),
                            last: r["last"].as_u64().unwrap_or(0),
                        })
                    })
                    .take(MAX_RECENT)
                    .collect()
            })
            .unwrap_or_default();
    });
}

fn save() {
    let (nick, recent, prefs) = {
        let x = EXTRA.lock().unwrap();
        let recent: Vec<serde_json::Value> = x
            .recent
            .iter()
            .map(|r| serde_json::json!({ "code": r.code, "name": r.name, "last": r.last }))
            .collect();
        (x.nick.clone(), recent, x.prefs.clone())
    };
    let v = serde_json::json!({
        "lang": if is_en() { "en" } else { "pt" },
        "theme": theme().id(),
        "nick": nick,
        "recent": recent,
        "prefs": prefs,
    });
    let p = path();
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(p, serde_json::to_string_pretty(&v).unwrap_or_default());
}

pub fn is_en() -> bool {
    init();
    EN.load(Ordering::Relaxed)
}

pub fn set_en(en: bool) {
    init();
    EN.store(en, Ordering::Relaxed);
    save();
}

pub fn theme() -> Theme {
    init();
    Theme::ALL[THEME.load(Ordering::Relaxed) as usize % Theme::ALL.len()]
}

pub fn set_theme(t: Theme) {
    init();
    THEME.store(Theme::ALL.iter().position(|x| *x == t).unwrap_or(0) as u8, Ordering::Relaxed);
    save();
}

/// Nome exibido no chat e para quem você transmite. Padrão: o nome do usuário do Windows.
pub fn nick() -> String {
    init();
    let n = EXTRA.lock().unwrap().nick.clone();
    if !n.is_empty() {
        return n;
    }
    let user = crate::control::clean_name(&std::env::var("USERNAME").unwrap_or_default());
    if user.is_empty() { crate::i18n::t("Jogador").to_string() } else { user }
}

/// Nome digitado pelo usuário (vazio = usar o padrão).
pub fn nick_raw() -> String {
    init();
    EXTRA.lock().unwrap().nick.clone()
}

pub fn set_nick(n: &str) {
    init();
    let n = crate::control::clean_name(n);
    {
        let mut x = EXTRA.lock().unwrap();
        if x.nick == n {
            return;
        }
        x.nick = n;
    }
    save();
}

/// Últimas escolhas da interface (JSON livre; Null se nunca salvas).
pub fn prefs() -> serde_json::Value {
    init();
    EXTRA.lock().unwrap().prefs.clone()
}

pub fn set_prefs(v: serde_json::Value) {
    init();
    {
        let mut x = EXTRA.lock().unwrap();
        if x.prefs == v {
            return;
        }
        x.prefs = v;
    }
    save();
}

pub fn recent() -> Vec<Recent> {
    init();
    EXTRA.lock().unwrap().recent.clone()
}

/// Registra (ou atualiza) uma sala recente; a mais nova fica no topo.
pub fn add_recent(code: &str, name: &str) {
    init();
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    {
        let mut x = EXTRA.lock().unwrap();
        let old_name = x.recent.iter().find(|r| r.code == code).map(|r| r.name.clone());
        x.recent.retain(|r| r.code != code);
        let name = if name.is_empty() { old_name.unwrap_or_default() } else { name.to_string() };
        x.recent.insert(0, Recent { code: code.to_string(), name, last: now });
        x.recent.truncate(MAX_RECENT);
    }
    save();
}

pub fn remove_recent(code: &str) {
    init();
    EXTRA.lock().unwrap().recent.retain(|r| r.code != code);
    save();
    let _ = std::fs::remove_file(recent_avatar_path(code));
}

fn recent_avatar_path(code: &str) -> PathBuf {
    let safe: String = code.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
    path().with_file_name("recent").join(format!("{safe}.png"))
}

/// Guarda a foto do transmissor de uma sala recente (para mostrar na lista).
pub fn save_recent_avatar(code: &str, png: &[u8]) {
    let p = recent_avatar_path(code);
    if std::fs::read(&p).ok().as_deref() == Some(png) {
        return;
    }
    if let Some(d) = p.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let _ = std::fs::write(p, png);
}

pub fn recent_avatar(code: &str) -> Option<std::sync::Arc<Vec<u8>>> {
    let png = std::fs::read(recent_avatar_path(code)).ok()?;
    crate::avatar::valid(&png).then(|| std::sync::Arc::new(png))
}
