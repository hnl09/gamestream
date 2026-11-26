//! Interface gráfica (egui): aba Transmitir e aba Assistir, com log embaixo.

use std::time::Duration;

use eframe::egui;
use tokio::{sync::oneshot, task::JoinHandle};

use crate::{
    host, net,
    sources::{self, MonitorInfo, Source, WindowInfo},
    state, viewer,
};

pub fn run() -> eframe::Result<()> {
    let opts = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("GameStream")
            .with_inner_size([600.0, 760.0])
            .with_min_inner_size([480.0, 520.0]),
        ..Default::default()
    };
    eframe::run_native("GameStream", opts, Box::new(|_cc| Ok(Box::new(App::new()))))
}

#[derive(PartialEq, Clone, Copy)]
enum Tab {
    Host,
    View,
}

#[derive(PartialEq, Clone, Copy)]
enum SrcKind {
    Monitor,
    Window,
}

struct Task {
    stop: Option<oneshot::Sender<()>>,
    handle: JoinHandle<()>,
}

impl Task {
    fn running(&self) -> bool {
        !self.handle.is_finished()
    }
    fn stop(&mut self) {
        if let Some(s) = self.stop.take() {
            let _ = s.send(());
        }
    }
}

struct App {
    rt: tokio::runtime::Runtime,
    tab: Tab,
    // host
    src_kind: SrcKind,
    monitors: Vec<MonitorInfo>,
    windows: Vec<WindowInfo>,
    sel_monitor: usize,
    sel_window: Option<(isize, String)>,
    auto_bitrate: bool,
    bitrate: u32,
    fps: u32,
    audio: bool,
    upnp: bool,
    encoder: String,
    password: String,
    friends: u32,
    host_task: Option<Task>,
    // viewer
    invite: String,
    view_password: String,
    view_task: Option<Task>,
}

impl App {
    fn new() -> Self {
        let mut a = App {
            rt: tokio::runtime::Builder::new_multi_thread().enable_all().build().expect("tokio"),
            tab: Tab::Host,
            src_kind: SrcKind::Monitor,
            monitors: vec![],
            windows: vec![],
            sel_monitor: 0,
            sel_window: None,
            auto_bitrate: true,
            bitrate: 20,
            fps: 60,
            audio: true,
            upnp: true,
            encoder: "auto".into(),
            password: String::new(),
            friends: 4,
            host_task: None,
            invite: String::new(),
            view_password: String::new(),
            view_task: None,
        };
        a.refresh_sources();
        a
    }

    fn refresh_sources(&mut self) {
        self.monitors = sources::monitors();
        self.windows = sources::windows();
        if self.sel_monitor >= self.monitors.len() {
            self.sel_monitor = 0;
        }
    }

    fn start_host(&mut self) {
        let source = match self.src_kind {
            SrcKind::Monitor => Source::Monitor(self.monitors.get(self.sel_monitor).map(|m| m.idx).unwrap_or(0)),
            SrcKind::Window => match &self.sel_window {
                Some((hwnd, title)) => Source::Window { hwnd: *hwnd, title: title.clone() },
                None => {
                    state::log("Escolha uma janela antes de iniciar.");
                    return;
                }
            },
        };
        let opts = host::HostOpts {
            port: net::DEFAULT_PORT,
            source,
            bitrate_mbps: self.bitrate,
            auto_bitrate: self.auto_bitrate,
            fps: self.fps,
            encoder: self.encoder.clone(),
            audio: self.audio,
            password: (!self.password.is_empty()).then(|| self.password.clone()),
            upnp: self.upnp,
            ffmpeg: "ffmpeg".into(),
        };
        let (tx, rx) = oneshot::channel();
        let handle = self.rt.spawn(async move {
            let _ = host::run(opts, async {
                let _ = rx.await;
            })
            .await;
        });
        self.host_task = Some(Task { stop: Some(tx), handle });
    }

    fn start_view(&mut self) {
        let opts = viewer::ViewOpts {
            invite: self.invite.clone(),
            password: (!self.view_password.is_empty()).then(|| self.view_password.clone()),
            mpv: None,
        };
        let (tx, rx) = oneshot::channel();
        let handle = self.rt.spawn(async move {
            let _ = viewer::run(opts, async {
                let _ = rx.await;
            })
            .await;
        });
        self.view_task = Some(Task { stop: Some(tx), handle });
    }

    fn host_ui(&mut self, ui: &mut egui::Ui) {
        let running = self.host_task.as_ref().is_some_and(|t| t.running());
        let st = state::HOST.lock().unwrap().clone();

        ui.add_enabled_ui(!running, |ui| {
            ui.heading("O que transmitir");
            ui.horizontal(|ui| {
                ui.radio_value(&mut self.src_kind, SrcKind::Monitor, "Monitor inteiro");
                ui.radio_value(&mut self.src_kind, SrcKind::Window, "Só uma janela / jogo");
                if ui.button("⟳ Atualizar").clicked() {
                    self.refresh_sources();
                }
            });
            match self.src_kind {
                SrcKind::Monitor => {
                    let sel = self.monitors.get(self.sel_monitor).map(|m| m.label()).unwrap_or("Monitor 1".into());
                    egui::ComboBox::from_id_salt("monitor").width(380.0).selected_text(sel).show_ui(ui, |ui| {
                        for (i, m) in self.monitors.iter().enumerate() {
                            ui.selectable_value(&mut self.sel_monitor, i, m.label());
                        }
                    });
                }
                SrcKind::Window => {
                    let sel = self.sel_window.as_ref().map(|(_, t)| t.clone()).unwrap_or("Escolha a janela do jogo...".into());
                    egui::ComboBox::from_id_salt("window").width(380.0).selected_text(sel).show_ui(ui, |ui| {
                        for w in &self.windows {
                            let v = Some((w.hwnd, w.title.clone()));
                            ui.selectable_value(&mut self.sel_window, v, &w.title);
                        }
                    });
                    ui.label(
                        egui::RichText::new(
                            "Captura a área da janela. Deixe o jogo visível (borderless de preferência); \
                             o que ficar por cima dele também aparece. O áudio é o do PC inteiro.",
                        )
                        .small()
                        .weak(),
                    );
                }
            }

            ui.add_space(8.0);
            ui.heading("Qualidade");
            ui.horizontal(|ui| {
                ui.radio_value(&mut self.auto_bitrate, true, "Automático");
                ui.radio_value(&mut self.auto_bitrate, false, "Fixo");
            });
            let label = if self.auto_bitrate { "Mbps no máximo" } else { "Mbps" };
            ui.add(egui::Slider::new(&mut self.bitrate, 3..=60).text(label));
            ui.label(
                egui::RichText::new(if self.auto_bitrate {
                    "Começa em 70% do máximo, cai sozinho se alguém tiver perda de pacotes ou travar, e volta a subir quando a conexão estabiliza. Com vários amigos, segue o de internet mais fraca."
                } else {
                    "Sempre o mesmo bitrate. Se a internet de alguém não aguentar, a imagem dele trava/pula."
                })
                .small()
                .weak(),
            );
            ui.horizontal(|ui| {
                ui.label("FPS:");
                for f in [30, 60, 120] {
                    ui.selectable_value(&mut self.fps, f, f.to_string());
                }
                ui.separator();
                ui.checkbox(&mut self.audio, "Transmitir áudio");
            });
            egui::CollapsingHeader::new("Guia: qual bitrate escolher?").show(ui, |ui| bitrate_guide(ui, self.bitrate, &mut self.friends));

            ui.add_space(8.0);
            ui.heading("Acesso");
            ui.horizontal(|ui| {
                ui.label("Senha:");
                ui.add(egui::TextEdit::singleline(&mut self.password).password(true).hint_text("opcional, mas recomendado"));
            });
            ui.checkbox(&mut self.upnp, "Tentar abrir a porta no roteador automaticamente (UPnP)");
            egui::CollapsingHeader::new("Avançado").show(ui, |ui| {
                egui::ComboBox::from_label("Encoder").selected_text(&self.encoder).show_ui(ui, |ui| {
                    for e in ["auto", "amf", "nvenc", "qsv", "cpu"] {
                        ui.selectable_value(&mut self.encoder, e.to_string(), e);
                    }
                });
            });
        });

        ui.add_space(10.0);
        if running {
            if ui.add_sized([220.0, 34.0], egui::Button::new("■  Parar transmissão")).clicked() {
                if let Some(t) = &mut self.host_task {
                    t.stop();
                }
            }
            ui.add_space(6.0);
            if st.encoder.is_empty() {
                ui.label("Preparando (detectando encoder)...");
            } else {
                ui.label(format!(
                    "Encoder: {}   |   alvo {} Mbps   |   saindo agora {:.1} Mbps",
                    st.encoder, st.bitrate, st.mbps
                ));
                ui.label(format!(
                    "{} assistindo   |   seu upload em uso ≈ {:.1} Mbps",
                    st.viewers.len(),
                    st.mbps * st.viewers.len() as f64
                ));
                for v in &st.viewers {
                    ui.label(format!("  • {v}"));
                }
            }
            if let Some(code) = &st.invite {
                ui.add_space(6.0);
                ui.strong("Convite (mande para os amigos):");
                ui.add(egui::TextEdit::multiline(&mut code.as_str()).desired_rows(2).desired_width(f32::INFINITY));
                if ui.button("Copiar convite").clicked() {
                    ui.output_mut(|o| o.copied_text = code.clone());
                }
                ui.label(format!("Código de segurança: {}", st.security_code));
                ui.label(
                    egui::RichText::new(
                        "Mande o convite por um canal confiável (DM). Quem assiste vê o mesmo código: \
                         se for diferente, alguém está interceptando.",
                    )
                    .small()
                    .weak(),
                );
                for r in &st.reach {
                    ui.label(egui::RichText::new(r).small());
                }
            }
        } else if ui.add_sized([220.0, 34.0], egui::Button::new("▶  Iniciar transmissão")).clicked() {
            self.start_host();
        }
    }

    fn view_ui(&mut self, ui: &mut egui::Ui) {
        let running = self.view_task.as_ref().is_some_and(|t| t.running());
        ui.heading("Assistir um amigo");
        ui.add_enabled_ui(!running, |ui| {
            ui.label("Convite (GS-...) ou IP:");
            ui.add(
                egui::TextEdit::multiline(&mut self.invite)
                    .desired_rows(2)
                    .desired_width(f32::INFINITY)
                    .hint_text("cole aqui o convite que o amigo mandou"),
            );
            ui.horizontal(|ui| {
                ui.label("Senha:");
                ui.add(egui::TextEdit::singleline(&mut self.view_password).password(true));
            });
        });
        ui.add_space(8.0);
        if running {
            if ui.add_sized([180.0, 34.0], egui::Button::new("■  Desconectar")).clicked() {
                if let Some(t) = &mut self.view_task {
                    t.stop();
                }
            }
        } else if ui
            .add_enabled(!self.invite.trim().is_empty(), egui::Button::new("▶  Conectar").min_size([180.0, 34.0].into()))
            .clicked()
        {
            self.start_view();
        }
        ui.add_space(6.0);
        ui.label(state::VIEW.lock().unwrap().text.clone());
        ui.add_space(6.0);
        ui.label(
            egui::RichText::new("O vídeo abre numa janela separada. Atalhos nela: F = tela cheia, Q = fechar.")
                .small()
                .weak(),
        );
    }
}

fn bitrate_guide(ui: &mut egui::Ui, chosen: u32, friends: &mut u32) {
    egui::Grid::new("guide").striped(true).num_columns(2).show(ui, |ui| {
        ui.strong("Mbps");
        ui.strong("Qualidade esperada (HEVC)");
        ui.end_row();
        for (r, q) in [
            ("3–5", "720p ok; 1080p borra em movimento rápido"),
            ("8–10", "1080p60 boa"),
            ("15", "1080p60 muito boa (recomendado)"),
            ("20–25", "1080p excelente / 1440p60 boa / ultrawide boa"),
            ("30–40", "1440p excelente / 4K60 boa"),
            ("50–60", "4K excelente (ideal só em rede local)"),
        ] {
            ui.label(r);
            ui.label(q);
            ui.end_row();
        }
    });
    ui.add_space(4.0);
    ui.label("Internet necessária:");
    ui.label(format!("  • Cada amigo: download de pelo menos {:.0} Mbps (bitrate + 30%).", chosen as f64 * 1.3));
    ui.horizontal(|ui| {
        ui.label("  • Você, com");
        ui.add(egui::DragValue::new(friends).range(1..=16));
        ui.label(format!(
            "amigo(s): upload de pelo menos {:.0} Mbps (bitrate × amigos + 20%).",
            (chosen * *friends) as f64 * 1.2
        ));
    });
    ui.label(egui::RichText::new("Dica: rode um speedtest e use o número de UPLOAD, não o de download.").small().weak());
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        ctx.request_repaint_after(Duration::from_millis(300));
        egui::TopBottomPanel::top("tabs").show(ctx, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.tab, Tab::Host, egui::RichText::new("  Transmitir  ").heading());
                ui.selectable_value(&mut self.tab, Tab::View, egui::RichText::new("  Assistir  ").heading());
            });
            ui.add_space(2.0);
        });
        egui::TopBottomPanel::bottom("log").resizable(true).default_height(140.0).show(ctx, |ui| {
            ui.add_space(4.0);
            ui.label(egui::RichText::new("Log").strong());
            egui::ScrollArea::vertical().stick_to_bottom(true).auto_shrink([false, false]).show(ui, |ui| {
                for l in state::LOG.lock().unwrap().iter() {
                    ui.label(egui::RichText::new(l).monospace().small());
                }
            });
        });
        egui::CentralPanel::default().show(ctx, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| match self.tab {
                Tab::Host => self.host_ui(ui),
                Tab::View => self.view_ui(ui),
            });
        });
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        if let Some(t) = &mut self.host_task {
            t.stop();
        }
        if let Some(t) = &mut self.view_task {
            t.stop();
        }
        // Dá um instante para fechar o UPnP e os players.
        std::thread::sleep(Duration::from_millis(500));
    }
}
