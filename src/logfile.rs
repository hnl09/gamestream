//! Arquivo de log em %APPDATA%\GameStream\logs (para investigar problemas depois).
//!
//! Tudo que aparece no "Log" do app vai também para logs\gamestream.log, com data e hora, mais um
//! resumo por minuto enquanto transmite ou assiste (bitrate, ping, perda, FPS). Nunca passa de
//! ~10 MB: quando o arquivo chega a 5 MB vira gamestream.1.log (o mais antigo é apagado).
//! O texto das mensagens do chat não é gravado.

use std::{
    fs::{File, OpenOptions},
    io::Write,
    path::PathBuf,
    sync::{LazyLock, Mutex},
};

/// Tamanho de cada arquivo; com o anterior guardado, no máximo ~10 MB no total.
const MAX_FILE: u64 = 5 * 1024 * 1024;

static FILE: LazyLock<Mutex<Option<(File, u64)>>> = LazyLock::new(|| Mutex::new(None));

pub fn dir() -> PathBuf {
    PathBuf::from(std::env::var("APPDATA").unwrap_or_else(|_| ".".into())).join("GameStream").join("logs")
}

fn current() -> PathBuf {
    dir().join("gamestream.log")
}

fn open() -> Option<(File, u64)> {
    let _ = std::fs::create_dir_all(dir());
    let f = OpenOptions::new().create(true).append(true).open(current()).ok()?;
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    Some((f, len))
}

fn timestamp() -> String {
    let t = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
    format!("{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03}", t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond, t.wMilliseconds)
}

/// Grava uma linha (com data e hora). Mensagens de várias linhas ficam indentadas.
pub fn write(msg: &str) {
    // Conversas do chat são pessoais: só fica registrado que houve uma mensagem.
    let msg = if msg.starts_with("[chat]") { "[chat] (mensagem)" } else { msg };
    let mut g = FILE.lock().unwrap();
    if g.is_none() {
        *g = open();
    }
    if g.as_ref().is_some_and(|(_, len)| *len >= MAX_FILE) {
        // Gira: o atual vira o ".1" (o ".1" anterior é apagado) e começa um arquivo novo.
        *g = None;
        let old = dir().join("gamestream.1.log");
        let _ = std::fs::remove_file(&old);
        let _ = std::fs::rename(current(), &old);
        *g = open();
    }
    let Some((f, len)) = g.as_mut() else { return };
    let line = format!("{} {}\n", timestamp(), msg.replace('\n', "\n                        "));
    if f.write_all(line.as_bytes()).is_ok() {
        *len += line.len() as u64;
    }
}

/// Início de uma execução: versão e modo (interface, transmitir pelo terminal...).
pub fn session_start(mode: &str) {
    write(&format!(
        "===== GameStream {} ({mode}) · Windows {} =====",
        env!("CARGO_PKG_VERSION"),
        windows_version()
    ));
    // Travamentos inesperados também ficam registrados.
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        write(&format!("[ERRO GRAVE] {info}"));
        default(info);
    }));
}

fn windows_version() -> String {
    // Número do build do Windows (ex.: 26100), lido do registro.
    use std::os::windows::process::CommandExt;
    std::process::Command::new("reg")
        .args(["query", r"HKLM\SOFTWARE\Microsoft\Windows NT\CurrentVersion", "/v", "CurrentBuild"])
        .creation_flags(0x0800_0000)
        .output()
        .ok()
        .and_then(|o| String::from_utf8_lossy(&o.stdout).split_whitespace().last().map(String::from))
        .unwrap_or_else(|| "?".into())
}

#[cfg(test)]
mod tests {
    #[test]
    fn rotates_and_stays_small() {
        let tmp = std::env::temp_dir().join(format!("gs-logtest-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::env::set_var("APPDATA", &tmp);
        let big = "x".repeat(64 * 1024);
        for _ in 0..200 {
            super::write(&big); // ~12,8 MB escritos
        }
        super::write("[chat] Ana: segredo");
        let cur = std::fs::metadata(super::current()).unwrap().len();
        let old = std::fs::metadata(super::dir().join("gamestream.1.log")).unwrap().len();
        assert!(cur <= super::MAX_FILE + 70_000 && old <= super::MAX_FILE + 70_000, "{cur} {old}");
        let text = std::fs::read_to_string(super::current()).unwrap();
        assert!(text.contains("[chat] (mensagem)") && !text.contains("segredo"));
        *super::FILE.lock().unwrap() = None;
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
