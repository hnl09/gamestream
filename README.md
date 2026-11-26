# GameStream

Transmissão de vídeo e áudio com baixa latência para Windows, desenvolvida em Rust.

## Recursos

- **Transmissor e espectador no mesmo aplicativo**, com interface gráfica e linha de comando.
- **Vídeo HEVC codificado na GPU**: AMD AMF, NVIDIA NVENC ou Intel QSV, com detecção automática e captura zero-copy via DXGI.
- **Captura de monitor ou de janela específica.** A janela é capturada pela Windows Graphics Capture (filtro `gfxcapture` do FFmpeg) e continua sendo transmitida quando movida, redimensionada ou sobreposta.
- **Áudio por aplicativo** (WASAPI process loopback): apenas o aplicativo transmitido ou todo o sistema exceto o Discord. Microfone opcional, mixado ao áudio.
- **Bitrate adaptativo** (AIMD, com base na perda de pacotes dos espectadores) ou fixo.
- **Múltiplos espectadores**: o vídeo é codificado uma única vez e distribuído a todos.
- **Conectividade sem configuração do roteador**: IPv6, UPnP ou VPN (Tailscale/Radmin). O convite contém todos os endereços disponíveis, testados em paralelo pelo espectador.
- **Segurança**: QUIC com TLS 1.3, certificate pinning pelo convite (proteção contra MITM), senha e código de segurança verificável.
- **Painel na janela de vídeo**: volume, zoom a partir do cursor, tela cheia e estatísticas (ping, FPS, perda de pacotes e taxa recebida).
- **Atualização automática** com verificação SHA-256.

## Download

Baixe o `GameStream.zip` na página de [Releases](../../releases), extraia o conteúdo e execute o `gamestream.exe`.
O FFmpeg é obtido automaticamente na primeira transmissão, e novas versões são oferecidas por um botão no topo do aplicativo.

Instruções completas: [LEIA-ME.txt](LEIA-ME.txt).

## Arquitetura

```
TRANSMISSOR                                               ESPECTADOR (xN)
ddagrab / gfxcapture (GPU) ─► hevc_amf/nvenc/qsv (FFmpeg)  QUIC ─► mpv (decodificação por hardware, baixa latência)
WASAPI (loopback por aplicativo + microfone) ─► mixer ─► QUIC ─► cpal (buffer de jitter ~40 ms)
```

| Módulo | Função |
|---|---|
| `src/host.rs` | encoder (FFmpeg), distribuição aos espectadores, controle de bitrate |
| `src/viewer.rs` | conexão (múltiplos endereços), player, estatísticas |
| `src/net.rs` | QUIC/TLS, identidade persistente, verificação por pinning |
| `src/invite.rs` | convites, endereços locais/IPv6/VPN, UPnP |
| `src/sources.rs` | monitores (DXGI) e janelas |
| `src/audio.rs` | captura (mixer de fontes e microfone) e reprodução de áudio |
| `src/procaudio.rs` | captura de áudio por aplicativo, detecção do Discord |
| `src/updater.rs` | atualização automática e download do FFmpeg |
| `src/gui.rs` | interface gráfica (egui) |
| `assets/overlay.lua` | painel da janela de vídeo (volume, zoom, estatísticas) |

## Compilação

Requisitos: Rust (toolchain MSVC), Visual Studio Build Tools (C++) e, para testes locais, `ffmpeg` e `mpv` no PATH.

```powershell
$env:CARGO_TARGET_DIR="$env:USERPROFILE\.gamestream-target"   # evita erro de caminho longo no Windows
cargo build --release
.\empacotar.ps1   # gera dist\ (teste local) e out\ (arquivos da release)
```

## Publicação

Crie e envie uma tag `v*` (ex.: `git tag v0.5.0 && git push origin v0.5.0`). O GitHub Actions compila e publica:

- `GameStream.zip`: aplicativo e player (pacote distribuído aos usuários);
- `gamestream.exe` e `gamestream.exe.sha256`: utilizados pela atualização automática;
- `ffmpeg.exe.gz` e `ffmpeg.exe.gz.sha256`: obtidos pelo aplicativo na primeira transmissão.

A atualização automática e o download do FFmpeg dependem das releases públicas deste repositório.

## Componentes de terceiros

O pacote distribuído utiliza o FFmpeg e o mpv, ambos sob licença GPL. Consulte [THIRD-PARTY.txt](THIRD-PARTY.txt).
