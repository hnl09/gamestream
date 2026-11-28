//! Canal de controle entre espectador e transmissor: nomes, chat, reações e controle remoto.
//!
//! Logo após a senha, o espectador abre um stream bidirecional e os dois lados trocam mensagens JSON,
//! uma por linha:
//!   espectador → transmissor: hello {name, token?}, chat {text}, react {r}, control_req {pad, kbm},
//!                             key {sc, ext, down}, mbtn {b, down}, wheel {d, h}
//!   transmissor → espectador: welcome {host, token}, chat {from, text}, react {from, r},
//!                             control {pad, kbm}, notice {text}
//! O estado do controle (gamepad) e o movimento do mouse vão por datagramas QUIC (não confiáveis, o
//! mais recente vale): perder um deles não atrasa os seguintes.

use anyhow::Result;
use serde_json::{json, Value};

/// Versão do protocolo do canal de controle. Sobe quando uma mudança deixa versões antigas
/// incompatíveis; o outro lado recusa conexões abaixo de MIN_PROTOCOL.
pub const PROTOCOL: u32 = 1;
pub const MIN_PROTOCOL: u32 = 1;

/// Datagrama: estado completo do controle (13 bytes).
pub const DG_PAD: u8 = 1;
/// Datagrama: movimento relativo do mouse (dx, dy em i16).
pub const DG_MOUSE: u8 = 2;

/// Reações rápidas (teclas 1 a 6 na janela do vídeo).
pub const REACTIONS: [&str; 6] = ["👍", "😂", "🔥", "❤", "😮", "GG"];

/// Permissões de controle remoto (bits).
pub const CTL_PAD: u8 = 1;
pub const CTL_KBM: u8 = 2;

/// Mensagem de chat ou reação, já com o nome de quem enviou.
#[derive(Clone, Debug, PartialEq)]
pub struct ChatMsg {
    /// Quem enviou ("h" = transmissor, "v1", "v2"... = espectadores), para mostrar o avatar.
    pub id: String,
    pub from: String,
    pub text: String,
    /// true = reação (text é um item de REACTIONS).
    pub react: bool,
    /// Apontar na tela: posição no vídeo (0..1 na largura e na altura). Não entra no chat.
    pub ping: Option<(f32, f32)>,
    /// Desenho rápido na tela (traço com pontos 0..1). Some em ~2,5 s. Não entra no chat.
    pub draw: Option<Vec<(f32, f32)>>,
}

impl ChatMsg {
    pub fn to_json(&self) -> Value {
        if let Some(pts) = &self.draw {
            let pts: Vec<[f32; 2]> = pts.iter().map(|&(x, y)| [x, y]).collect();
            json!({ "t": "draw", "id": self.id, "from": self.from, "pts": pts })
        } else if let Some((x, y)) = self.ping {
            json!({ "t": "ping", "id": self.id, "from": self.from, "x": x, "y": y })
        } else if self.react {
            json!({ "t": "react", "id": self.id, "from": self.from, "r": self.text })
        } else {
            json!({ "t": "chat", "id": self.id, "from": self.from, "text": self.text })
        }
    }

    /// Mensagem vinda do transmissor (o nome já vem preenchido por ele).
    pub fn from_json(v: &Value) -> Option<ChatMsg> {
        let from = clean_name(v["from"].as_str()?);
        let id: String = v["id"].as_str().unwrap_or("").chars().take(16).collect();
        match v["t"].as_str()? {
            "chat" => Some(ChatMsg { id, from, text: clean_text(v["text"].as_str()?)?, react: false, ping: None, draw: None }),
            "react" => Some(ChatMsg { id, from, text: clean_react(v["r"].as_str()?)?, react: true, ping: None, draw: None }),
            "ping" => Some(ChatMsg { id, from, text: String::new(), react: false, ping: Some(clean_point(v)?), draw: None }),
            "draw" => Some(ChatMsg { id, from, text: String::new(), react: false, ping: None, draw: Some(clean_stroke(v)?) }),
            _ => None,
        }
    }
}

/// Ajuste de imagem (como no controle de imagem de uma TV): aplicado pelo player de quem assiste,
/// na placa de vídeo dele. Não pesa na transmissão nem perde qualidade. Valores de -100 a 100.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Image {
    pub brightness: i32,
    pub contrast: i32,
    pub saturation: i32,
    pub gamma: i32,
}

/// Presets (id, brilho, contraste, saturação, gama).
pub const IMAGE_PRESETS: [(&str, Image); 6] = [
    ("normal", Image { brightness: 0, contrast: 0, saturation: 0, gamma: 0 }),
    ("vivid", Image { brightness: 0, contrast: 8, saturation: 30, gamma: 0 }),
    ("very_vivid", Image { brightness: 0, contrast: 12, saturation: 55, gamma: 0 }),
    ("cinema", Image { brightness: -3, contrast: 15, saturation: -10, gamma: -8 }),
    ("bright", Image { brightness: 8, contrast: 5, saturation: 10, gamma: 15 }),
    ("bw", Image { brightness: 0, contrast: 12, saturation: -100, gamma: 0 }),
];

impl Image {
    pub fn preset(id: &str) -> Option<Image> {
        IMAGE_PRESETS.iter().find(|(p, _)| *p == id).map(|(_, i)| *i)
    }

    /// Id do preset igual a este ajuste (None = personalizado).
    pub fn preset_id(&self) -> Option<&'static str> {
        IMAGE_PRESETS.iter().find(|(_, i)| i == self).map(|(p, _)| *p)
    }

    pub fn label(id: &str) -> &'static str {
        match id {
            "normal" => crate::i18n::t("Normal"),
            "vivid" => crate::i18n::t("Vívido"),
            "very_vivid" => crate::i18n::t("Muito vívido"),
            "cinema" => crate::i18n::t("Cinema"),
            "bright" => crate::i18n::t("Claro (jogos escuros)"),
            "bw" => crate::i18n::t("Preto e branco"),
            _ => crate::i18n::t("Personalizado"),
        }
    }

    pub fn describe(&self) -> &'static str {
        Image::label(self.preset_id().unwrap_or(""))
    }

    pub fn to_json(&self) -> Value {
        json!({ "t": "image", "b": self.brightness, "c": self.contrast, "s": self.saturation, "g": self.gamma })
    }

    pub fn from_json(v: &Value) -> Image {
        let f = |k: &str| (v[k].as_i64().unwrap_or(0) as i32).clamp(-100, 100);
        Image { brightness: f("b"), contrast: f("c"), saturation: f("s"), gamma: f("g") }
    }

    /// Comandos da IPC do mpv para aplicar o ajuste.
    pub fn mpv_cmds(&self) -> Vec<String> {
        [("brightness", self.brightness), ("contrast", self.contrast), ("saturation", self.saturation), ("gamma", self.gamma)]
            .iter()
            .map(|(p, v)| json!({ "command": ["set_property", p, v] }).to_string())
            .collect()
    }
}

/// Nome: sem quebras de linha nem caracteres de controle, até 24 caracteres.
pub fn clean_name(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).take(24).collect::<String>().trim().to_string()
}

/// Texto de chat: uma linha, até 200 caracteres. None se ficar vazio.
pub fn clean_text(s: &str) -> Option<String> {
    let t: String = s.chars().map(|c| if c.is_control() { ' ' } else { c }).take(200).collect();
    let t = t.trim().to_string();
    (!t.is_empty()).then_some(t)
}

/// Posição apontada (x, y entre 0 e 1). None se faltar ou estiver fora do vídeo.
pub fn clean_point(v: &Value) -> Option<(f32, f32)> {
    let (x, y) = (v["x"].as_f64()? as f32, v["y"].as_f64()? as f32);
    ((0.0..=1.0).contains(&x) && (0.0..=1.0).contains(&y)).then_some((x, y))
}

/// Traço desenhado: de 2 a 64 pontos, todos dentro do vídeo (0..1). None se inválido.
pub const MAX_STROKE: usize = 64;

pub fn clean_stroke(v: &Value) -> Option<Vec<(f32, f32)>> {
    let a = v["pts"].as_array()?;
    if !(2..=MAX_STROKE).contains(&a.len()) {
        return None;
    }
    a.iter()
        .map(|p| {
            let (x, y) = (p.get(0)?.as_f64()? as f32, p.get(1)?.as_f64()? as f32);
            ((0.0..=1.0).contains(&x) && (0.0..=1.0).contains(&y)).then_some(((x * 1e4).round() / 1e4, (y * 1e4).round() / 1e4))
        })
        .collect()
}

pub fn clean_react(s: &str) -> Option<String> {
    REACTIONS.iter().find(|r| **r == s).map(|r| r.to_string())
}

/// Escreve uma mensagem (uma linha JSON) no stream de controle.
pub async fn send(w: &mut quinn::SendStream, v: &Value) -> Result<()> {
    let mut line = serde_json::to_string(v)?;
    line.push('\n');
    w.write_all(line.as_bytes()).await?;
    Ok(())
}

/// Estado de um controle no formato XInput.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Pad {
    pub buttons: u16,
    pub lt: u8,
    pub rt: u8,
    pub lx: i16,
    pub ly: i16,
    pub rx: i16,
    pub ry: i16,
}

impl Pad {
    pub fn encode(&self) -> [u8; 13] {
        let mut b = [0u8; 13];
        b[0] = DG_PAD;
        b[1..3].copy_from_slice(&self.buttons.to_le_bytes());
        b[3] = self.lt;
        b[4] = self.rt;
        b[5..7].copy_from_slice(&self.lx.to_le_bytes());
        b[7..9].copy_from_slice(&self.ly.to_le_bytes());
        b[9..11].copy_from_slice(&self.rx.to_le_bytes());
        b[11..13].copy_from_slice(&self.ry.to_le_bytes());
        b
    }

    pub fn decode(b: &[u8]) -> Option<Pad> {
        if b.len() != 13 || b[0] != DG_PAD {
            return None;
        }
        let i = |o: usize| i16::from_le_bytes([b[o], b[o + 1]]);
        Some(Pad { buttons: u16::from_le_bytes([b[1], b[2]]), lt: b[3], rt: b[4], lx: i(5), ly: i(7), rx: i(9), ry: i(11) })
    }
}

pub fn encode_mouse(dx: i32, dy: i32) -> [u8; 5] {
    let (dx, dy) = (dx.clamp(-32768, 32767) as i16, dy.clamp(-32768, 32767) as i16);
    let mut b = [0u8; 5];
    b[0] = DG_MOUSE;
    b[1..3].copy_from_slice(&dx.to_le_bytes());
    b[3..5].copy_from_slice(&dy.to_le_bytes());
    b
}

pub fn decode_mouse(b: &[u8]) -> Option<(i32, i32)> {
    if b.len() != 5 || b[0] != DG_MOUSE {
        return None;
    }
    Some((i16::from_le_bytes([b[1], b[2]]) as i32, i16::from_le_bytes([b[3], b[4]]) as i32))
}

/// Chave que identifica este PC para um transmissor (para ele lembrar que já aprovou você).
/// Derivada de um segredo local e da identidade do transmissor: cada transmissor recebe uma chave
/// diferente, então um não consegue usar a sua chave para se passar por você com outro.
pub fn viewer_key(host_fp: &[u8; 32]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(b"gamestream-viewer-key\0");
    h.update(crate::room::load_secret().as_bytes());
    h.update(host_fp);
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// O que o transmissor guarda: só o hash da chave (nunca a chave em si).
pub fn key_hash(key: &str) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(key.as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}

/// Descrição das permissões ("controle", "teclado e mouse", ...).
pub fn ctl_desc(bits: u8) -> String {
    match (bits & CTL_PAD != 0, bits & CTL_KBM != 0) {
        (true, true) => crate::i18n::t("controle + teclado e mouse").into(),
        (true, false) => crate::i18n::t("controle").into(),
        (false, true) => crate::i18n::t("teclado e mouse").into(),
        _ => crate::i18n::t("nada").into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pad_roundtrip() {
        let p = Pad { buttons: 0x9013, lt: 200, rt: 7, lx: -32768, ly: 32767, rx: 123, ry: -456 };
        assert_eq!(Pad::decode(&p.encode()), Some(p));
        assert_eq!(decode_mouse(&encode_mouse(-5, 70000)), Some((-5, 32767)));
    }

    #[test]
    fn ping_roundtrip() {
        let m = ChatMsg { id: "v1".into(), from: "Ana".into(), text: String::new(), react: false, ping: Some((0.25, 0.75)), draw: None };
        assert_eq!(ChatMsg::from_json(&m.to_json()), Some(m));
        assert_eq!(ChatMsg::from_json(&json!({ "t": "ping", "from": "x", "x": 1.5, "y": 0.2 })), None);
    }

    #[test]
    fn image_presets_and_limits() {
        let v = Image::preset("vivid").unwrap();
        assert_eq!(Image::from_json(&v.to_json()), v);
        assert_eq!(v.preset_id(), Some("vivid"));
        assert_eq!(Image::from_json(&json!({ "s": 500, "c": -300 })), Image { saturation: 100, contrast: -100, ..Default::default() });
        assert_eq!(Image { saturation: 7, ..Default::default() }.preset_id(), None);
        assert_eq!(v.mpv_cmds().len(), 4);
    }

    #[test]
    fn draw_roundtrip_and_limits() {
        let m = ChatMsg { id: "v1".into(), from: "Ana".into(), text: String::new(), react: false, ping: None, draw: Some(vec![(0.1, 0.2), (0.5, 0.5), (0.9, 0.25)]) };
        assert_eq!(ChatMsg::from_json(&m.to_json()), Some(m));
        assert_eq!(ChatMsg::from_json(&json!({ "t": "draw", "from": "x", "pts": [[0.1, 0.1]] })), None);
        assert_eq!(ChatMsg::from_json(&json!({ "t": "draw", "from": "x", "pts": [[0.1, 0.1], [1.2, 0.3]] })), None);
        let many: Vec<[f32; 2]> = (0..65).map(|i| [i as f32 / 100.0, 0.5]).collect();
        assert_eq!(ChatMsg::from_json(&json!({ "t": "draw", "from": "x", "pts": many })), None);
    }

    #[test]
    fn cleaning() {
        assert_eq!(clean_text("  oi\ntudo bem  ").as_deref(), Some("oi tudo bem"));
        assert_eq!(clean_text(" \n "), None);
        assert_eq!(clean_name("Ana\u{7}Maria com um nome muito comprido demais"), "AnaMaria com um nome mui");
        assert_eq!(clean_react("🔥").as_deref(), Some("🔥"));
        assert_eq!(clean_react("<script>"), None);
    }
}
