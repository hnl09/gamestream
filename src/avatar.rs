//! Foto de perfil (avatar). Fica só no PC de cada pessoa (%APPDATA%\GameStream\avatar.png) e é
//! enviada diretamente a quem está na mesma transmissão; não passa por nenhum servidor.
//!
//! Limites (padrão de mercado, como Discord e Slack): PNG, JPG, WebP ou GIF (primeiro quadro) de até
//! 8 MB. A imagem é cortada no centro (quadrada) e reduzida para 128×128, ficando com poucos KB.

use std::{path::PathBuf, sync::Arc};

use anyhow::{bail, Context, Result};
use base64::Engine;

/// Tamanho máximo do arquivo escolhido.
pub const MAX_FILE_BYTES: u64 = 8 * 1024 * 1024;
/// Lado do avatar salvo e enviado.
pub const SIZE: u32 = 128;
/// Maior avatar aceito pela rede (um PNG 128×128 costuma ter 10 a 40 KB).
pub const MAX_WIRE_BYTES: usize = 96 * 1024;
/// Formatos aceitos (extensões).
pub const EXTENSIONS: [&str; 5] = ["png", "jpg", "jpeg", "webp", "gif"];

fn path() -> PathBuf {
    PathBuf::from(std::env::var("APPDATA").unwrap_or_else(|_| ".".into())).join("GameStream").join("avatar.png")
}

fn limits() -> image::Limits {
    let mut l = image::Limits::default();
    l.max_image_width = Some(8192);
    l.max_image_height = Some(8192);
    l.max_alloc = Some(256 * 1024 * 1024);
    l
}

/// Importa uma imagem escolhida pelo usuário: confere tamanho e formato, corta e reduz para 128×128.
pub fn import(file: &std::path::Path) -> Result<()> {
    let ext = file.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
    if !EXTENSIONS.contains(&ext.as_str()) {
        bail!("{}", crate::i18n::t("formato não suportado. Use PNG, JPG, WebP ou GIF."));
    }
    let len = std::fs::metadata(file).context(crate::i18n::t("não foi possível ler a imagem"))?.len();
    if len > MAX_FILE_BYTES {
        bail!("{}", crate::tr!("a imagem tem {:.1} MB; o limite é 8 MB.", "the image is {:.1} MB; the limit is 8 MB.", len as f64 / 1048576.0));
    }
    let mut reader = image::ImageReader::open(file)?.with_guessed_format()?;
    reader.limits(limits());
    let img = reader.decode().map_err(|e| anyhow::anyhow!("{}: {e}", crate::i18n::t("imagem inválida ou corrompida")))?;
    let (w, h) = (img.width(), img.height());
    if w < 16 || h < 16 {
        bail!("{}", crate::i18n::t("a imagem é pequena demais (mínimo 16×16)."));
    }
    // Corte quadrado no centro, depois redução.
    let side = w.min(h);
    let square = img.crop_imm((w - side) / 2, (h - side) / 2, side, side);
    let small = square.resize_exact(SIZE, SIZE, image::imageops::FilterType::Lanczos3).to_rgba8();
    let mut png = Vec::new();
    small.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)?;
    if png.len() > MAX_WIRE_BYTES {
        bail!("{}", crate::i18n::t("não foi possível reduzir a imagem."));
    }
    let p = path();
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d)?;
    }
    std::fs::write(&p, &png)?;
    Ok(())
}

pub fn remove() {
    let _ = std::fs::remove_file(path());
}

/// O avatar salvo (PNG), se houver e for válido.
pub fn load() -> Option<Arc<Vec<u8>>> {
    let png = std::fs::read(path()).ok()?;
    valid(&png).then(|| Arc::new(png))
}

/// Confere um avatar recebido pela rede: PNG pequeno, de no máximo 256×256.
pub fn valid(png: &[u8]) -> bool {
    if png.len() > MAX_WIRE_BYTES || !png.starts_with(b"\x89PNG\r\n\x1a\n") {
        return false;
    }
    let Ok(r) = image::ImageReader::with_format(std::io::Cursor::new(png), image::ImageFormat::Png).into_dimensions() else {
        return false;
    };
    r.0 <= 256 && r.1 <= 256 && r.0 > 0 && r.1 > 0
}

pub fn to_b64(png: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(png)
}

/// Avatar recebido (base64) → PNG validado.
pub fn from_b64(s: &str) -> Option<Arc<Vec<u8>>> {
    if s.len() > MAX_WIRE_BYTES * 4 / 3 + 8 {
        return None;
    }
    let png = base64::engine::general_purpose::STANDARD.decode(s).ok()?;
    valid(&png).then(|| Arc::new(png))
}

/// Pixels RGBA para exibir na interface.
pub fn decode_rgba(png: &[u8]) -> Option<([usize; 2], Vec<u8>)> {
    if !valid(png) {
        return None;
    }
    let img = image::load_from_memory_with_format(png, image::ImageFormat::Png).ok()?.to_rgba8();
    Some(([img.width() as usize, img.height() as usize], img.into_raw()))
}
