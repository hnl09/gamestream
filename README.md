# GameStream

Stream de jogos com baixa latência entre amigos, para Windows. Feito em Rust.

- **Host e viewer no mesmo app**, com interface gráfica.
- **Vídeo HEVC codificado na GPU** (AMD AMF, NVIDIA NVENC ou Intel QSV, detectado sozinho), com captura zero-copy via DXGI.
- **Transmite um monitor inteiro ou só a janela de um jogo.**
- **Áudio do sistema** (WASAPI loopback).
- **Bitrate automático** (AIMD conforme perda de pacotes dos viewers) ou fixo.
- **Vários viewers ao mesmo tempo**: o vídeo é codificado uma vez e repassado para todos.
- **Conexão sem mexer no modem**: IPv6, UPnP ou VPN (Tailscale/Radmin). O convite leva todos os caminhos e o viewer tenta todos em paralelo.
- **Segurança**: QUIC + TLS 1.3, certificate pinning pelo convite (anti-MITM), senha e código de segurança para conferir.

## Download

Pegue o `GameStream.zip` na aba [Releases](../../releases), extraia e abra o `gamestream.exe`.
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
| `src/audio.rs` | captura e reprodução de áudio |
| `src/gui.rs` | interface (egui) |

## Compilar

Requisitos: Rust (MSVC), Visual Studio Build Tools (C++), e `ffmpeg` e `mpv` no PATH para testar.

```powershell
$env:CARGO_TARGET_DIR="$env:USERPROFILE\.gamestream-target"   # evita erro de caminho longo
cargo build --release
.\empacotar.ps1   # monta dist\ e GameStream.zip
```

Para publicar uma release, crie uma tag `v*` (ex.: `git tag v0.2.0 && git push --tags`).
O GitHub Actions compila, empacota com FFmpeg e mpv e publica a release.

## Componentes de terceiros

O pacote de release inclui o FFmpeg e o mpv, ambos sob a licença GPL. Veja [THIRD-PARTY.txt](THIRD-PARTY.txt).
