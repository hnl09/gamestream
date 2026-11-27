//! Preferências do usuário (idioma e tema), salvas em %APPDATA%\GameStream\settings.json.

use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicU8, Ordering},
        Once,
    },
};

static EN: AtomicBool = AtomicBool::new(false);
static THEME: AtomicU8 = AtomicU8::new(0);
static INIT: Once = Once::new();

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
    });
}

fn save() {
    let v = serde_json::json!({ "lang": if is_en() { "en" } else { "pt" }, "theme": theme().id() });
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
