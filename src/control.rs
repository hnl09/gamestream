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
}

impl ChatMsg {
    pub fn to_json(&self) -> Value {
        if self.react {
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
            "chat" => Some(ChatMsg { id, from, text: clean_text(v["text"].as_str()?)?, react: false }),
            "react" => Some(ChatMsg { id, from, text: clean_react(v["r"].as_str()?)?, react: true }),
            _ => None,
        }
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
    fn cleaning() {
        assert_eq!(clean_text("  oi\ntudo bem  ").as_deref(), Some("oi tudo bem"));
        assert_eq!(clean_text(" \n "), None);
        assert_eq!(clean_name("Ana\u{7}Maria com um nome muito comprido demais"), "AnaMaria com um nome mui");
        assert_eq!(clean_react("🔥").as_deref(), Some("🔥"));
        assert_eq!(clean_react("<script>"), None);
    }
}
