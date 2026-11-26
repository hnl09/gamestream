//! Interface gráfica (egui): aba Transmitir e aba Assistir.

use std::time::Duration;

use eframe::egui::{self, Align, Color32, Layout, RichText, Stroke};
use tokio::{sync::oneshot, task::JoinHandle};

use crate::{
    audio::{self, AudioSel},
    host, net,
    sources::{self, MonitorInfo, Source, WindowInfo},
    state::{self, ViewPhase},
    viewer,
};

const ACCENT: Color32 = Color32::from_rgb(124, 92, 255);
const LIVE: Color32 = Color32::from_rgb(229, 72, 77);
const OK: Color32 = Color32::from_rgb(48, 164, 108);
const BG: Color32 = Color32::from_rgb(22, 23, 28);
const CARD: Color32 = Color32::from_rgb(32, 34, 41);
const BORDER: Color32 = Color32::from_rgb(50, 53, 63);
const MUTED_TXT: Color32 = Color32::from_rgb(150, 154, 168);

pub fn run() -> eframe::Result<()> {
    let opts = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("GameStream")
            .with_inner_size([640.0, 820.0])
            .with_min_inner_size([520.0, 560.0]),
        ..Default::default()
    };
    eframe::run_native(
        "GameStream",
        opts,
        Box::new(|cc| {
            setup_style(&cc.egui_ctx);
            Ok(Box::new(App::new()))
        }),
    )
}

fn setup_style(ctx: &egui::Context) {
    let mut v = egui::Visuals::dark();
    v.panel_fill = BG;
    v.window_fill = CARD;
    v.window_stroke = Stroke::new(1.0_f32, BORDER);
    v.extreme_bg_color = Color32::from_rgb(16, 17, 21);
    v.selection.bg_fill = ACCENT;
    v.hyperlink_color = ACCENT;
    v.widgets.hovered.weak_bg_fill = Color32::from_rgb(52, 55, 66);
    v.widgets.inactive.weak_bg_fill = Color32::from_rgb(42, 44, 53);
    v.widgets.inactive.rounding = egui::Rounding::same(6.0);
    v.widgets.hovered.rounding = egui::Rounding::same(6.0);
    v.widgets.active.rounding = egui::Rounding::same(6.0);
    ctx.set_visuals(v);

    let mut style = (*ctx.style()).clone();
    use egui::{FontId, TextStyle};
    style.text_styles.insert(TextStyle::Heading, FontId::proportional(22.0));
    style.text_styles.insert(TextStyle::Body, FontId::proportional(15.0));
    style.text_styles.insert(TextStyle::Button, FontId::proportional(15.0));
    style.text_styles.insert(TextStyle::Small, FontId::proportional(12.5));
    style.text_styles.insert(TextStyle::Monospace, FontId::monospace(13.0));
    style.spacing.item_spacing = egui::vec2(8.0, 8.0);
    style.spacing.button_padding = egui::vec2(12.0, 6.0);
    style.spacing.slider_width = 260.0;
    ctx.set_style(style);
}

// ---------- componentes visuais ----------

fn card(ui: &mut egui::Ui, title: &str, add: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::none()
        .fill(CARD)
        .stroke(Stroke::new(1.0_f32, BORDER))
        .rounding(10.0_f32)
        .inner_margin(16.0)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            if !title.is_empty() {
                ui.label(RichText::new(title).size(16.0).strong());
                ui.add_space(4.0);
            }
            add(ui);
        });
    ui.add_space(10.0);
}

fn hint(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).small().color(MUTED_TXT));
}

fn big_button(ui: &mut egui::Ui, text: &str, color: Color32) -> egui::Response {
    ui.add_sized(
        [ui.available_width(), 46.0],
        egui::Button::new(RichText::new(text).size(17.0).strong().color(Color32::WHITE))
            .fill(color)
            .rounding(8.0_f32),
    )
}

fn stat(ui: &mut egui::Ui, label: &str, value: String) {
    ui.vertical(|ui| {
        ui.label(RichText::new(label).small().color(MUTED_TXT));
        ui.label(RichText::new(value).size(20.0).strong());
    });
}

fn error_box(ui: &mut egui::Ui, msg: &str) {
    egui::Frame::none()
        .fill(Color32::from_rgb(60, 24, 28))
        .stroke(Stroke::new(1.0_f32, LIVE))
        .rounding(8.0_f32)
        .inner_margin(12.0)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(format!("⚠  {msg}")).color(Color32::from_rgb(255, 200, 200)));
        });
    ui.add_space(10.0);
}

fn pill(ui: &mut egui::Ui, text: &str, color: Color32) {
    egui::Frame::none()
        .fill(color.gamma_multiply(0.25))
        .stroke(Stroke::new(1.0_f32, color))
        .rounding(20.0_f32)
        .inner_margin(egui::Margin::symmetric(10.0, 3.0))
        .show(ui, |ui| {
            // Desenhado à mão: funciona igual em layouts da esquerda ou da direita.
            let font = egui::TextStyle::Small.resolve(ui.style());
            let galley = ui.painter().layout_no_wrap(text.to_string(), font, color);
            let size = egui::vec2(14.0 + galley.size().x, galley.size().y.max(10.0));
            let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
            // Bolinha pulsando (a fonte padrão não tem "●").
            let t = ui.input(|i| i.time);
            let a = 0.55 + 0.45 * ((t * 3.0).sin() as f32).abs();
            ui.painter().circle_filled(egui::pos2(rect.left() + 4.0, rect.center().y), 4.0, color.gamma_multiply(a));
            ui.painter().galley(egui::pos2(rect.left() + 14.0, rect.center().y - galley.size().y / 2.0), galley, color);
        });
}

// ---------- app ----------

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
    show_log: bool,
    // host
    src_kind: SrcKind,
    monitors: Vec<MonitorInfo>,
    windows: Vec<WindowInfo>,
    sel_monitor: usize,
    sel_window: Option<(isize, String)>,
    out_devices: Vec<String>,
    in_devices: Vec<String>,
    audio: bool,
    audio_sel: AudioSel,
    auto_bitrate: bool,
    bitrate: u32,
    fps: u32,
    upnp: bool,
    encoder: String,
    password: String,
    friends: u32,
    preview: bool,
    host_task: Option<Task>,
    live_desc: String,
    popup: Option<String>,
    popup_done_for: Option<String>,
    // viewer
    invite: String,
    view_password: String,
    view_out: Option<String>,
    volume: f32,
    muted: bool,
    view_task: Option<Task>,
}

impl App {
    fn new() -> Self {
        let mut a = App {
            rt: tokio::runtime::Builder::new_multi_thread().enable_all().build().expect("tokio"),
            tab: Tab::Host,
            show_log: false,
            src_kind: SrcKind::Monitor,
            monitors: vec![],
            windows: vec![],
            sel_monitor: 0,
            sel_window: None,
            out_devices: vec![],
            in_devices: vec![],
            audio: true,
            audio_sel: AudioSel::DefaultOutput,
            auto_bitrate: true,
            bitrate: 20,
            fps: 60,
            upnp: true,
            encoder: "auto".into(),
            password: String::new(),
            friends: 4,
            preview: true,
            host_task: None,
            live_desc: String::new(),
            popup: None,
            popup_done_for: None,
            invite: String::new(),
            view_password: String::new(),
            view_out: None,
            volume: 1.0,
            muted: false,
            view_task: None,
        };
        a.refresh();
        a
    }

    fn refresh(&mut self) {
        self.monitors = sources::monitors();
        self.windows = sources::windows();
        if self.sel_monitor >= self.monitors.len() {
            self.sel_monitor = 0;
        }
        self.out_devices = audio::output_devices();
        self.in_devices = audio::input_devices();
    }

    fn host_running(&self) -> bool {
        self.host_task.as_ref().is_some_and(|t| t.running())
    }

    fn view_running(&self) -> bool {
        self.view_task.as_ref().is_some_and(|t| t.running())
    }

    fn start_host(&mut self) {
        let source = match self.src_kind {
            SrcKind::Monitor => {
                let m = self.monitors.get(self.sel_monitor);
                self.live_desc = m.map(|m| m.label()).unwrap_or("Monitor 1".into());
                Source::Monitor(m.map(|m| m.idx).unwrap_or(0))
            }
            SrcKind::Window => match &self.sel_window {
                Some((hwnd, title)) => {
                    self.live_desc = format!("Janela: {title}");
                    Source::Window { hwnd: *hwnd, title: title.clone() }
                }
                None => {
                    state::host(|h| h.error = Some("Escolha a janela do jogo antes de iniciar.".into()));
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
            audio_device: self.audio_sel.clone(),
            password: (!self.password.is_empty()).then(|| self.password.clone()),
            upnp: self.upnp,
            ffmpeg: "ffmpeg".into(),
        };
        state::host(|h| *h = state::HostStatus::default());
        host::set_preview(self.preview);
        self.popup_done_for = None;
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
        *state::VIEW_CTL.lock().unwrap() =
            state::ViewCtl { volume: self.volume, muted: self.muted, ..Default::default() };
        audio::set_volume(self.volume);
        audio::set_muted(self.muted);
        let opts = viewer::ViewOpts {
            invite: self.invite.clone(),
            password: (!self.view_password.is_empty()).then(|| self.view_password.clone()),
            mpv: None,
            audio_device: self.view_out.clone(),
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

    // ---------- aba Transmitir ----------

    fn host_ui(&mut self, ui: &mut egui::Ui) {
        let st = state::HOST.lock().unwrap().clone();
        if self.host_running() {
            self.host_live_ui(ui, &st);
            return;
        }

        card(ui, "1. O que transmitir", |ui| {
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.src_kind, SrcKind::Monitor, "🖥  Monitor inteiro");
                ui.selectable_value(&mut self.src_kind, SrcKind::Window, "🎮  Só a janela do jogo");
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.button("⟳").on_hover_text("Atualizar listas de monitores, janelas e áudio").clicked() {
                        self.refresh();
                    }
                });
            });
            match self.src_kind {
                SrcKind::Monitor => {
                    let sel = self.monitors.get(self.sel_monitor).map(|m| m.label()).unwrap_or("Monitor 1".into());
                    egui::ComboBox::from_id_salt("monitor")
                        .width(ui.available_width())
                        .selected_text(sel)
                        .show_ui(ui, |ui| {
                            for (i, m) in self.monitors.iter().enumerate() {
                                ui.selectable_value(&mut self.sel_monitor, i, m.label());
                            }
                        });
                }
                SrcKind::Window => {
                    let sel = self.sel_window.as_ref().map(|(_, t)| t.clone()).unwrap_or("Escolha a janela do jogo...".into());
                    egui::ComboBox::from_id_salt("window")
                        .width(ui.available_width())
                        .selected_text(sel)
                        .show_ui(ui, |ui| {
                            for w in &self.windows {
                                ui.selectable_value(&mut self.sel_window, Some((w.hwnd, w.title.clone())), &w.title);
                            }
                        });
                    hint(ui, "Deixe o jogo visível (de preferência em janela sem bordas). O que ficar por cima dele também aparece.");
                }
            }
            ui.add_space(4.0);
            ui.checkbox(&mut self.preview, "👁  Ver minha transmissão (prévia sem som, como no Discord)");
            hint(
                ui,
                "Abre uma janela com exatamente o que os amigos estão vendo. Se ela ficar no mesmo monitor transmitido, \
                 aparece o efeito espelho: arraste a prévia para o outro monitor.",
            );
        });

        card(ui, "2. Áudio", |ui| {
            ui.checkbox(&mut self.audio, "Transmitir áudio");
            ui.add_enabled_ui(self.audio, |ui| {
                ui.label("Capturar o som de:");
                egui::ComboBox::from_id_salt("audio_src")
                    .width(ui.available_width())
                    .selected_text(self.audio_sel.label())
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.audio_sel, AudioSel::DefaultOutput, AudioSel::DefaultOutput.label());
                        ui.separator();
                        ui.label(RichText::new("Saídas (o que toca nelas)").small().color(MUTED_TXT));
                        for d in &self.out_devices {
                            let v = AudioSel::Output(d.clone());
                            let l = v.label();
                            ui.selectable_value(&mut self.audio_sel, v, l);
                        }
                        ui.separator();
                        ui.label(RichText::new("Entradas (microfone, mixagem estéreo...)").small().color(MUTED_TXT));
                        for d in &self.in_devices {
                            let v = AudioSel::Input(d.clone());
                            let l = v.label();
                            ui.selectable_value(&mut self.audio_sel, v, l);
                        }
                    });
                hint(ui, "Escolha a saída onde o jogo está tocando (ex.: fone Realtek ou o monitor). Se ninguém ouvir nada, troque aqui.");
            });
        });

        card(ui, "3. Qualidade", |ui| {
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.auto_bitrate, true, "⚡ Automático");
                ui.selectable_value(&mut self.auto_bitrate, false, "📌 Fixo");
            });
            let label = if self.auto_bitrate { "Mbps no máximo" } else { "Mbps" };
            ui.add(egui::Slider::new(&mut self.bitrate, 3..=60).text(label));
            hint(
                ui,
                if self.auto_bitrate {
                    "Começa em 70% do máximo, cai sozinho se alguém perder pacotes e volta a subir quando a conexão estabiliza. Segue o amigo com a internet mais fraca."
                } else {
                    "Sempre o mesmo bitrate. Se a internet de alguém não aguentar, a imagem dele trava."
                },
            );
            ui.horizontal(|ui| {
                ui.label("FPS:");
                for f in [30, 60, 120] {
                    ui.selectable_value(&mut self.fps, f, f.to_string());
                }
            });
            egui::CollapsingHeader::new("Qual bitrate escolher?").show(ui, |ui| bitrate_guide(ui, self.bitrate, &mut self.friends));
        });

        card(ui, "4. Acesso", |ui| {
            ui.horizontal(|ui| {
                ui.label("Senha:");
                ui.add(
                    egui::TextEdit::singleline(&mut self.password)
                        .password(true)
                        .desired_width(ui.available_width())
                        .hint_text("recomendado"),
                );
            });
            ui.checkbox(&mut self.upnp, "Tentar abrir a porta no roteador (UPnP)");
            egui::CollapsingHeader::new("Avançado").show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label("Encoder:");
                    egui::ComboBox::from_id_salt("enc").selected_text(&self.encoder).show_ui(ui, |ui| {
                        for e in ["auto", "amf", "nvenc", "qsv", "cpu"] {
                            ui.selectable_value(&mut self.encoder, e.to_string(), e);
                        }
                    });
                });
            });
        });

        if let Some(e) = &st.error {
            error_box(ui, e);
        }
    }

    fn host_live_ui(&mut self, ui: &mut egui::Ui, st: &state::HostStatus) {
        if st.invite.is_none() {
            card(ui, "", |ui| {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(RichText::new("Preparando a transmissão...").size(17.0).strong());
                });
                hint(ui, "Detectando o encoder da placa de vídeo e abrindo a conexão. Leva alguns segundos.");
            });
        } else {
            card(ui, "", |ui| {
                ui.horizontal(|ui| {
                    pill(ui, "AO VIVO", LIVE);
                    ui.label(RichText::new(&self.live_desc).strong());
                });
                ui.add_space(6.0);
                ui.columns(4, |c| {
                    stat(&mut c[0], "Assistindo", st.viewers.len().to_string());
                    stat(&mut c[1], "Bitrate alvo", format!("{} Mbps", st.bitrate));
                    stat(&mut c[2], "Saindo agora", format!("{:.1} Mbps", st.mbps));
                    stat(&mut c[3], "Seu upload", format!("{:.0} Mbps", st.mbps * st.viewers.len() as f64));
                });
                ui.horizontal(|ui| {
                    hint(ui, &format!("Encoder: {}", st.encoder));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let label = if st.preview_open { "👁  Fechar prévia" } else { "👁  Ver minha transmissão" };
                        if ui.button(label).clicked() {
                            self.preview = !st.preview_open;
                            host::set_preview(self.preview);
                        }
                    });
                });
            });

            card(ui, "Convite", |ui| {
                ui.horizontal(|ui| {
                    if ui
                        .add(egui::Button::new(RichText::new("📋  Copiar convite").strong().color(Color32::WHITE)).fill(ACCENT))
                        .clicked()
                    {
                        if let Some(code) = &st.invite {
                            ui.output_mut(|o| o.copied_text = code.clone());
                            state::log("Convite copiado.");
                        }
                    }
                    if ui.button("Ver convite").clicked() {
                        self.popup = st.invite.clone();
                    }
                });
                ui.label(format!("🔒 Código de segurança: {}", st.security_code));
                egui::CollapsingHeader::new("Por onde os amigos podem conectar").show(ui, |ui| {
                    for r in &st.reach {
                        hint(ui, r);
                    }
                });
            });

            card(ui, "Quem está assistindo", |ui| {
                if st.preview_open {
                    ui.label(RichText::new("🖥  Você (prévia, sem som)").color(MUTED_TXT));
                }
                if st.viewers.is_empty() {
                    hint(ui, "Nenhum amigo ainda. Mande o convite e a senha.");
                }
                for v in &st.viewers {
                    ui.label(format!("👤  {v}"));
                }
            });
        }
    }

    // ---------- aba Assistir ----------

    fn view_ui(&mut self, ui: &mut egui::Ui) {
        let vs = state::VIEW.lock().unwrap().clone();
        let running = self.view_running();

        if running && vs.phase == ViewPhase::Connected {
            self.view_live_ui(ui, &vs);
            return;
        }

        card(ui, "Entrar numa transmissão", |ui| {
            ui.add_enabled_ui(!running, |ui| {
                ui.label("Convite:");
                ui.add(
                    egui::TextEdit::multiline(&mut self.invite)
                        .desired_rows(3)
                        .desired_width(f32::INFINITY)
                        .font(egui::TextStyle::Monospace)
                        .hint_text("Cole aqui (Ctrl+V) o convite GS-... que o amigo mandou"),
                );
                ui.horizontal(|ui| {
                    ui.label("Senha:");
                    ui.add(egui::TextEdit::singleline(&mut self.view_password).password(true).desired_width(ui.available_width()));
                });
                ui.label("Tocar o som em:");
                let sel = self.view_out.clone().unwrap_or("Padrão do Windows".into());
                egui::ComboBox::from_id_salt("view_out").width(ui.available_width()).selected_text(sel).show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.view_out, None, "Padrão do Windows");
                    for d in &self.out_devices {
                        ui.selectable_value(&mut self.view_out, Some(d.clone()), format!("🔊 {d}"));
                    }
                });
            });
        });

        if vs.phase == ViewPhase::Error && !running {
            error_box(ui, &vs.text);
        }
        if running {
            card(ui, "", |ui| {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(RichText::new("Conectando...").size(17.0).strong());
                });
            });
        }
    }

    fn view_live_ui(&mut self, ui: &mut egui::Ui, vs: &state::ViewStatus) {
        card(ui, "", |ui| {
            ui.horizontal(|ui| {
                pill(ui, "ASSISTINDO", OK);
                ui.label(RichText::new(&vs.peer).strong());
            });
            if vs.verified {
                ui.label(format!("🔒 Criptografado · identidade do host verificada · código {}", vs.code));
            } else {
                ui.label(
                    RichText::new(format!("⚠ Criptografado, mas identidade NÃO verificada (conectou por IP) · código {}", vs.code))
                        .color(Color32::from_rgb(255, 190, 90)),
                );
            }
        });

        // Volume: a mesma informação do painel dentro da janela do vídeo (sincronizado nos dois sentidos).
        let mut ctl = state::VIEW_CTL.lock().unwrap().clone();
        card(ui, "Som", |ui| {
            ui.horizontal(|ui| {
                let label = if ctl.muted { "Ativar som" } else { "Mutar" };
                if ui.button(label).clicked() {
                    ctl.muted = !ctl.muted;
                }
                let mut pct = ctl.volume * 100.0;
                if ui
                    .add_enabled(!ctl.muted, egui::Slider::new(&mut pct, 0.0..=200.0).fixed_decimals(0).suffix("%").text("volume"))
                    .changed()
                {
                    ctl.volume = pct / 100.0;
                }
            });
        });

        card(ui, "Janela do vídeo", |ui| {
            ui.horizontal(|ui| {
                if ui.button("Tela cheia").clicked() {
                    ctl.fullscreen_toggles += 1;
                }
                ui.checkbox(&mut ctl.on_top, "Sempre por cima das outras janelas");
            });
            ui.add_space(4.0);
            ui.label(RichText::new("Controles dentro do vídeo").strong());
            for (k, v) in [
                ("Mexa o mouse", "aparece o painel no canto inferior direito (som, volume, zoom, tela cheia)"),
                ("Scroll", "zoom no ponto onde o mouse está: para cima aproxima, para baixo volta"),
                ("Arrastar", "com zoom, move a imagem"),
                ("Clique do meio", "volta ao tamanho normal"),
                ("Duplo clique / F", "tela cheia"),
                ("M · 9 · 0", "mutar · baixar · aumentar volume"),
            ] {
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new(k).strong().color(ACCENT));
                    ui.label(RichText::new(v).color(MUTED_TXT));
                });
            }
        });

        if ctl != *state::VIEW_CTL.lock().unwrap() {
            audio::set_volume(ctl.volume);
            audio::set_muted(ctl.muted);
        }
        self.volume = ctl.volume;
        self.muted = ctl.muted;
        *state::VIEW_CTL.lock().unwrap() = ctl;
    }

    /// Barra fixa no rodapé com o botão principal da aba atual.
    fn action_bar(&mut self, ui: &mut egui::Ui) {
        match self.tab {
            Tab::Host => {
                if self.host_running() {
                    if big_button(ui, "■   Parar transmissão", LIVE).clicked() {
                        if let Some(t) = &mut self.host_task {
                            t.stop();
                        }
                    }
                } else if big_button(ui, "▶   Iniciar transmissão", ACCENT).clicked() {
                    self.start_host();
                }
            }
            Tab::View => {
                if self.view_running() {
                    let connected = state::VIEW.lock().unwrap().phase == ViewPhase::Connected;
                    let (label, color) =
                        if connected { ("■   Desconectar", LIVE) } else { ("Cancelar", Color32::from_rgb(70, 72, 82)) };
                    if big_button(ui, label, color).clicked() {
                        if let Some(t) = &mut self.view_task {
                            t.stop();
                        }
                    }
                } else {
                    let enabled = !self.invite.trim().is_empty();
                    ui.add_enabled_ui(enabled, |ui| {
                        if big_button(ui, "▶   Conectar", ACCENT).clicked() {
                            self.start_view();
                        }
                    });
                }
            }
        }
    }

    // ---------- popup do convite ----------

    fn invite_popup(&mut self, ctx: &egui::Context) {
        let Some(code) = self.popup.clone() else { return };
        let code_sec = state::HOST.lock().unwrap().security_code.clone();
        // Escurece o fundo.
        ctx.layer_painter(egui::LayerId::new(egui::Order::Middle, egui::Id::new("dim")))
            .rect_filled(ctx.screen_rect(), 0.0, Color32::from_black_alpha(170));
        let mut close = false;
        egui::Window::new("invite_popup")
            .title_bar(false)
            .order(egui::Order::Foreground)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .fixed_size([460.0, 0.0])
            .frame(egui::Frame::window(&ctx.style()).inner_margin(22.0).rounding(12.0_f32))
            .show(ctx, |ui| {
                ui.vertical_centered(|ui| {
                    ui.label(RichText::new("🎉").size(34.0));
                    ui.label(RichText::new("Você está ao vivo!").size(22.0).strong());
                    ui.add_space(4.0);
                    ui.label(RichText::new("✅ Convite copiado para a área de transferência").color(OK).strong());
                    ui.label("Cole no Discord com Ctrl+V e mande a senha separada.");
                });
                ui.add_space(8.0);
                egui::Frame::none()
                    .fill(Color32::from_rgb(16, 17, 21))
                    .rounding(8.0_f32)
                    .inner_margin(10.0)
                    .show(ui, |ui| {
                        ui.add(
                            egui::Label::new(RichText::new(&code).monospace().small().color(MUTED_TXT)).wrap(),
                        );
                    });
                ui.add_space(6.0);
                ui.label(format!("🔒 Código de segurança: {code_sec}"));
                hint(ui, "Quem assiste vê esse mesmo código. Se for diferente, alguém está interceptando.");
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if ui.button("📋 Copiar de novo").clicked() {
                        ui.output_mut(|o| o.copied_text = code.clone());
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui
                            .add(egui::Button::new(RichText::new("   OK   ").strong().color(Color32::WHITE)).fill(ACCENT))
                            .clicked()
                        {
                            close = true;
                        }
                    });
                });
            });
        if close || ctx.input(|i| i.key_pressed(egui::Key::Escape) || i.key_pressed(egui::Key::Enter)) {
            self.popup = None;
        }
    }
}

fn bitrate_guide(ui: &mut egui::Ui, chosen: u32, friends: &mut u32) {
    egui::Grid::new("guide").striped(true).num_columns(2).spacing([16.0, 6.0]).show(ui, |ui| {
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
    ui.add_space(6.0);
    ui.label(format!("• Cada amigo precisa de download de pelo menos {:.0} Mbps.", chosen as f64 * 1.3));
    ui.horizontal(|ui| {
        ui.label("• Com");
        ui.add(egui::DragValue::new(friends).range(1..=16));
        ui.label(format!("amigo(s), seu upload precisa de pelo menos {:.0} Mbps.", (chosen * *friends) as f64 * 1.2));
    });
    hint(ui, "Use o número de UPLOAD do speedtest, não o de download.");
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        ctx.request_repaint_after(Duration::from_millis(250));

        // Abre o popup (e copia o convite) assim que a transmissão fica pronta.
        let invite = state::HOST.lock().unwrap().invite.clone();
        if let Some(code) = invite {
            if self.host_running() && self.popup_done_for.as_ref() != Some(&code) {
                ctx.output_mut(|o| o.copied_text = code.clone());
                state::log("Convite copiado para a área de transferência.");
                self.popup = Some(code.clone());
                self.popup_done_for = Some(code);
                self.tab = Tab::Host;
            }
        }

        let host_live = self.host_running();
        let view_live = self.view_running();
        egui::TopBottomPanel::top("top")
            .frame(egui::Frame::none().fill(Color32::from_rgb(28, 29, 35)).inner_margin(egui::Margin::symmetric(16.0, 10.0)))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("GameStream").size(20.0).strong().color(ACCENT));
                    ui.add_space(12.0);
                    ui.selectable_value(&mut self.tab, Tab::Host, RichText::new("  Transmitir  ").size(16.0));
                    ui.selectable_value(&mut self.tab, Tab::View, RichText::new("  Assistir  ").size(16.0));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.toggle_value(&mut self.show_log, "Log");
                        if host_live {
                            pill(ui, "AO VIVO", LIVE);
                        } else if view_live {
                            pill(ui, "ASSISTINDO", OK);
                        }
                    });
                });
            });

        if self.show_log {
            egui::TopBottomPanel::bottom("log").resizable(true).default_height(180.0).show(ctx, |ui| {
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Detalhes técnicos").strong());
                    if ui.small_button("Copiar log").clicked() {
                        let all = state::LOG.lock().unwrap().join("\n");
                        ui.output_mut(|o| o.copied_text = all);
                    }
                });
                egui::ScrollArea::vertical().stick_to_bottom(true).auto_shrink([false, false]).show(ui, |ui| {
                    for l in state::LOG.lock().unwrap().iter() {
                        ui.label(RichText::new(l).monospace().small());
                    }
                });
            });
        }

        egui::TopBottomPanel::bottom("actions")
            .frame(egui::Frame::none().fill(Color32::from_rgb(28, 29, 35)).inner_margin(egui::Margin::symmetric(16.0, 12.0)))
            .show(ctx, |ui| self.action_bar(ui));

        egui::CentralPanel::default()
            .frame(egui::Frame::none().fill(BG).inner_margin(16.0))
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| match self.tab {
                    Tab::Host => self.host_ui(ui),
                    Tab::View => self.view_ui(ui),
                });
            });

        self.invite_popup(ctx);
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
