//! Convites (endereços + impressão digital do host) e alcançabilidade:
//! IPs locais, IPv6 público, Tailscale/VPN e abertura automática de porta via UPnP.

use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    time::Duration,
};

use anyhow::{bail, Context, Result};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD as B64, Engine};
use igd_next::PortMappingProtocol;

pub struct Invite {
    pub addrs: Vec<SocketAddr>,
    pub fingerprint: Option<[u8; 32]>,
}

pub fn encode(addrs: &[SocketAddr], fp: &[u8; 32]) -> String {
    let list: Vec<String> = addrs.iter().map(|a| a.to_string()).collect();
    format!("GS-{}", B64.encode(format!("{}|{}", list.join(","), B64.encode(fp))))
}

/// Aceita um convite `GS-...` ou um IP/host simples (sem verificação de identidade).
pub async fn decode(input: &str, default_port: u16) -> Result<Invite> {
    let t: String = input.chars().filter(|c| !c.is_whitespace()).collect();
    if let Some(b) = t.strip_prefix("GS-") {
        let raw = String::from_utf8(B64.decode(b).context("convite inválido")?).context("convite inválido")?;
        let (a, f) = raw.split_once('|').context("convite inválido")?;
        let addrs = a
            .split(',')
            .filter(|s| !s.is_empty())
            .map(|s| s.parse::<SocketAddr>())
            .collect::<Result<Vec<_>, _>>()
            .context("convite inválido")?;
        let fp: [u8; 32] = B64
            .decode(f)
            .ok()
            .and_then(|v| v.try_into().ok())
            .context("convite inválido")?;
        if addrs.is_empty() {
            bail!("convite sem endereços");
        }
        return Ok(Invite { addrs, fingerprint: Some(fp) });
    }
    let target = if let Ok(sa) = t.parse::<SocketAddr>() {
        sa.to_string()
    } else if let Ok(ip) = t.parse::<IpAddr>() {
        SocketAddr::new(ip, default_port).to_string()
    } else if t.contains(':') {
        t.clone()
    } else {
        format!("{t}:{default_port}")
    };
    let addrs: Vec<SocketAddr> = tokio::net::lookup_host(&target).await.context("endereço inválido")?.collect();
    if addrs.is_empty() {
        bail!("endereço inválido: {t}");
    }
    Ok(Invite { addrs, fingerprint: None })
}

/// Código de resposta do espectador (GS-R-...): endereços do espectador para o transmissor liberar.
pub fn encode_reply(addrs: &[SocketAddr]) -> String {
    let list: Vec<String> = addrs.iter().map(|a| a.to_string()).collect();
    format!("GS-R-{}", B64.encode(list.join(",")))
}

pub fn decode_reply(input: &str) -> Result<Vec<SocketAddr>> {
    let t: String = input.chars().filter(|c| !c.is_whitespace() && *c != '\u{feff}').collect();
    let b = t.strip_prefix("GS-R-").context("código de resposta inválido (deve começar com GS-R-)")?;
    let raw = String::from_utf8(B64.decode(b).context("código de resposta inválido")?).context("código de resposta inválido")?;
    let addrs = raw
        .split(',')
        .filter(|s| !s.is_empty())
        .map(|s| s.parse::<SocketAddr>())
        .collect::<Result<Vec<_>, _>>()
        .context("código de resposta inválido")?;
    if addrs.is_empty() || addrs.len() > 16 {
        bail!("código de resposta inválido");
    }
    Ok(addrs)
}

fn is_cgnat(v4: Ipv4Addr) -> bool {
    let o = v4.octets();
    o[0] == 100 && (64..128).contains(&o[1])
}

/// Endereços deste computador pelos quais os espectadores podem se conectar, com a descrição de cada um.
pub fn local_addrs(port: u16) -> Vec<(SocketAddr, String)> {
    let mut out = Vec::new();
    for iface in if_addrs::get_if_addrs().unwrap_or_default() {
        if iface.is_loopback() {
            continue;
        }
        let ip = iface.ip();
        let kind = match ip {
            IpAddr::V4(v4) if v4.is_link_local() => continue,
            IpAddr::V4(v4) if v4.is_private() => format!("rede local ({})", iface.name),
            IpAddr::V4(v4) if is_cgnat(v4) => format!("VPN/Tailscale ({})", iface.name),
            IpAddr::V4(_) => "IPv4 público".into(),
            IpAddr::V6(v6) if (v6.segments()[0] & 0xe000) == 0x2000 => "IPv6 público".into(),
            IpAddr::V6(_) => continue,
        };
        out.push((SocketAddr::new(ip, port), kind));
    }
    out
}

pub struct PortMapping {
    gw: igd_next::aio::Gateway<igd_next::aio::tokio::Tokio>,
    local: SocketAddr,
    port: u16,
}

/// Pede ao roteador (UPnP) para abrir a porta UDP. Retorna o IP externo informado pelo roteador.
pub async fn map_port(port: u16) -> Result<(PortMapping, IpAddr)> {
    let opts = igd_next::SearchOptions { timeout: Some(Duration::from_secs(3)), ..Default::default() };
    let gw = igd_next::aio::tokio::search_gateway(opts)
        .await
        .context("o roteador não respondeu ao UPnP (o recurso pode estar desativado no modem)")?;
    let local_ip = {
        let s = std::net::UdpSocket::bind("0.0.0.0:0")?;
        s.connect(gw.addr)?;
        s.local_addr()?.ip()
    };
    let local = SocketAddr::new(local_ip, port);
    let m = PortMapping { gw, local, port };
    m.renew().await?;
    let ext = m.gw.get_external_ip().await.context("o roteador não informou o IP externo")?;
    Ok((m, ext))
}

impl PortMapping {
    pub async fn renew(&self) -> Result<()> {
        if self
            .gw
            .add_port(PortMappingProtocol::UDP, self.port, self.local, 3600, "GameStream")
            .await
            .is_ok()
        {
            return Ok(());
        }
        // Alguns roteadores só aceitam mapeamento "permanente".
        self.gw
            .add_port(PortMappingProtocol::UDP, self.port, self.local, 0, "GameStream")
            .await
            .context("o roteador recusou abrir a porta")?;
        Ok(())
    }

    pub async fn remove(&self) {
        let _ = self.gw.remove_port(PortMappingProtocol::UDP, self.port).await;
    }
}

/// O IP externo do roteador é realmente público? (Se não, é CGNAT da operadora.)
pub fn is_public_v4(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => !(v4.is_private() || is_cgnat(v4) || v4.is_loopback() || v4.is_link_local() || v4.is_unspecified()),
        IpAddr::V6(_) => true,
    }
}
