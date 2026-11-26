//! Viewer: conecta no host via QUIC (tenta todos os endereços do convite ao mesmo tempo),
//! decodifica o HEVC na GPU (mpv, modo baixa latência) e toca o áudio com buffer curto.

use std::{future::Future, path::PathBuf, process::Stdio, time::Duration};

use anyhow::{anyhow, bail, Context, Result};
use tokio::{io::AsyncWriteExt, process::Command, task::JoinSet};

use crate::{
    audio, invite, net,
    state::{log, view_status},
};

pub struct ViewOpts {
    pub invite: String,
    pub password: Option<String>,
    pub mpv: Option<String>,
}

/// Procura o mpv.exe ao lado do gamestream.exe; senão, usa o do PATH.
fn find_mpv(explicit: Option<String>) -> PathBuf {
    if let Some(p) = explicit {
        return p.into();
    }
    if let Ok(exe) = std::env::current_exe() {
        let local = exe.with_file_name("mpv.exe");
        if local.exists() {
            return local;
        }
    }
    "mpv".into()
}

pub async fn run(o: ViewOpts, stop: impl Future<Output = ()>) -> Result<()> {
    let r = tokio::select! {
        r = run_inner(o) => r,
        _ = stop => Ok(()),
    };
    match &r {
        Ok(()) => view_status("Desconectado."),
        Err(e) => {
            log(format!("Erro: {e:#}"));
            view_status(format!("Erro: {e:#}"));
        }
    }
    r
}

async fn run_inner(o: ViewOpts) -> Result<()> {
    let inv = invite::decode(&o.invite, net::DEFAULT_PORT).await?;
    if inv.fingerprint.is_none() {
        log("Aviso: conectando por IP direto, sem verificar a identidade do host. Prefira o convite GS-...");
    }
    view_status("Conectando...");
    log(format!("Conectando ({} endereço(s))...", inv.addrs.len()));
    let ep = net::client_endpoint(inv.fingerprint)?;

    // Tenta todos os endereços em paralelo; o primeiro que responder ganha.
    let mut tries = JoinSet::new();
    for a in inv.addrs.clone() {
        let ep = ep.clone();
        tries.spawn(async move {
            let c = ep.connect(a, "gamestream")?.await?;
            anyhow::Ok(c)
        });
    }
    let conn = tokio::time::timeout(Duration::from_secs(8), async {
        let mut last = None;
        while let Some(r) = tries.join_next().await {
            match r {
                Ok(Ok(c)) => return Ok(c),
                Ok(Err(e)) => last = Some(e),
                Err(e) => last = Some(e.into()),
            }
        }
        Err(last.unwrap_or_else(|| anyhow!("nenhum endereço")))
    })
    .await
    .map_err(|_| anyhow!("tempo esgotado: o host está transmitindo? Você está no mesmo tipo de rede (IPv6/Tailscale)?"))??;
    tries.abort_all();

    // Sem convite (IP direto) não há como saber se é mesmo o host: não manda a senha às cegas.
    if inv.fingerprint.is_none() && o.password.is_some() {
        conn.close(0u32.into(), b"");
        bail!("por segurança, a senha só é enviada quando você conecta pelo convite GS-... (que verifica a identidade do host)");
    }
    let mut auth = conn.open_uni().await?;
    auth.write_all(o.password.unwrap_or_default().as_bytes()).await?;
    auth.finish()?;
    let via = conn.remote_address().ip().to_canonical();
    let code = net::peer_security_code(&conn).unwrap_or_else(|| "?".into());
    let verified = if inv.fingerprint.is_some() { "identidade verificada pelo convite" } else { "identidade NÃO verificada" };
    log(format!("Conectado em {via} (criptografado, {verified}). Código de segurança do host: {code}"));
    view_status(format!(
        "Conectado em {via} — criptografado, {verified}.\nCódigo de segurança: {code} (confira com o host)\nO vídeo abre em outra janela (F = tela cheia)."
    ));

    let mpv = find_mpv(o.mpv);
    let mut child = Command::new(&mpv)
        .args([
            "--profile=low-latency",
            "--untimed",
            "--cache=no",
            "--hwdec=auto",
            "--vd-lavc-threads=1",
            "--video-latency-hacks=yes",
            "--demuxer-lavf-format=hevc",
            "--demuxer-lavf-probesize=32",
            "--demuxer-lavf-analyzeduration=0",
            "--audio=no",
            "--osc=no",
            "--keep-open=no",
            "--force-window=immediate",
            "--title=GameStream - assistindo",
            "--really-quiet",
            "-",
        ])
        .stdin(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .with_context(|| format!("não achei o mpv ({}). Coloque o mpv.exe na mesma pasta do gamestream.exe", mpv.display()))?;
    let mut stdin = Some(child.stdin.take().unwrap());

    let mut tasks = JoinSet::new();
    let result = loop {
        tokio::select! {
            st = conn.accept_uni() => {
                let mut st = match st {
                    Ok(s) => s,
                    Err(quinn::ConnectionError::ApplicationClosed(c)) => {
                        break Err(anyhow!("o host fechou a conexão: {}", String::from_utf8_lossy(&c.reason)));
                    }
                    Err(e) => break Err(anyhow!("conexão com o host caiu: {e}")),
                };
                let mut kind = [0u8; 1];
                st.read_exact(&mut kind).await?;
                match kind[0] {
                    net::STREAM_VIDEO => {
                        let Some(mut pipe) = stdin.take() else { bail!("host mandou dois streams de vídeo") };
                        tasks.spawn(async move {
                            let mut buf = Vec::new();
                            while net::read_msg(&mut st, &mut buf).await? {
                                pipe.write_all(&buf).await?;
                            }
                            anyhow::Ok(())
                        });
                    }
                    net::STREAM_AUDIO => {
                        let mut hdr = [0u8; 6];
                        st.read_exact(&mut hdr).await?;
                        let rate = u32::from_le_bytes(hdr[0..4].try_into().unwrap());
                        let ch = u16::from_le_bytes(hdr[4..6].try_into().unwrap());
                        match audio::start_player(rate, ch) {
                            Ok(mut player) => {
                                tasks.spawn(async move {
                                    let mut buf = Vec::new();
                                    while net::read_msg(&mut st, &mut buf).await? {
                                        player.push(&buf);
                                    }
                                    anyhow::Ok(())
                                });
                            }
                            Err(e) => log(format!("Sem áudio: {e:#}")),
                        }
                    }
                    k => log(format!("stream desconhecido: {k}")),
                }
            }
            Some(r) = tasks.join_next() => {
                break match r? {
                    Ok(()) => { log("O host encerrou a transmissão."); Ok(()) }
                    Err(e) => Err(anyhow!("transmissão interrompida: {e:#}")),
                };
            }
            _ = child.wait() => { log("Player fechado."); break Ok(()); }
        }
    };
    conn.close(0u32.into(), b"tchau");
    let _ = tokio::time::timeout(Duration::from_secs(1), ep.wait_idle()).await;
    result
}
