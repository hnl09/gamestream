// Sem janela de console quando aberto com dois cliques (a interface é gráfica).
#![windows_subsystem = "windows"]

mod audio;
mod gui;
mod host;
mod invite;
mod net;
mod sources;
mod state;
mod viewer;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "gamestream", version, about = "Stream de jogos com baixa latência entre amigos (HEVC na GPU + QUIC)")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Transmite a sua tela (ou uma janela) + áudio
    Host {
        #[arg(long, default_value_t = net::DEFAULT_PORT)]
        port: u16,
        /// Bitrate em Mbps (fixo) ou o teto, com --auto
        #[arg(long, default_value_t = 20)]
        bitrate: u32,
        /// Ajusta o bitrate sozinho conforme a internet dos viewers
        #[arg(long)]
        auto: bool,
        #[arg(long, default_value_t = 60)]
        fps: u32,
        /// Número do monitor (1 = primeiro)
        #[arg(long, default_value_t = 1)]
        monitor: u32,
        /// Transmite só a janela cujo título contém este texto
        #[arg(long)]
        window: Option<String>,
        /// auto | amf | nvenc | qsv | cpu
        #[arg(long, default_value = "auto")]
        encoder: String,
        #[arg(long)]
        no_audio: bool,
        /// Nome do dispositivo de áudio a capturar (saída ou entrada; veja `gamestream sources`)
        #[arg(long)]
        audio_device: Option<String>,
        #[arg(long)]
        no_upnp: bool,
        #[arg(long)]
        password: Option<String>,
        #[arg(long, default_value = "ffmpeg")]
        ffmpeg: String,
    },
    /// Assiste a transmissão de alguém
    View {
        /// Convite GS-... ou IP do host
        invite: String,
        #[arg(long)]
        password: Option<String>,
        #[arg(long)]
        mpv: Option<String>,
        /// Saída de áudio onde tocar o som (padrão do Windows se omitido)
        #[arg(long)]
        audio_out: Option<String>,
    },
    /// Lista monitores e janelas que podem ser transmitidos
    Sources,
}

fn main() {
    unsafe {
        use windows::Win32::UI::HiDpi::{SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2};
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
    if std::env::args().len() > 1 {
        // Rodando pelo terminal: reaproveita o console de quem chamou.
        unsafe {
            use windows::Win32::System::Console::{AttachConsole, ATTACH_PARENT_PROCESS};
            let _ = AttachConsole(ATTACH_PARENT_PROCESS);
        }
        if let Err(e) = cli() {
            eprintln!("Erro: {e:#}");
            std::process::exit(1);
        }
    } else if let Err(e) = gui::run() {
        eprintln!("Erro na interface: {e}");
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
            Ok(())
        }
        Cmd::Host { port, bitrate, auto, fps, monitor, window, encoder, no_audio, audio_device, no_upnp, password, ffmpeg } => {
            let audio_device = match audio_device {
                None => audio::AudioSel::DefaultOutput,
                Some(n) if audio::input_devices().contains(&n) && !audio::output_devices().contains(&n) => {
                    audio::AudioSel::Input(n)
                }
                Some(n) => audio::AudioSel::Output(n),
            };
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
                password,
                upnp: !no_upnp,
                ffmpeg,
            };
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
