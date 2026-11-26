//! Atualização automática (pelas releases do GitHub) e download do FFmpeg sob demanda.
//! O pacote principal não leva o FFmpeg (~140 MB): só quem transmite precisa dele, e ele é baixado
//! (comprimido, ~90 MB) na primeira transmissão. Todo download é conferido por SHA-256.

use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{LazyLock, Mutex},
    time::Duration,
};

use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};

use crate::state::log;

pub const REPO: &str = "hnl09/gamestream";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Clone, Debug, Default, PartialEq)]
pub enum UpdateState {
    #[default]
    Idle,
    Checking,
    UpToDate,
    Available(Release),
    /// (o que está baixando, 0..1)
    Downloading(String, f32),
    Error(String),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Release {
    pub tag: String,
    exe_url: String,
    exe_sha_url: Option<String>,
}

pub static UPDATE: LazyLock<Mutex<UpdateState>> = LazyLock::new(|| Mutex::new(UpdateState::Idle));
/// Progresso do download do FFmpeg (mostrado na tela de "Preparando").
pub static FFMPEG_PROGRESS: LazyLock<Mutex<Option<f32>>> = LazyLock::new(|| Mutex::new(None));

/// Endereços (dá para trocar por variáveis de ambiente para testar com um servidor local).
fn releases_api() -> String {
    std::env::var("GAMESTREAM_RELEASES_API")
        .unwrap_or_else(|_| format!("https://api.github.com/repos/{REPO}/releases/latest"))
}

fn download_base() -> (String, String) {
    match std::env::var("GAMESTREAM_DOWNLOAD_BASE") {
        Ok(b) => (b.clone(), b),
        Err(_) => (
            format!("https://github.com/{REPO}/releases/download/v{VERSION}"),
            format!("https://github.com/{REPO}/releases/latest/download"),
        ),
    }
}

fn set_state(s: UpdateState) {
    *UPDATE.lock().unwrap() = s;
}

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(10))
        .timeout_read(Duration::from_secs(30))
        .user_agent(&format!("GameStream/{VERSION}"))
        .build()
}

fn parse_version(v: &str) -> (u32, u32, u32) {
    let mut it = v.trim_start_matches('v').split('.').map(|p| p.parse::<u32>().unwrap_or(0));
    (it.next().unwrap_or(0), it.next().unwrap_or(0), it.next().unwrap_or(0))
}

/// Consulta a última release. `Ok(None)` = já está na versão mais nova.
pub fn check() -> Result<Option<Release>> {
    let url = releases_api();
    let body = match agent().get(&url).set("Accept", "application/vnd.github+json").call() {
        Ok(r) => r.into_string()?,
        Err(ureq::Error::Status(404, _)) => {
            bail!("nenhuma release pública encontrada (o repositório precisa ser público para atualizar sozinho)")
        }
        Err(e) => return Err(e).context("sem conexão com o GitHub"),
    };
    let v: serde_json::Value = serde_json::from_str(&body)?;
    let tag = v["tag_name"].as_str().context("resposta inesperada do GitHub")?.to_string();
    if parse_version(&tag) <= parse_version(VERSION) {
        return Ok(None);
    }
    let assets = v["assets"].as_array().cloned().unwrap_or_default();
    let find = |name: &str| {
        assets
            .iter()
            .find(|a| a["name"].as_str() == Some(name))
            .and_then(|a| a["browser_download_url"].as_str())
            .map(|s| s.to_string())
    };
    let exe_url = find("gamestream.exe").context("a release nova não tem o gamestream.exe")?;
    Ok(Some(Release { tag, exe_url, exe_sha_url: find("gamestream.exe.sha256") }))
}

/// Verifica atualização em segundo plano (chamado ao abrir o app).
pub fn check_in_background() {
    std::thread::spawn(|| {
        set_state(UpdateState::Checking);
        match check() {
            Ok(Some(r)) => {
                log(format!("Atualização disponível: {} (você tem v{VERSION}).", r.tag));
                set_state(UpdateState::Available(r));
            }
            Ok(None) => set_state(UpdateState::UpToDate),
            Err(e) => {
                log(format!("[atualização] {e:#}"));
                set_state(UpdateState::Error(format!("{e:#}")));
            }
        }
    });
}

/// Baixa `url` reportando o progresso (0..1).
fn download(url: &str, mut progress: impl FnMut(f32)) -> Result<Vec<u8>> {
    let resp = agent().get(url).call().with_context(|| format!("falha ao baixar {url}"))?;
    let total: usize = resp.header("Content-Length").and_then(|v| v.parse().ok()).unwrap_or(0);
    let mut reader = resp.into_reader();
    let mut out = Vec::with_capacity(total);
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        out.extend_from_slice(&buf[..n]);
        if total > 0 {
            progress(out.len() as f32 / total as f32);
        }
    }
    Ok(out)
}

fn sha256_hex(data: &[u8]) -> String {
    Sha256::digest(data).iter().map(|b| format!("{b:02x}")).collect()
}

fn verify(data: &[u8], sha_url: Option<&str>) -> Result<()> {
    let Some(url) = sha_url else {
        log("[atualização] release sem arquivo .sha256; pulando verificação");
        return Ok(());
    };
    let expected = String::from_utf8(download(url, |_| {})?)?;
    let expected = expected.split_whitespace().next().unwrap_or("").to_lowercase();
    let got = sha256_hex(data);
    if expected != got {
        bail!("o arquivo baixado não confere (SHA-256 diferente). Atualização cancelada por segurança.");
    }
    Ok(())
}

fn exe_dir() -> Result<PathBuf> {
    Ok(std::env::current_exe()?.parent().context("pasta do programa")?.to_path_buf())
}

/// Apaga o executável antigo que ficou da última atualização.
pub fn cleanup_old() {
    if let Ok(dir) = exe_dir() {
        let _ = std::fs::remove_file(dir.join("gamestream.old.exe"));
    }
}

/// Baixa a versão nova e troca o executável (o app novo vale a partir da próxima abertura).
pub fn install(rel: &Release) -> Result<PathBuf> {
    set_state(UpdateState::Downloading(rel.tag.clone(), 0.0));
    let tag = rel.tag.clone();
    let data = download(&rel.exe_url, |p| set_state(UpdateState::Downloading(tag.clone(), p)))?;
    verify(&data, rel.exe_sha_url.as_deref())?;
    let cur = std::env::current_exe()?;
    let dir = exe_dir()?;
    let new = dir.join("gamestream.new.exe");
    std::fs::write(&new, &data).context("sem permissão para gravar na pasta do GameStream")?;
    // O Windows deixa renomear um .exe em uso (mas não apagar).
    let old = dir.join("gamestream.old.exe");
    let _ = std::fs::remove_file(&old);
    std::fs::rename(&cur, &old)?;
    if let Err(e) = std::fs::rename(&new, &cur) {
        let _ = std::fs::rename(&old, &cur); // desfaz
        return Err(e.into());
    }
    log(format!("Atualizado para {}.", rel.tag));
    Ok(cur)
}

/// Atualiza e reinicia o app (botão da interface).
pub fn apply(rel: Release) {
    std::thread::spawn(move || match install(&rel) {
        Ok(cur) => {
            let _ = std::process::Command::new(&cur).spawn();
            std::process::exit(0);
        }
        Err(e) => {
            log(format!("[atualização] {e:#}"));
            set_state(UpdateState::Error(format!("{e:#}")));
        }
    });
}

/// O `ffmpeg` funciona a partir deste caminho/comando?
fn ffmpeg_works(cmd: &Path) -> bool {
    use std::os::windows::process::CommandExt;
    std::process::Command::new(cmd)
        .arg("-version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .creation_flags(0x0800_0000)
        .status()
        .is_ok_and(|s| s.success())
}

/// Garante um ffmpeg.exe: usa o da pasta do GameStream, o do PATH, ou baixa da release desta versão.
pub fn ensure_ffmpeg() -> Result<String> {
    let dir = exe_dir()?;
    let local = dir.join("ffmpeg.exe");
    if local.exists() && ffmpeg_works(&local) {
        return Ok(local.to_string_lossy().into_owned());
    }
    if ffmpeg_works(Path::new("ffmpeg")) {
        return Ok("ffmpeg".into());
    }
    log("Baixando o encoder de vídeo (FFmpeg, ~90 MB). Só acontece na primeira transmissão...");
    *FFMPEG_PROGRESS.lock().unwrap() = Some(0.0);
    let r = (|| -> Result<String> {
        let (base, latest) = download_base();
        let gz = download(&format!("{base}/ffmpeg.exe.gz"), |p| *FFMPEG_PROGRESS.lock().unwrap() = Some(p))
            .or_else(|_| {
                // Versão sem release (ex.: compilada localmente): tenta a última release.
                download(&format!("{latest}/ffmpeg.exe.gz"), |p| *FFMPEG_PROGRESS.lock().unwrap() = Some(p))
            })
            .context("não consegui baixar o FFmpeg (o repositório do GameStream está público?)")?;
        let sha = download(&format!("{base}/ffmpeg.exe.gz.sha256"), |_| {})
            .or_else(|_| download(&format!("{latest}/ffmpeg.exe.gz.sha256"), |_| {}));
        if let Ok(sha) = sha {
            let expected = String::from_utf8_lossy(&sha).split_whitespace().next().unwrap_or("").to_lowercase();
            if expected != sha256_hex(&gz) {
                bail!("o FFmpeg baixado não confere (SHA-256 diferente)");
            }
        }
        let mut exe = Vec::new();
        flate2::read::GzDecoder::new(&gz[..]).read_to_end(&mut exe).context("arquivo do FFmpeg corrompido")?;
        let tmp = dir.join("ffmpeg.exe.part");
        std::fs::File::create(&tmp)
            .and_then(|mut f| f.write_all(&exe))
            .context("sem permissão para gravar na pasta do GameStream")?;
        std::fs::rename(&tmp, &local)?;
        log("FFmpeg instalado.");
        Ok(local.to_string_lossy().into_owned())
    })();
    *FFMPEG_PROGRESS.lock().unwrap() = None;
    r
}
