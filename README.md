# GameStream

Stream de jogos com baixa latência entre amigos, para Windows. Feito em Rust.

- **Host e viewer no mesmo app**, com interface gráfica.
- **Vídeo HEVC codificado na GPU** (AMD AMF, NVIDIA NVENC ou Intel QSV, detectado sozinho), com captura zero-copy via DXGI.
- **Transmite um monitor inteiro ou só a janela de um jogo.** A janela é capturada pela Windows Graphics Capture (`gfxcapture` do FFmpeg), então segue a janela e funciona mesmo coberta.
- **Áudio por programa** (WASAPI process loopback): só o jogo, ou tudo menos o Discord. Microfone opcional, mixado.
- **Bitrate automático** (AIMD conforme perda de pacotes dos viewers) ou fixo.
- **Vários viewers ao mesmo tempo**: o vídeo é codificado uma vez e repassado para todos.
- **Conexão sem mexer no modem**: IPv6, UPnP ou VPN (Tailscale/Radmin). O convite leva todos os caminhos e o viewer tenta todos em paralelo.
- **Segurança**: QUIC + TLS 1.3, certificate pinning pelo convite (anti-MITM), senha e código de segurança para conferir.

## Download

Pegue o `GameStream.zip` na aba [Releases](../../releases), extraia e abra o `gamestream.exe`.
O FFmpeg é baixado pelo app na primeira transmissão, e as atualizações aparecem como um botão no topo.

> A atualização automática e o download do FFmpeg usam as releases do GitHub: **o repositório precisa ser público**.
As instruções estão no [LEIA-ME.txt](LEIA-ME.txt).

## Arquitetura

```
HOST                                                  VIEWER (xN)
ddagrab (DXGI, GPU) ─► hevc_amf/nvenc/qsv (ffmpeg)     QUIC ─► mpv (decodificação HW, low-latency)
WASAPI loopback ─► PCM                       ─► QUIC ─►      ─► cpal (buffer de jitter ~40 ms)
```

| Módulo | Função |
|---|---|
| `src/host.rs` | encoder (FFmpeg), distribuição para viewers, controle de bitrate |
| `src/viewer.rs` | conexão (multi-endereço), player |
| `src/net.rs` | QUIC/TLS, identidade persistente, verificação por pinning |
| `src/invite.rs` | convites, IPs locais/IPv6/VPN, UPnP |
| `src/sources.rs` | monitores (DXGI) e janelas |
| `src/audio.rs` | captura (mixer de fontes + microfone) e reprodução de áudio |
| `src/procaudio.rs` | captura de áudio por processo, detecção do Discord |
| `src/updater.rs` | atualização automática e download do FFmpeg |
| `assets/overlay.lua` | painel dentro da janela do vídeo (volume, zoom, estatísticas) |
| `src/gui.rs` | interface (egui) |

## Compilar

Requisitos: Rust (MSVC), Visual Studio Build Tools (C++), e `ffmpeg` e `mpv` no PATH para testar.

```powershell
$env:CARGO_TARGET_DIR="$env:USERPROFILE\.gamestream-target"   # evita erro de caminho longo
cargo build --release
.\empacotar.ps1   # monta dist\ e GameStream.zip
```

Para publicar uma release, crie uma tag `v*` (ex.: `git tag v0.2.0 && git push --tags`).
O GitHub Actions compila e publica: `GameStream.zip` (app + mpv), `gamestream.exe` (+ `.sha256`, para a atualização)
e `ffmpeg.exe.gz` (+ `.sha256`, baixado pelo app quando alguém transmite).

## Componentes de terceiros

O pacote de release inclui o FFmpeg e o mpv, ambos sob a licença GPL. Veja [THIRD-PARTY.txt](THIRD-PARTY.txt).
