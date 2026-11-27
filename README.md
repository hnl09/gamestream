<div align="center">

# GameStream

**Transmissão de jogos e telas com baixa latência para Windows.**
Vídeo HEVC acelerado pela placa de vídeo, conexão direta e criptografada entre computadores, sem servidores intermediários e sem cadastro.

[![Versão](https://img.shields.io/github/v/release/hnl09/gamestream?label=vers%C3%A3o)](https://github.com/hnl09/gamestream/releases/latest)
[![Downloads](https://img.shields.io/github/downloads/hnl09/gamestream/total?label=downloads)](https://github.com/hnl09/gamestream/releases)
[![Licença](https://img.shields.io/github/license/hnl09/gamestream?label=licen%C3%A7a)](LICENSE)
![Plataforma](https://img.shields.io/badge/plataforma-Windows%2010%20%7C%2011-0078D6)

[**Baixar a versão mais recente**](https://github.com/hnl09/gamestream/releases/latest)

<sub>*Low-latency game and screen streaming for Windows (GPU-accelerated HEVC over encrypted QUIC). The interface is available in English and Brazilian Portuguese.*</sub>

<img src="docs/janela-de-video.png" alt="Janela de vídeo com painel de controles e estatísticas" width="820">

</div>

---

## Recursos

- **Baixa latência e alta qualidade**: vídeo HEVC codificado pela placa de vídeo (AMD, NVIDIA ou Intel, com detecção automática) e transmitido por QUIC/UDP.
- **Monitor ou janela específica**: a captura de janela é nativa do Windows e continua funcionando quando a janela é movida, redimensionada ou sobreposta.
- **Áudio sob controle**: transmita apenas o áudio do jogo ou todo o áudio do sistema **exceto o Discord**, evitando que participantes de uma chamada de voz ouçam o próprio retorno. Microfone opcional. Áudio comprimido com **Opus a 160 kbps** (~0,16 Mbps por espectador), com opção sem compressão.
- **Vários espectadores ao mesmo tempo**: o vídeo é codificado uma única vez e distribuído a todos.
- **Bitrate adaptativo**: a qualidade se ajusta automaticamente à conexão dos espectadores, ou pode ser fixada manualmente. Uma perda de pacotes pequena e constante (Wi-Fi, rede da operadora) não derruba mais a qualidade: o controle de congestionamento BBR reenvia o que se perde e o bitrate só cai quando a conexão realmente não dá conta.
- **Código de sala curto** (ex.: `KX7-92B`) e **conexão automática**: o espectador digita o código e o aplicativo libera a conexão pelo firewall do transmissor sozinho, sem configurar o roteador. Também funciona por IPv6, UPnP, código de resposta ou VPN.
- **Sala de espera**: aprove cada espectador antes de exibir a transmissão e remova quem quiser durante a sessão. Quem você já aprovou entra direto nas próximas transmissões; "Remover" (ou "Esquecer todos") faz a pessoa voltar a passar pela sala de espera.
- **Teste de conexão** opcional, que mede o upload e sugere o bitrate.
- **Troca de monitor ou janela durante a transmissão**, sem desconectar os espectadores, e recuperação automática quando um jogo muda o modo de tela (se a captura do monitor falhar seguidamente, o aplicativo alterna sozinho para a Windows Graphics Capture).
- **Segurança**: criptografia TLS 1.3, verificação da identidade do transmissor pelo convite (proteção contra interceptação), senha e código de segurança.
- **Controles na janela de vídeo**: volume, zoom a partir do cursor, tela cheia e estatísticas em tempo real (ping, FPS, perda de pacotes e taxa recebida).
- **Chat e reações em cima do vídeo**: todos os conectados veem as mensagens e as reações (👍 😂 🔥 ❤ 😮 GG) na hora, na janela do vídeo e no aplicativo. Enter escreve no chat; as teclas 1 a 6 enviam reações.
- **Quem está na sala**: avatares de todos os conectados no canto do vídeo, como no Discord (passe o mouse para ver o nome). Cada pessoa escolhe um **nome** (até 24 caracteres, com emojis) e uma **foto** (PNG, JPG, WebP ou GIF de até 8 MB, reduzida para 128×128). A foto fica no seu PC e vai direto para quem está na sala, sem servidor.
- **Jogar junto (controle remoto, opcional)**: o espectador pede para jogar com o controle ou com teclado e mouse, e o transmissor permite ou revoga a qualquer momento. O controle vira um controle de Xbox 360 virtual no PC do transmissor (requer o driver [ViGEmBus](https://github.com/nefarius/ViGEmBus/releases)); teclado e mouse funcionam sem instalar nada (F8 liga e desliga na janela do vídeo).
- **Reconexão automática**: se a conexão cair, o espectador volta sozinho em alguns segundos, na mesma janela de vídeo e sem passar de novo pela sala de espera.
- **Salas recentes**: as salas em que você entrou ficam na aba Assistir, com a foto de quem transmite e o selo **AO VIVO** quando a pessoa está transmitindo; entre com um clique.
- **Lembra as suas escolhas**: fonte, áudio, qualidade e demais opções voltam como você deixou (a senha nunca é salva).
- **Painel por espectador**: ping, perda de pacotes e taxa de cada espectador, além dos pedidos de controle.
- **Pré-visualização local** da transmissão, sem áudio.
- **Atualização automática, como no Discord**: ao abrir, o aplicativo instala a versão nova (conferida por SHA-256) e reabre sozinho. Com o app aberto, só atualiza se não houver transmissão nem conexão ativa. Ao conectar, transmissor e espectador trocam a versão e o app avisa quando um dos lados está desatualizado.
- **Português e inglês**, com três temas visuais (Padrão, Agente e Bioameaça), selecionáveis no menu ⚙.

<table>
  <tr>
    <td align="center"><img src="docs/transmitir.png" alt="Configuração da transmissão" width="300"><br><sub>Configuração da transmissão</sub></td>
    <td align="center"><img src="docs/ao-vivo.png" alt="Transmissão em andamento" width="300"><br><sub>Transmissão em andamento</sub></td>
    <td align="center"><img src="docs/assistir.png" alt="Conexão a uma transmissão" width="300"><br><sub>Conexão a uma transmissão</sub></td>
  </tr>
</table>

## Instalação

1. Baixe o **`GameStream.zip`** na página de [Releases](https://github.com/hnl09/gamestream/releases/latest) (em *Assets*).
2. Extraia o conteúdo para uma pasta com permissão de escrita (por exemplo, `Documentos\GameStream`).
3. Execute o **`gamestream.exe`**.

O aplicativo é portátil: não há instalador nem alterações no sistema. Para removê-lo, basta apagar a pasta.

> **Aviso do Windows SmartScreen:** por não possuir assinatura digital, o Windows pode exibir *"O Windows protegeu o computador"* na primeira execução. Clique em **Mais informações → Executar assim mesmo**. O código-fonte completo está neste repositório, e cada versão é compilada pelo GitHub Actions a partir dele.

## Requisitos

| | Transmissor | Espectador |
|---|---|---|
| **Sistema** | Windows 10 (versão 2004 ou superior) ou Windows 11, 64 bits | Windows 10 ou 11, 64 bits |
| **Placa de vídeo** | Encoder HEVC por hardware: AMD Radeon RX 400 ou superior, NVIDIA GeForce GTX 10 ou superior, Intel Core 6ª geração ou superior | Decodificação HEVC por hardware recomendada (praticamente todas as placas a partir de 2016) |
| **Conexão** | Upload ≥ bitrate × número de espectadores + 20% | Download ≥ bitrate + 30% |

Sem encoder HEVC por hardware, o transmissor usa codificação por CPU, com desempenho bastante inferior.

## Como usar

### Transmitir

1. Na aba **Transmitir**, escolha a fonte de vídeo (**Monitor** ou **Janela específica**) e a fonte de áudio.
2. Defina a qualidade (**Automático** é o recomendado) e uma **senha**.
3. Clique em **Iniciar transmissão**. Na primeira vez, o encoder de vídeo (FFmpeg, ~90 MB) é baixado automaticamente.
4. Envie aos espectadores o **código da sala** (ex.: `KX7-92B`) ou o convite completo (`GS-...`, copiado automaticamente) e informe a senha por um canal separado.
5. Com a sala de espera ativada, aprove cada espectador no painel **Sala de espera**. Para remover alguém durante a transmissão, use **Remover** na lista de espectadores.

### Assistir

1. Na aba **Assistir**, digite o código da sala ou cole o convite e informe a senha.
2. Clique em **Conectar**. O vídeo é exibido em uma janela própria.

| Na janela de vídeo | Ação |
|---|---|
| Mover o mouse | Exibe o painel de controles |
| Roda do mouse | Zoom a partir do cursor |
| Arrastar (com zoom) | Desloca a imagem |
| Botão do meio | Restaura o tamanho original |
| Duplo clique / `F` | Tela cheia |
| `M` · `9` · `0` | Silenciar · diminuir · aumentar volume |
| `I` | Fixa as estatísticas no canto da tela |

### Guia de bitrate

| Mbps | Qualidade esperada |
|---|---|
| 3–5 | 720p adequado; 1080p perde nitidez em movimentos rápidos |
| 8–10 | 1080p60 boa |
| 15 | 1080p60 muito boa **(recomendado)** |
| 20–25 | 1080p excelente / 1440p60 boa |
| 30–40 | 1440p excelente / 4K60 boa |
| 50–60 | 4K excelente (recomendado apenas em rede local) |

## Conexão pela internet

O GameStream conecta os computadores diretamente. O convite inclui todos os endereços disponíveis, e o espectador utiliza o primeiro que responder:

1. **Sala (automático)**: o espectador informa o código da sala; o servidor de encontro entrega o convite e avisa o transmissor, que libera a conexão pelo firewall do roteador sem nenhuma ação manual. Funciona na maioria das redes domésticas, especialmente com IPv6.
1. **IPv6**: oferecido pela maioria das operadoras. Se transmissor e espectador tiverem IPv6, a conexão costuma funcionar sem nenhuma configuração.
2. **UPnP**: o aplicativo solicita ao roteador a abertura automática da porta UDP 9000.
3. **Código de resposta (hole punching manual)**: se a sala estiver indisponível e a conexão direta não responder, o espectador recebe um código `GS-R-...` para enviar ao transmissor, que o cola em **Liberar espectador**. O transmissor envia pacotes ao espectador, o firewall do roteador passa a aceitar a conexão, e o espectador conecta automaticamente. É especialmente eficaz com IPv6, mesmo quando o modem bloqueia conexões de entrada.
4. **VPN** ([Tailscale](https://tailscale.com) ou [Radmin VPN](https://www.radmin-vpn.com)): funciona em qualquer cenário, inclusive com CGNAT. Com todos na mesma rede virtual, o endereço da VPN entra no convite automaticamente.
5. **Redirecionamento manual** da porta **UDP 9000** no roteador.

Na primeira execução, permita o acesso do GameStream à rede quando o Firewall do Windows solicitar.

## Segurança e privacidade

- **Conexão criptografada**: vídeo, áudio e senha trafegam diretamente entre os computadores por QUIC com TLS 1.3.
- **Proteção contra interceptação**: o convite contém a impressão digital do certificado do transmissor; conexões com qualquer outro computador são recusadas. Transmissor e espectadores podem conferir o mesmo **código de segurança**.
- **Senha**: enviada somente após a verificação da identidade do transmissor. Cada tentativa incorreta é atrasada em 1 segundo.
- **Sem conta e sem coleta de dados**: a transmissão ocorre diretamente entre os computadores. Além dessa conexão, o aplicativo acessa apenas o GitHub (atualizações e download do FFmpeg), o seu roteador (UPnP, se ativado) e o **servidor de encontro** (Cloudflare Worker, código em [`worker/`](worker/)).
- **Proteção contra abuso no servidor de encontro**: limite de pedidos por IP (60 por minuto; teste de conexão, 20), para ninguém esgotar a cota gratuita.
- **Servidor de encontro**: recebe somente os endereços de rede necessários para conectar transmissor e espectador; vídeo, áudio e senha nunca passam por ele, e nada é registrado. O código da sala é derivado da identidade do transmissor, e o aplicativo do espectador confere essa correspondência, de modo que nem o servidor consegue redirecionar a conexão para outro computador.
- **Sala de espera**: com ela ativada, ninguém recebe a transmissão sem a sua aprovação, mesmo conhecendo o código e a senha. Para lembrar quem já foi aprovado, cada espectador apresenta uma chave secreta derivada de um segredo local e da identidade do transmissor (uma chave diferente para cada transmissor); o transmissor guarda apenas o hash dela.
- **Chat, nome e foto**: trafegam pela mesma conexão criptografada, diretamente entre os computadores; nada passa pelo servidor de encontro. Fotos recebidas são validadas (PNG de até 96 KB e 256×256) antes de serem exibidas.
- **Controle remoto**: desligado por padrão. Só funciona depois que o transmissor permite, apenas para o que foi pedido (controle e/ou teclado e mouse), e pode ser revogado a qualquer momento. As teclas Windows nunca são repassadas. Teclas e botões pressionados são soltos automaticamente quando o controle é revogado ou a conexão cai. Do lado do espectador, teclado e mouse só são enviados enquanto o modo de controle está ligado (F8) e a janela do vídeo está em primeiro plano.
- **Endereço IP**: como em qualquer conexão direta, os espectadores conseguem ver o endereço IP do transmissor. Para não expô-lo, utilize uma VPN.
- A identidade do transmissor é um certificado gerado localmente e armazenado em `%APPDATA%\GameStream`.

## Solução de problemas

<details>
<summary><b>"Tempo de conexão esgotado" ao conectar</b></summary>

Não há caminho de rede entre os computadores. Use o **código de resposta** exibido ao espectador (o transmissor deve colá-lo em *Liberar espectador*), verifique se ambos têm IPv6 ou utilize uma VPN (Tailscale ou Radmin VPN). Confirme também que o Firewall do Windows permitiu o GameStream.
</details>

<details>
<summary><b>A imagem trava ou apresenta artefatos</b></summary>

Use o modo de bitrate **Automático** ou reduza o bitrate. Confira a perda de pacotes nas estatísticas da janela de vídeo: valores acima de 2% indicam conexão instável (Wi-Fi fraco ou upload insuficiente do transmissor).
</details>

<details>
<summary><b>A janela capturada fica preta ou parada</b></summary>

Não minimize a janela transmitida. Em jogos, prefira o modo **janela sem bordas** (borderless) em vez de tela cheia exclusiva. Na captura de janela, o FPS diminui quando o conteúdo não muda; isso é esperado.
</details>

<details>
<summary><b>A transmissão falha ao abrir um jogo em tela cheia</b></summary>

Alguns jogos em tela cheia exclusiva (às vezes junto com overlays como o RivaTuner/RTSS) impedem a captura padrão do monitor. O aplicativo reinicia a captura e, se ela falhar duas vezes seguidas, troca sozinho para a Windows Graphics Capture; os espectadores continuam conectados. Se a imagem continuar falhando, coloque o jogo em **tela cheia em janela** (sem bordas) ou transmita a **janela específica** do jogo.
</details>

<details>
<summary><b>Os espectadores não ouvem o áudio</b></summary>

Em **Fonte de áudio**, selecione outra opção (por exemplo, o dispositivo de saída em que o jogo está tocando). Para testar a captura sem iniciar uma transmissão, execute `gamestream audio-test` no terminal.
</details>

<details>
<summary><b>O antivírus bloqueou o aplicativo</b></summary>

Executáveis sem assinatura digital podem gerar alertas falsos. Os binários são compilados pelo GitHub Actions a partir deste código-fonte, e cada versão publica o hash SHA-256 do `gamestream.exe`. Se preferir, compile o aplicativo a partir do código (veja abaixo).
</details>

## Linha de comando

Todas as funções também estão disponíveis pelo terminal:

```powershell
gamestream sources                                        # monitores, janelas e dispositivos de áudio
gamestream host --auto --bitrate 25 --password SENHA      # transmite o monitor principal
gamestream host --window "Nome da janela" --audio game    # transmite uma janela com o áudio dela
gamestream view GS-xxxx --password SENHA                  # conecta a uma transmissão
#   durante a transmissão (digitando no terminal):
#   transmissor: chat <texto> · react 🔥 · allow <nome> pad|kbm|both · revoke · monitor 2 · window <título>
#   espectador:  chat <texto> · react 🔥 · control pad|kbm|both|off
gamestream audio-test --audio no-discord                  # mede o áudio capturado
gamestream update                                         # atualiza para a versão mais recente
```

Use `gamestream --help` ou `gamestream <comando> --help` para ver todas as opções.

## Compilação a partir do código-fonte

Requisitos: [Rust](https://rustup.rs) (toolchain MSVC) e Visual Studio Build Tools com C++. Para testes locais, `ffmpeg` e `mpv` no PATH.

```powershell
git clone https://github.com/hnl09/gamestream.git
cd gamestream
$env:CARGO_TARGET_DIR = "$env:USERPROFILE\.gamestream-target"   # evita erros de caminho longo no Windows
cargo build --release
.\empacotar.ps1   # gera dist\ (teste local) e out\ (arquivos de release)
```

### Arquitetura

```
TRANSMISSOR                                                 ESPECTADOR (×N)
ddagrab / gfxcapture (GPU) ─► HEVC AMF/NVENC/QSV (FFmpeg) ─► QUIC ─► mpv (decodificação por hardware)
WASAPI (por aplicativo + microfone) ─► mixer ─────────────► QUIC ─► cpal (buffer de jitter ~40 ms)
```

| Módulo | Função |
|---|---|
| `src/host.rs` | encoder, distribuição aos espectadores, controle de bitrate |
| `src/viewer.rs` | conexão (múltiplos endereços), player, estatísticas |
| `src/net.rs` | QUIC/TLS, identidade persistente, verificação por pinning |
| `src/invite.rs` | convites, endereços locais/IPv6/VPN, UPnP |
| `src/sources.rs` | monitores e janelas |
| `src/audio.rs` · `src/procaudio.rs` | captura, mixagem e reprodução de áudio; captura por aplicativo |
| `src/updater.rs` | atualização automática e download do FFmpeg |
| `src/gui.rs` | interface gráfica (egui) |
| `assets/overlay.lua` | painel da janela de vídeo |

### Servidor de encontro

O servidor fica em [`worker/`](worker/) e roda no plano gratuito do Cloudflare Workers (Durable Objects com SQLite). Para publicar uma instância própria:

```powershell
cd worker
npm install
npx wrangler login    # entra na sua conta Cloudflare (gratuita)
npx wrangler deploy   # mostra o endereço https://gamestream-rendezvous.<sua-conta>.workers.dev
```

O endereço padrão fica em `DEFAULT_SERVER` (`src/room.rs`) e pode ser substituído pela variável de ambiente `GAMESTREAM_SERVER`. Para testar localmente: `npx wrangler dev`.

### Publicação de versões

Ao enviar uma tag `v*` (por exemplo, `git tag v0.5.0 && git push origin v0.5.0`), o GitHub Actions compila e publica `GameStream.zip`, `gamestream.exe` e `ffmpeg.exe.gz`, cada um com o respectivo arquivo `.sha256`.

## Contribuições

Relatos de problemas e sugestões são bem-vindos na aba [Issues](https://github.com/hnl09/gamestream/issues). Ao relatar um problema, inclua a versão do aplicativo e o conteúdo do **Log** (botão no canto superior direito → *Copiar log*), removendo endereços IP e convites.

## Licença

Distribuído sob a licença [MIT](LICENSE).

O pacote de distribuição utiliza o [FFmpeg](https://ffmpeg.org) e o [mpv](https://mpv.io), executados como processos separados e licenciados sob a GPL. Consulte [THIRD-PARTY.txt](THIRD-PARTY.txt).

Utilize o GameStream de acordo com a legislação aplicável e com os termos de uso do conteúdo transmitido.
