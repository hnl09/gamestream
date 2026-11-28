//! Link para entrar com um clique.
//!
//! Quem transmite copia um link https (página do servidor de encontro, `/j/KX7-92B`) e manda no
//! Discord. A página abre `gamestream://join/KX792B`, que o Windows entrega ao GameStream (o app
//! registra esse tipo de link para o usuário atual, sem precisar de administrador). Se o GameStream
//! já estiver aberto, o código vai para a janela aberta por um named pipe, em vez de abrir outra.

use std::sync::{LazyLock, Mutex};

use tokio::io::AsyncBufReadExt;

/// Código de sala vindo de um link (entregue à interface na próxima atualização da tela).
pub static PENDING: LazyLock<Mutex<Option<String>>> = LazyLock::new(|| Mutex::new(None));

/// Código da sala em `gamestream://join/KX792B` (aceita variações como barra no fim).
pub fn code_from_url(arg: &str) -> Option<String> {
    let rest = arg.trim().strip_prefix("gamestream:").or_else(|| arg.trim().strip_prefix("GAMESTREAM:"))?;
    let path = rest.split(['?', '#']).next().unwrap_or("");
    path.split('/').filter(|s| !s.is_empty()).last().and_then(crate::room::parse_code)
}

/// Link para compartilhar (página https que abre o GameStream na sala).
pub fn web_link(code: &str) -> Option<String> {
    crate::room::server().map(|s| format!("{}/j/{}", s.trim_end_matches('/'), crate::room::display(code)))
}

/// Registra `gamestream://` para este usuário (HKCU), apontando para este executável.
/// Roda em segundo plano a cada abertura (corrige o caminho se o app mudar de pasta).
pub fn register_scheme() {
    std::thread::spawn(|| {
        use std::os::windows::process::CommandExt;
        let Ok(exe) = std::env::current_exe() else { return };
        let exe = exe.display().to_string();
        let key = r"HKCU\Software\Classes\gamestream";
        let reg = |args: &[&str]| {
            let _ = std::process::Command::new("reg").args(args).creation_flags(0x0800_0000).output();
        };
        reg(&["add", key, "/ve", "/d", "URL:GameStream", "/f"]);
        reg(&["add", key, "/v", "URL Protocol", "/d", "", "/f"]);
        reg(&["add", &format!(r"{key}\DefaultIcon"), "/ve", "/d", &format!("\"{exe}\",0"), "/f"]);
        reg(&["add", &format!(r"{key}\shell\open\command"), "/ve", "/d", &format!("\"{exe}\" \"%1\""), "/f"]);
    });
}

fn pipe_name() -> String {
    use std::hash::{Hash, Hasher};
    let user: String = std::env::var("USERNAME").unwrap_or_default().chars().filter(|c| c.is_ascii_alphanumeric()).collect();
    // Um GameStream por perfil (APPDATA): perfis/testes separados não se atrapalham.
    let mut h = std::collections::hash_map::DefaultHasher::new();
    std::env::var("APPDATA").unwrap_or_default().to_lowercase().hash(&mut h);
    format!(r"\\.\pipe\gamestream-ui-{user}-{:08x}", h.finish() as u32)
}

/// Manda o código para um GameStream já aberto. `true` se havia um aberto para receber.
pub fn forward_to_running(code: &str) -> bool {
    use std::io::Write;
    match std::fs::OpenOptions::new().write(true).open(pipe_name()) {
        Ok(mut f) => f.write_all(format!("{code}\n").as_bytes()).is_ok(),
        Err(_) => false,
    }
}

/// Recebe códigos de links abertos enquanto o app está aberto. Se outro GameStream já estiver
/// recebendo, este não recebe (o link vai para o primeiro).
pub async fn serve(ctx: eframe::egui::Context) {
    use tokio::net::windows::named_pipe::ServerOptions;
    let name = pipe_name();
    // Logo depois de uma atualização o app antigo pode ainda estar fechando: tenta por alguns segundos.
    let mut server = None;
    for _ in 0..20 {
        if let Ok(s) = ServerOptions::new().first_pipe_instance(true).create(&name) {
            server = Some(s);
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
    let Some(mut server) = server else { return };
    loop {
        if server.connect().await.is_err() {
            return;
        }
        let client = server;
        let Ok(next) = ServerOptions::new().create(&name) else { return };
        server = next;
        let mut line = String::new();
        let mut rd = tokio::io::BufReader::new(client);
        if tokio::time::timeout(std::time::Duration::from_secs(2), rd.read_line(&mut line)).await.is_ok() {
            if line.trim() == "show" {
                crate::tray::show_main();
            } else if let Some(code) = crate::room::parse_code(line.trim()) {
                *PENDING.lock().unwrap() = Some(code);
                crate::tray::show_main();
                ctx.request_repaint();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::code_from_url;

    #[test]
    fn urls() {
        assert_eq!(code_from_url("gamestream://join/KX792B").as_deref(), Some("KX792B"));
        assert_eq!(code_from_url("gamestream://join/kx7-92b/").as_deref(), Some("KX792B"));
        assert_eq!(code_from_url("gamestream:join/KX792B?x=1").as_deref(), Some("KX792B"));
        assert_eq!(code_from_url("https://evil/join/KX792B"), None);
        assert_eq!(code_from_url("gamestream://join/OOOOOO"), None);
    }
}
