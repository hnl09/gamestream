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
        let raw = String::from_utf8(B64.decode(b).context(crate::i18n::t("convite inválido"))?).context(crate::i18n::t("convite inválido"))?;
        let (a, f) = raw.split_once('|').context(crate::i18n::t("convite inválido"))?;
        let addrs = a
            .split(',')
            .filter(|s| !s.is_empty())
            .map(|s| s.parse::<SocketAddr>())
            .collect::<Result<Vec<_>, _>>()
            .context(crate::i18n::t("convite inválido"))?;
        let fp: [u8; 32] = B64
            .decode(f)
            .ok()
            .and_then(|v| v.try_into().ok())
            .context(crate::i18n::t("convite inválido"))?;
        if addrs.is_empty() {
            bail!(crate::i18n::t("convite sem endereços"));
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
    let addrs: Vec<SocketAddr> = tokio::net::lookup_host(&target).await.context(crate::i18n::t("endereço inválido"))?.collect();
    if addrs.is_empty() {
        bail!("{}", crate::tr!("endereço inválido: {t}", "invalid address: {t}"));
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
    let b = t.strip_prefix("GS-R-").context(crate::i18n::t("código de resposta inválido (deve começar com GS-R-)"))?;
    let raw = String::from_utf8(B64.decode(b).context(crate::i18n::t("código de resposta inválido"))?).context(crate::i18n::t("código de resposta inválido"))?;
    let addrs = raw
        .split(',')
        .filter(|s| !s.is_empty())
        .map(|s| s.parse::<SocketAddr>())
        .collect::<Result<Vec<_>, _>>()
        .context(crate::i18n::t("código de resposta inválido"))?;
    if addrs.is_empty() || addrs.len() > 16 {
        bail!(crate::i18n::t("código de resposta inválido"));
    }
    Ok(addrs)
}

fn is_cgnat(v4: Ipv4Addr) -> bool {
    let o = v4.octets();
    o[0] == 100 && (64..128).contains(&o[1])
}

/// Endereços deste computador pelos quais os espectadores podem se conectar, com a descrição de cada um.
/// Tipo de endereço (a ordem é a prioridade no convite). A lógica usa o tipo, nunca o texto exibido,
/// que muda conforme o idioma.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum AddrKind {
    PublicV4,
    PublicV6,
    Vpn,
    Lan,
}

impl AddrKind {
    /// Endereço alcançável de fora da rede local.
    pub fn is_internet(self) -> bool {
        self != AddrKind::Lan
    }
}

/// Endereço local, tipo e descrição (no idioma atual).
pub fn local_addrs(port: u16) -> Vec<(SocketAddr, AddrKind, String)> {
    let mut out = Vec::new();
    for iface in if_addrs::get_if_addrs().unwrap_or_default() {
        if iface.is_loopback() {
            continue;
        }
        let ip = iface.ip();
        let (kind, label) = match ip {
            IpAddr::V4(v4) if v4.is_link_local() => continue,
            IpAddr::V4(v4) if v4.is_private() => {
                (AddrKind::Lan, crate::tr!("rede local ({})", "local network ({})", iface.name))
            }
            IpAddr::V4(v4) if is_cgnat(v4) => (AddrKind::Vpn, format!("VPN/Tailscale ({})", iface.name)),
            IpAddr::V4(_) => (AddrKind::PublicV4, crate::i18n::t("IPv4 público").into()),
            IpAddr::V6(v6) if (v6.segments()[0] & 0xe000) == 0x2000 => {
                (AddrKind::PublicV6, crate::i18n::t("IPv6 público").into())
            }
            IpAddr::V6(_) => continue,
        };
        out.push((SocketAddr::new(ip, port), kind, label));
    }
    out
}

/// Tipo de um endereço do convite (do lado de quem assiste, para entender se há caminho).
pub fn kind_of(ip: IpAddr) -> Option<AddrKind> {
    match ip {
        IpAddr::V4(v4) if v4.is_loopback() || v4.is_link_local() || v4.is_unspecified() => None,
        IpAddr::V4(v4) if v4.is_private() => Some(AddrKind::Lan),
        IpAddr::V4(v4) if is_cgnat(v4) => Some(AddrKind::Vpn),
        IpAddr::V4(_) => Some(AddrKind::PublicV4),
        IpAddr::V6(v6) if (v6.segments()[0] & 0xe000) == 0x2000 => Some(AddrKind::PublicV6),
        IpAddr::V6(v6) => v6.to_ipv4_mapped().and_then(|v4| kind_of(IpAddr::V4(v4))),
    }
}

/// Por que não há caminho entre quem assiste e o transmissor (se dá para saber antes de tentar).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoPath {
    /// O transmissor só aceita IPv6 e quem assiste não tem IPv6.
    Ipv6Only,
    /// O transmissor só aceita pela VPN (Tailscale) e quem assiste não está nela.
    VpnOnly,
    /// O transmissor só é acessível na rede local dele.
    LanOnly,
}

pub fn no_path(host: &[SocketAddr], mine: &[IpAddr]) -> Option<NoPath> {
    let hk: Vec<(IpAddr, AddrKind)> = host.iter().filter_map(|a| kind_of(a.ip()).map(|k| (a.ip().to_canonical(), k))).collect();
    let mk: Vec<(IpAddr, AddrKind)> = mine.iter().filter_map(|ip| kind_of(*ip).map(|k| (ip.to_canonical(), k))).collect();
    let has = |v: &[(IpAddr, AddrKind)], k: AddrKind| v.iter().any(|(_, x)| *x == k);
    if has(&hk, AddrKind::PublicV4) {
        return None;
    }
    // Mesma rede local (ex.: dois PCs na mesma casa).
    let same_lan = hk.iter().filter(|(_, k)| *k == AddrKind::Lan).any(|(h, _)| {
        mk.iter().filter(|(_, k)| *k == AddrKind::Lan).any(|(m, _)| match (h, m) {
            (IpAddr::V4(a), IpAddr::V4(b)) => a.octets()[..3] == b.octets()[..3],
            _ => false,
        })
    });
    if same_lan || (has(&hk, AddrKind::Vpn) && has(&mk, AddrKind::Vpn)) || (has(&hk, AddrKind::PublicV6) && has(&mk, AddrKind::PublicV6)) {
        return None;
    }
    Some(if has(&hk, AddrKind::PublicV6) {
        NoPath::Ipv6Only
    } else if has(&hk, AddrKind::Vpn) {
        NoPath::VpnOnly
    } else {
        NoPath::LanOnly
    })
}

impl NoPath {
    /// Explicação para quem assiste.
    pub fn viewer_text(self) -> String {
        match self {
            NoPath::Ipv6Only => crate::i18n::t("Sem caminho até o transmissor: ele só aceita conexões por IPv6, e a sua internet não tem IPv6. Para resolver: (1) quem transmite liga o UPnP no modem dele (o GameStream mostra como, no card \"Quem consegue te assistir\"); ou (2) você ativa o IPv6 no seu modem ou pede para a operadora (confira em test-ipv6.com); ou (3) vocês dois usam o Tailscale (gratuito).").into(),
            NoPath::VpnOnly => crate::i18n::t("Sem caminho até o transmissor: ele só está acessível pela VPN (Tailscale). Entre na mesma rede do Tailscale que ele e tente de novo.").into(),
            NoPath::LanOnly => crate::i18n::t("Sem caminho até o transmissor: ele só está acessível na rede local dele (sem IPv6 nem porta aberta). Peça para ele ligar o UPnP no modem ou usem o Tailscale.").into(),
        }
    }
}

/// Endereço do roteador (para abrir a página de configuração dele).
pub fn default_gateway() -> Option<std::net::Ipv4Addr> {
    use windows::Win32::NetworkManagement::IpHelper::{GetBestRoute, MIB_IPFORWARDROW};
    let mut row = MIB_IPFORWARDROW::default();
    let dest = u32::from_ne_bytes([8, 8, 8, 8]);
    let r = unsafe { GetBestRoute(dest, 0, &mut row) };
    if r != 0 {
        return None;
    }
    let ip = std::net::Ipv4Addr::from(row.dwForwardNextHop.to_ne_bytes());
    (!ip.is_unspecified()).then_some(ip)
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
        .context(crate::i18n::t("o roteador não respondeu ao UPnP (o recurso pode estar desativado no modem)"))?;
    let local_ip = {
        let s = std::net::UdpSocket::bind("0.0.0.0:0")?;
        s.connect(gw.addr)?;
        s.local_addr()?.ip()
    };
    let local = SocketAddr::new(local_ip, port);
    let m = PortMapping { gw, local, port };
    m.renew().await?;
    let ext = m.gw.get_external_ip().await.context(crate::i18n::t("o roteador não informou o IP externo"))?;
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
            .context(crate::i18n::t("o roteador recusou abrir a porta"))?;
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

#[cfg(test)]
mod tests {
    use super::*;

    fn a(s: &str) -> SocketAddr {
        s.parse().unwrap()
    }

    #[test]
    fn no_path_cases() {
        let host_v6_only = [a("[2804:7f0:1::1]:9000"), a("192.168.15.71:9000")];
        // Quem assiste sem IPv6 (só IPv4 da operadora e rede local): sem caminho.
        let viewer_v4: Vec<IpAddr> = vec!["177.10.20.30".parse().unwrap(), "192.168.0.10".parse().unwrap()];
        assert_eq!(no_path(&host_v6_only, &viewer_v4), Some(NoPath::Ipv6Only));
        // Com IPv6: tem caminho.
        let viewer_v6: Vec<IpAddr> = vec!["2804:14c:abcd::5".parse().unwrap()];
        assert_eq!(no_path(&host_v6_only, &viewer_v6), None);
        // Na mesma casa (mesma rede local): tem caminho.
        let same_house: Vec<IpAddr> = vec!["192.168.15.20".parse().unwrap()];
        assert_eq!(no_path(&host_v6_only, &same_house), None);
        // Transmissor com IPv4 aberto: todos conseguem.
        assert_eq!(no_path(&[a("189.1.2.3:9000")], &viewer_v4), None);
        // Só VPN dos dois lados.
        let vpn = [a("100.101.102.103:9000")];
        assert_eq!(no_path(&vpn, &["100.64.0.9".parse().unwrap()]), None);
        assert_eq!(no_path(&vpn, &viewer_v4), Some(NoPath::VpnOnly));
        // Só rede local de outra casa.
        assert_eq!(no_path(&[a("192.168.15.71:9000")], &["192.168.0.10".parse().unwrap()]), Some(NoPath::LanOnly));
    }
}
