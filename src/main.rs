// Sem janela de console quando aberto com dois cliques (a interface é gráfica).
#![windows_subsystem = "windows"]

mod audio;
mod gui;
mod host;
mod i18n;
mod invite;
mod net;
mod procaudio;
mod room;
mod settings;
mod sources;
mod state;
mod updater;
mod viewer;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "gamestream", version, about = "Transmissão de vídeo e áudio com baixa latência (HEVC acelerado por GPU + QUIC)")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Inicia uma transmissão (monitor ou janela, com áudio)
    Host {
        #[arg(long, default_value_t = net::DEFAULT_PORT)]
        port: u16,
        /// Bitrate em Mbps (fixo) ou máximo, com --auto
        #[arg(long, default_value_t = 20)]
        bitrate: u32,
        /// Ajusta o bitrate automaticamente conforme a conexão dos espectadores
        #[arg(long)]
        auto: bool,
        #[arg(long, default_value_t = 60)]
        fps: u32,
        /// Número do monitor (1 = primeiro)
        #[arg(long, default_value_t = 1)]
        monitor: u32,
        /// Transmite somente a janela cujo título contém este texto
        #[arg(long)]
        window: Option<String>,
        /// auto | amf | nvenc | qsv | cpu
        #[arg(long, default_value = "auto")]
        encoder: String,
        #[arg(long)]
        no_audio: bool,
        /// Fonte de áudio: game (aplicativo da janela) | no-discord (sistema exceto Discord, padrão) |
        /// all (todo o sistema) | app:NOME.exe | output:DISPOSITIVO | input:DISPOSITIVO
        #[arg(long, default_value = "auto")]
        audio: String,
        /// Inclui o microfone (nome do dispositivo de entrada). Não use durante chamadas no Discord.
        #[arg(long)]
        mic: Option<String>,
        #[arg(long)]
        no_upnp: bool,
        /// Não usar o servidor de encontro (sem código de sala nem conexão automática)
        #[arg(long)]
        no_room: bool,
        /// Áudio sem compressão (PCM) em vez de Opus 160 kbps
        #[arg(long)]
        pcm: bool,
        /// Não abrir a janela de pré-visualização
        #[arg(long)]
        no_preview: bool,
        #[arg(long)]
        password: Option<String>,
        #[arg(long, default_value = "ffmpeg")]
        ffmpeg: String,
    },
    /// Conecta a uma transmissão
    View {
        /// Convite GS-... ou endereço IP do transmissor
        invite: String,
        #[arg(long)]
        password: Option<String>,
        #[arg(long)]
        mpv: Option<String>,
        /// Dispositivo de saída de áudio (padrão do Windows se omitido)
        #[arg(long)]
        audio_out: Option<String>,
    },
    /// Lista monitores, janelas e dispositivos de áudio disponíveis
    Sources,
    /// Verifica e instala a versão mais recente do GameStream
    Update,
    /// Mede o upload até o servidor de encontro e sugere o bitrate
    Speedtest {
        /// Número de espectadores esperados
        #[arg(long, default_value_t = 1)]
        viewers: u32,
    },
    /// Verifica o FFmpeg (faz o download se necessário) e exibe o caminho utilizado
    Ffmpeg,
    /// Testa a captura de áudio, exibindo o nível medido por alguns segundos
    AudioTest {
        /// Mesmo formato do --audio do host
        #[arg(long, default_value = "no-discord")]
        audio: String,
        /// Janela (para --audio game)
        #[arg(long)]
        window: Option<String>,
        #[arg(long)]
        mic: Option<String>,
        #[arg(long, default_value_t = 4)]
        secs: u32,
    },
}

fn main() {
    // Provedor de criptografia único para todo o app (QUIC, HTTPS e WebSocket).
    let _ = rustls::crypto::ring::default_provider().install_default();
    unsafe {
        use windows::Win32::UI::HiDpi::{SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2};
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
    if std::env::args().len() > 1 {
        // Rodando pelo terminal: reaproveita o console de quem chamou.
        // Só se a entrada/saída não estiverem redirecionadas (pipe/arquivo): anexar ao console
        // nesse caso trocaria os handles redirecionados pelos do console.
        unsafe {
            use windows::Win32::System::Console::{AttachConsole, GetStdHandle, ATTACH_PARENT_PROCESS, STD_OUTPUT_HANDLE};
            let redirected = GetStdHandle(STD_OUTPUT_HANDLE).is_ok_and(|h| !h.is_invalid() && !h.0.is_null());
            if !redirected {
                let _ = AttachConsole(ATTACH_PARENT_PROCESS);
            }
        }
        if let Err(e) = cli() {
            eprintln!("Erro: {e:#}");
            std::process::exit(1);
        }
    } else {
        updater::cleanup_old();
        if let Err(e) = gui::run() {
            eprintln!("Erro na interface: {e}");
        }
    }
}

fn cli() -> Result<()> {
    let cli = Cli::parse();
    let rt = tokio::runtime::Runtime::new()?;
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    match cli.cmd {
        Cmd::Sources => {
            println!("Monitores:");
            for m in sources::monitors() {
                println!("  {}  [{}]  --monitor {}", m.label(), m.name, m.idx + 1);
            }
            println!("Janelas:");
            for w in sources::windows() {
                println!("  {}", w.title);
            }
            println!("Saídas de áudio:");
            for d in audio::output_devices() {
                println!("  {d}");
            }
            println!("Entradas de áudio:");
            for d in audio::input_devices() {
                println!("  {d}");
            }
            println!("Aplicativos com áudio ativo (--audio app:NOME):");
            for (pid, n) in procaudio::audio_apps() {
                println!("  {n}  (pid {pid})");
            }
            match procaudio::discord_root_pid() {
                Some(p) => println!("Discord em execução (pid {p}): --audio no-discord o exclui da captura."),
                None => println!("Discord não está em execução."),
            }
            Ok(())
        }
        Cmd::Speedtest { viewers } => {
            let server = room::server().context("servidor de encontro não configurado")?;
            println!("Medindo o upload por ~6 s...");
            let mbps = room::speedtest(&server, 6.0)?;
            let suggested = ((mbps * 0.8 / viewers.max(1) as f64).floor() as u32).clamp(3, 60);
            println!("Upload: {mbps:.0} Mbps  ->  bitrate sugerido para {viewers} espectador(es): {suggested} Mbps");
            Ok(())
        }
        Cmd::Update => {
            println!("Versão atual: v{}", updater::VERSION);
            match updater::check()? {
                None => println!("O GameStream já está na versão mais recente."),
                Some(rel) => {
                    println!("Baixando {}...", rel.tag);
                    updater::install(&rel)?;
                    println!("Atualização concluída. Reinicie o GameStream para usar a versão {}.", rel.tag);
                }
            }
            Ok(())
        }
        Cmd::Ffmpeg => {
            let f = updater::ensure_ffmpeg()?;
            println!("FFmpeg: {f}");
            Ok(())
        }
        Cmd::AudioTest { audio: a, window, mic, secs } => {
            let source = match window {
                Some(t) => {
                    let w = sources::find_window(&t).with_context(|| format!("nenhuma janela com \"{t}\" no título"))?;
                    sources::Source::Window { hwnd: w.hwnd, title: w.title }
                }
                None => sources::Source::Monitor(0),
            };
            let (src, desc) = if let Some(name) = a.strip_prefix("except:") {
                // Diagnóstico: tudo menos um programa qualquer (o mesmo mecanismo do "menos o Discord").
                let pid = procaudio::pid_by_exe(name).with_context(|| format!("{name} não está em execução"))?;
                (audio::CaptureSource::ProcessExcept(pid), format!("todo o áudio, exceto {name}"))
            } else {
                let sel = parse_audio(&a, matches!(source, sources::Source::Window { .. }))?;
                host::resolve_audio_sel(&sel, &source)
            };
            println!("Capturando: {desc}");
            let (tx, mut rx) = tokio::sync::broadcast::channel::<bytes::Bytes>(4096);
            let (_pipe, _, _) = audio::start_capture(tx, src, mic.map(|d| (d, 1.0)), false)?;
            rt.block_on(async {
                for s in 1..=secs {
                    let end = tokio::time::Instant::now() + std::time::Duration::from_secs(1);
                    let (mut sum, mut n, mut peak) = (0f64, 0u64, 0i32);
                    while let Ok(Ok(b)) = tokio::time::timeout_at(end, rx.recv()).await {
                        for c in b[4..].chunks_exact(2) {
                            let v = i16::from_le_bytes([c[0], c[1]]) as i32;
                            sum += (v as f64) * (v as f64);
                            n += 1;
                            peak = peak.max(v.abs());
                        }
                    }
                    let rms = if n > 0 { (sum / n as f64).sqrt() / 32768.0 } else { 0.0 };
                    let db = if rms > 0.0 { 20.0 * rms.log10() } else { -120.0 };
                    println!("  {s}s: {:>6.1} dBFS  pico {:>5.1}%  ({} amostras)", db, peak as f64 / 327.68, n);
                }
            });
            Ok(())
        }
        Cmd::Host { port, bitrate, auto, fps, monitor, window, encoder, no_audio, audio, mic, no_upnp, no_room, pcm, no_preview, password, ffmpeg } => {
            if no_preview {
                host::set_preview(false);
            }
            let audio_device = parse_audio(&audio, window.is_some())?;
            let source = match window {
                Some(t) => {
                    let w = sources::find_window(&t).with_context(|| format!("nenhuma janela com \"{t}\" no título"))?;
                    sources::Source::Window { hwnd: w.hwnd, title: w.title }
                }
                None => sources::Source::Monitor(monitor.saturating_sub(1)),
            };
            let opts = host::HostOpts {
                port,
                source,
                bitrate_mbps: bitrate,
                auto_bitrate: auto,
                fps,
                encoder,
                audio: !no_audio,
                audio_device,
                mic,
                password,
                upnp: !no_upnp,
                room: !no_room,
                approval: false,
                audio_pcm: pcm,
                ffmpeg,
            };
            // Códigos de resposta (GS-R-...) colados no terminal liberam espectadores.
            std::thread::spawn(|| {
                let mut line = String::new();
                while std::io::stdin().read_line(&mut line).is_ok_and(|n| n > 0) {
                    // Alguns terminais/programas enviam um BOM (U+FEFF) no início da entrada.
                    let code = line.trim().trim_start_matches('\u{feff}');
                    if code.starts_with("GS-R-") {
                        if let Err(e) = host::release_viewer(code) {
                            state::log(crate::tr!("Código de resposta: {e:#}", "Reply code: {e:#}"));
                        }
                    } else if let Some(n) = code.strip_prefix("monitor ").and_then(|n| n.trim().parse::<u32>().ok()) {
                        // Troca de fonte durante a transmissão: "monitor 2" ou "window <parte do título>".
                        let _ = host::switch_source(sources::Source::Monitor(n.saturating_sub(1)));
                    } else if let Some(t) = code.strip_prefix("window ") {
                        match sources::find_window(t.trim()) {
                            Some(w) => {
                                let _ = host::switch_source(sources::Source::Window { hwnd: w.hwnd, title: w.title });
                            }
                            None => state::log(crate::tr!("nenhuma janela com \"{t}\" no título", "no window with \"{t}\" in its title")),
                        }
                    }
                    line.clear();
                }
            });
            // Erros já aparecem no log; só sinaliza o código de saída.
            if rt.block_on(host::run(opts, ctrl_c)).is_err() {
                std::process::exit(1);
            }
            Ok(())
        }
        Cmd::View { invite, password, mpv, audio_out } => {
            let opts = viewer::ViewOpts { invite, password, mpv, audio_device: audio_out };
            if rt.block_on(viewer::run(opts, ctrl_c)).is_err() {
                std::process::exit(1);
            }
            Ok(())
        }
    }
}

fn parse_audio(a: &str, window: bool) -> Result<audio::AudioSel> {
    Ok(match a {
        "game" => audio::AudioSel::GameWindow,
        "all" => audio::AudioSel::DefaultOutput,
        "no-discord" => audio::AudioSel::AllExceptDiscord,
        "auto" if window => audio::AudioSel::GameWindow,
        "auto" => audio::AudioSel::AllExceptDiscord,
        a => match a.split_once(':') {
            Some(("app", n)) => audio::AudioSel::App { pid: 0, name: n.to_string() },
            Some(("output", n)) => audio::AudioSel::Output(n.to_string()),
            Some(("input", n)) => audio::AudioSel::Input(n.to_string()),
            _ => anyhow::bail!("--audio inválido: {a}"),
        },
    })
}
