//! Interface gráfica (egui): aba Transmitir e aba Assistir.

use std::{collections::HashMap, sync::Arc, time::Duration};

use eframe::egui::{self, Align, Color32, Layout, RichText, Stroke};
use tokio::{sync::oneshot, task::JoinHandle};

use crate::{
    audio::{self, AudioSel},
    host, net,
    sources::{self, MonitorInfo, Source, WindowInfo},
    state::{self, ViewPhase},
    viewer,
};

/// Paleta de cores e formas do tema atual.
#[derive(Clone, Copy)]
struct Palette {
    accent: Color32,
    live: Color32,
    ok: Color32,
    bg: Color32,
    card: Color32,
    border: Color32,
    muted: Color32,
    header: Color32,
    input: Color32,
    widget: Color32,
    widget_hover: Color32,
    text: Color32,
    /// Arredondamento dos cards (os demais elementos usam proporções dele).
    radius: f32,
}

fn pal() -> Palette {
    use crate::settings::Theme;
    let c = Color32::from_rgb;
    match crate::settings::theme() {
        Theme::Default => Palette {
            accent: c(124, 92, 255),
            live: c(229, 72, 77),
            ok: c(48, 164, 108),
            bg: c(22, 23, 28),
            card: c(32, 34, 41),
            border: c(50, 53, 63),
            muted: c(150, 154, 168),
            header: c(28, 29, 35),
            input: c(16, 17, 21),
            widget: c(42, 44, 53),
            widget_hover: c(52, 55, 66),
            text: c(235, 236, 240),
            radius: 10.0,
        },
        // "Agente": preto, branco e vermelho carmesim; cantos retos, visual minimalista.
        Theme::Agent => Palette {
            accent: c(178, 18, 27),
            live: c(226, 30, 40),
            ok: c(205, 205, 205),
            bg: c(9, 9, 10),
            card: c(18, 18, 20),
            border: c(46, 46, 50),
            muted: c(138, 138, 144),
            header: c(0, 0, 0),
            input: c(4, 4, 5),
            widget: c(30, 30, 33),
            widget_hover: c(48, 48, 52),
            text: c(242, 242, 242),
            radius: 1.0,
        },
        // "Bioameaça": verde-escuro de laboratório, verde tóxico e vermelho sangue.
        Theme::Biohazard => Palette {
            accent: c(98, 158, 46),
            live: c(176, 22, 30),
            ok: c(136, 204, 74),
            bg: c(9, 13, 8),
            card: c(17, 23, 15),
            border: c(46, 62, 38),
            muted: c(138, 156, 126),
            header: c(5, 8, 4),
            input: c(5, 8, 4),
            widget: c(27, 37, 24),
            widget_hover: c(40, 54, 34),
            text: c(226, 234, 214),
            radius: 4.0,
        },
    }
}

pub fn run() -> eframe::Result<()> {
    let opts = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("GameStream")
            .with_inner_size([640.0, 820.0])
            .with_min_inner_size([520.0, 560.0])
            .with_icon(eframe::icon_data::from_png_bytes(include_bytes!("../assets/icon.png")).unwrap_or_default()),
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
    let p = pal();
    let mut v = egui::Visuals::dark();
    v.panel_fill = pal().bg;
    v.window_fill = pal().card;
    v.window_stroke = Stroke::new(1.0_f32, pal().border);
    v.extreme_bg_color = p.input;
    v.selection.bg_fill = pal().accent;
    v.hyperlink_color = pal().accent;
    v.widgets.hovered.weak_bg_fill = p.widget_hover;
    v.widgets.inactive.weak_bg_fill = p.widget;
    v.override_text_color = Some(p.text);
    v.widgets.inactive.rounding = egui::Rounding::same(p.radius * 0.6);
    v.widgets.hovered.rounding = egui::Rounding::same(p.radius * 0.6);
    v.widgets.active.rounding = egui::Rounding::same(p.radius * 0.6);
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
        .fill(pal().card)
        .stroke(Stroke::new(1.0_f32, pal().border))
        .rounding(pal().radius)
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
    ui.label(RichText::new(text).small().color(pal().muted));
}

fn big_button(ui: &mut egui::Ui, text: &str, color: Color32) -> egui::Response {
    ui.add_sized(
        [ui.available_width(), 46.0],
        egui::Button::new(RichText::new(text).size(17.0).strong().color(Color32::WHITE))
            .fill(color)
            .rounding(pal().radius * 0.8),
    )
}

fn stat(ui: &mut egui::Ui, label: &str, value: String) {
    ui.vertical(|ui| {
        ui.label(RichText::new(label).small().color(pal().muted));
        ui.label(RichText::new(value).size(20.0).strong());
    });
}

fn error_box(ui: &mut egui::Ui, msg: &str) {
    egui::Frame::none()
        .fill(Color32::from_rgb(60, 24, 28))
        .stroke(Stroke::new(1.0_f32, pal().live))
        .rounding(pal().radius * 0.8)
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
        .rounding(pal().radius * 2.0)
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

/// Cor estável para o avatar padrão (a partir do nome).
fn name_color(name: &str) -> Color32 {
    let h = name.bytes().fold(5381u32, |h, b| h.wrapping_mul(33) ^ b as u32);
    let hue = (h % 360) as f32 / 360.0;
    egui::ecolor::Hsva::new(hue, 0.55, 0.75, 1.0).into()
}

/// Avatar redondo: a foto, ou a inicial do nome sobre uma cor.
fn avatar_circle(ui: &mut egui::Ui, tex: Option<&egui::TextureHandle>, name: &str, size: f32) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::hover());
    match tex {
        Some(t) => {
            egui::Image::new(t).rounding(size / 2.0).paint_at(ui, rect);
        }
        None => {
            ui.painter().circle_filled(rect.center(), size / 2.0, name_color(name));
            let initial: String = name.chars().next().map(|c| c.to_uppercase().collect()).unwrap_or_else(|| "?".into());
            ui.painter().text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                initial,
                egui::FontId::proportional(size * 0.5),
                Color32::WHITE,
            );
        }
    }
    resp
}

/// Texturas dos avatares (recriadas só quando a imagem muda).
#[derive(Default)]
struct AvatarCache {
    tex: HashMap<String, (usize, egui::TextureHandle)>,
}

impl AvatarCache {
    fn get(&mut self, ctx: &egui::Context, key: &str, png: Option<&Arc<Vec<u8>>>) -> Option<egui::TextureHandle> {
        let png = png?;
        let stamp = Arc::as_ptr(png) as usize ^ png.len();
        if let Some((s, t)) = self.tex.get(key) {
            if *s == stamp {
                return Some(t.clone());
            }
        }
        let (size, rgba) = crate::avatar::decode_rgba(png)?;
        let img = egui::ColorImage::from_rgba_unmultiplied(size, &rgba);
        let t = ctx.load_texture(format!("avatar-{key}"), img, egui::TextureOptions::LINEAR);
        self.tex.insert(key.to_string(), (stamp, t.clone()));
        Some(t)
    }
}

/// Lista de mensagens do chat com avatar, mais o campo de texto e as reações.
/// Devolve Some(texto) para enviar ou a reação clicada (como "react:🔥").
fn chat_box(
    ui: &mut egui::Ui,
    cache: &mut AvatarCache,
    ns: &str,
    chat: &[crate::control::ChatMsg],
    avatars: &HashMap<String, Arc<Vec<u8>>>,
    input: &mut String,
) -> Option<String> {
    let mut out = None;
    egui::Frame::none().fill(pal().input).rounding(pal().radius * 0.8).inner_margin(8.0).show(ui, |ui| {
        ui.set_width(ui.available_width());
        egui::ScrollArea::vertical().max_height(220.0).auto_shrink([false, true]).stick_to_bottom(true).show(ui, |ui| {
            if chat.is_empty() {
                hint(ui, crate::i18n::t("Nenhuma mensagem ainda. As mensagens e reações aparecem também em cima do vídeo, para todos."));
            }
            for m in chat.iter().rev().take(80).rev() {
                ui.horizontal_wrapped(|ui| {
                    let tex = cache.get(ui.ctx(), &format!("{ns}:{}", m.id), avatars.get(&m.id));
                    avatar_circle(ui, tex.as_ref(), &m.from, 20.0);
                    ui.label(RichText::new(&m.from).strong().color(pal().accent));
                    if m.react {
                        ui.label(RichText::new(&m.text).size(18.0));
                    } else {
                        ui.label(&m.text);
                    }
                });
            }
        });
    });
    ui.horizontal(|ui| {
        let edit = ui.add(
            egui::TextEdit::singleline(input)
                .char_limit(200)
                .hint_text(crate::i18n::t("Escreva uma mensagem e tecle Enter"))
                .desired_width(ui.available_width() - 80.0),
        );
        let enter = edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        if (ui.button(crate::i18n::t("Enviar")).clicked() || enter) && !input.trim().is_empty() {
            out = Some(std::mem::take(input));
            edit.request_focus();
        }
    });
    ui.horizontal(|ui| {
        for r in crate::control::REACTIONS {
            if ui.add(egui::Button::new(RichText::new(r).size(18.0)).min_size(egui::vec2(40.0, 30.0))).clicked() {
                out = Some(format!("react:{r}"));
            }
        }
    });
    out
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
    last_src_kind: SrcKind,
    monitors: Vec<MonitorInfo>,
    windows: Vec<WindowInfo>,
    sel_monitor: usize,
    sel_window: Option<(isize, String)>,
    out_devices: Vec<String>,
    in_devices: Vec<String>,
    audio: bool,
    audio_sel: AudioSel,
    audio_apps: Vec<(u32, String)>,
    mic: Option<String>,
    auto_bitrate: bool,
    bitrate: u32,
    fps: u32,
    upnp: bool,
    encoder: String,
    password: String,
    friends: u32,
    approval: bool,
    audio_pcm: bool,
    speed: Option<std::sync::Arc<std::sync::Mutex<Option<Result<f64, String>>>>>,
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
    reply_input: String,
    reply_msg: Option<(bool, String)>,
    // chat, perfil e controle remoto
    chat_input: String,
    avatars: AvatarCache,
    profile_open: bool,
    nick_edit: String,
    profile_msg: Option<(bool, String)>,
    my_avatar: Option<Arc<Vec<u8>>>,
    want_pad: bool,
    want_kbm: bool,
    /// Fotos das salas recentes (código -> PNG), recarregadas de tempos em tempos.
    recent_avatars: (std::time::Instant, HashMap<String, Arc<Vec<u8>>>),
    /// Tela inicial de atualização (como o Discord): verifica e instala antes de abrir o app.
    startup: Option<std::time::Instant>,
    /// Já mandou instalar a atualização (não repete).
    update_started: bool,
    /// Atualização automática suspensa nesta execução (proteção contra loop).
    update_blocked: bool,
    /// Última interação do usuário (atualização com o app aberto só depois de um tempo parado).
    last_input: std::time::Instant,
}

impl App {
    fn new() -> Self {
        let mut a = App {
            rt: tokio::runtime::Builder::new_multi_thread().enable_all().build().expect("tokio"),
            tab: Tab::Host,
            show_log: false,
            src_kind: SrcKind::Monitor,
            last_src_kind: SrcKind::Monitor,
            monitors: vec![],
            windows: vec![],
            sel_monitor: 0,
            sel_window: None,
            out_devices: vec![],
            in_devices: vec![],
            audio: true,
            audio_sel: AudioSel::AllExceptDiscord,
            audio_apps: vec![],
            mic: None,
            auto_bitrate: true,
            bitrate: 20,
            fps: 60,
            upnp: true,
            encoder: "auto".into(),
            password: String::new(),
            friends: 4,
            approval: true,
            audio_pcm: false,
            speed: None,
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
            reply_input: String::new(),
            reply_msg: None,
            chat_input: String::new(),
            avatars: AvatarCache::default(),
            profile_open: false,
            nick_edit: crate::settings::nick_raw(),
            profile_msg: None,
            my_avatar: crate::avatar::load(),
            want_pad: true,
            want_kbm: true,
            recent_avatars: (std::time::Instant::now() - Duration::from_secs(60), HashMap::new()),
            startup: (!crate::updater::auto_update_disabled()).then(std::time::Instant::now),
            update_started: false,
            update_blocked: false,
            last_input: std::time::Instant::now(),
        };
        a.refresh();
        crate::updater::check_in_background();
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
        self.audio_apps = crate::procaudio::audio_apps();
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
                self.live_desc = m.map(|m| m.label()).unwrap_or(crate::i18n::t("Monitor 1").into());
                Source::Monitor(m.map(|m| m.idx).unwrap_or(0))
            }
            SrcKind::Window => match &self.sel_window {
                Some((hwnd, title)) => {
                    self.live_desc = crate::tr!("Janela: {title}", "Window: {title}");
                    Source::Window { hwnd: *hwnd, title: title.clone() }
                }
                None => {
                    state::host(|h| h.error = Some(crate::i18n::t("Selecione uma janela antes de iniciar a transmissão.").into()));
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
            mic: self.mic.clone(),
            password: (!self.password.is_empty()).then(|| self.password.clone()),
            upnp: self.upnp,
            room: true,
            approval: self.approval,
            audio_pcm: self.audio_pcm,
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

        card(ui, crate::i18n::t("1. Fonte de vídeo"), |ui| {
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.src_kind, SrcKind::Monitor, crate::i18n::t("🖥  Monitor"));
                ui.selectable_value(&mut self.src_kind, SrcKind::Window, crate::i18n::t("🎮  Janela específica"));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.button("⟳").on_hover_text(crate::i18n::t("Atualizar listas de monitores, janelas e áudio")).clicked() {
                        self.refresh();
                    }
                });
            });
            match self.src_kind {
                SrcKind::Monitor => {
                    let sel = self.monitors.get(self.sel_monitor).map(|m| m.label()).unwrap_or(crate::i18n::t("Monitor 1").into());
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
                    let sel = self.sel_window.as_ref().map(|(_, t)| t.clone()).unwrap_or(crate::i18n::t("Selecione a janela...").into());
                    egui::ComboBox::from_id_salt("window")
                        .width(ui.available_width())
                        .selected_text(sel)
                        .show_ui(ui, |ui| {
                            for w in &self.windows {
                                ui.selectable_value(&mut self.sel_window, Some((w.hwnd, w.title.clone())), &w.title);
                            }
                        });
                    hint(ui, crate::i18n::t("Captura nativa do Windows: a janela pode ser movida, redimensionada ou sobreposta sem interromper a transmissão. Evite minimizá-la."));
                }
            }
            ui.add_space(4.0);
            ui.checkbox(&mut self.preview, crate::i18n::t("👁  Exibir pré-visualização da transmissão (sem áudio)"));
            hint(
                ui,
                crate::i18n::t("Abre uma janela com a imagem exata recebida pelos espectadores. Se ela estiver no monitor transmitido, \
                 ocorre o efeito espelho; posicione a pré-visualização em outro monitor."),
            );
        });

        // Ajusta o padrão de áudio conforme a fonte: janela -> áudio do aplicativo; monitor -> sistema exceto Discord.
        if self.src_kind != self.last_src_kind {
            match (self.src_kind, &self.audio_sel) {
                (SrcKind::Window, AudioSel::AllExceptDiscord) => self.audio_sel = AudioSel::GameWindow,
                (SrcKind::Monitor, AudioSel::GameWindow) => self.audio_sel = AudioSel::AllExceptDiscord,
                _ => {}
            }
            self.last_src_kind = self.src_kind;
        }

        card(ui, crate::i18n::t("2. Áudio"), |ui| {
            ui.checkbox(&mut self.audio, crate::i18n::t("Transmitir áudio"));
            ui.add_enabled_ui(self.audio, |ui| {
                ui.checkbox(&mut self.audio_pcm, crate::i18n::t("Áudio sem compressão (PCM)"))
                    .on_hover_text(crate::i18n::t("Por padrão o áudio usa Opus a 160 kbps, qualidade considerada transparente, com ~0,16 Mbps por espectador. Sem compressão usa ~1,5 Mbps por espectador."));
            });
            ui.add_enabled_ui(self.audio, |ui| {
                ui.label(crate::i18n::t("Fonte de áudio:"));
                let window_mode = self.src_kind == SrcKind::Window;
                egui::ComboBox::from_id_salt("audio_src")
                    .width(ui.available_width())
                    .selected_text(self.audio_sel.label())
                    .show_ui(ui, |ui| {
                        ui.add_enabled_ui(window_mode, |ui| {
                            ui.selectable_value(&mut self.audio_sel, AudioSel::GameWindow, AudioSel::GameWindow.label());
                        });
                        ui.selectable_value(
                            &mut self.audio_sel,
                            AudioSel::AllExceptDiscord,
                            AudioSel::AllExceptDiscord.label(),
                        );
                        if !self.audio_apps.is_empty() {
                            ui.separator();
                            ui.label(RichText::new(crate::i18n::t("Aplicativo específico (com áudio ativo)")).small().color(pal().muted));
                            for (pid, name) in &self.audio_apps {
                                let v = AudioSel::App { pid: *pid, name: name.clone() };
                                let l = v.label();
                                ui.selectable_value(&mut self.audio_sel, v, l);
                            }
                        }
                        ui.separator();
                        ui.label(RichText::new(crate::i18n::t("Dispositivo de saída (inclui o Discord)")).small().color(pal().muted));
                        ui.selectable_value(&mut self.audio_sel, AudioSel::DefaultOutput, AudioSel::DefaultOutput.label());
                        for d in &self.out_devices {
                            let v = AudioSel::Output(d.clone());
                            let l = v.label();
                            ui.selectable_value(&mut self.audio_sel, v, l);
                        }
                        ui.separator();
                        ui.label(RichText::new(crate::i18n::t("Dispositivos de entrada (mixagem estéreo, placa de captura)")).small().color(pal().muted));
                        for d in &self.in_devices {
                            let v = AudioSel::Input(d.clone());
                            let l = v.label();
                            ui.selectable_value(&mut self.audio_sel, v, l);
                        }
                    });
                match &self.audio_sel {
                    AudioSel::GameWindow => hint(ui, crate::i18n::t("Somente o áudio do aplicativo transmitido. Discord, navegador e notificações não são incluídos.")),
                    AudioSel::AllExceptDiscord => hint(
                        ui,
                        crate::i18n::t("Todo o áudio do sistema, exceto o Discord. Evita que participantes de uma chamada de voz ouçam o próprio retorno."),
                    ),
                    AudioSel::App { .. } => hint(ui, crate::i18n::t("Somente o áudio deste aplicativo. Caso ele não esteja na lista, inicie-o e clique em ⟳.")),
                    AudioSel::DefaultOutput | AudioSel::Output(_) => {
                        ui.label(
                            RichText::new(crate::i18n::t("⚠ Inclui o Discord: durante uma chamada de voz, os participantes ouvirão o retorno das próprias vozes."))
                                .small()
                                .color(Color32::from_rgb(255, 190, 90)),
                        );
                    }
                    AudioSel::Input(_) => hint(ui, crate::i18n::t("Captura um dispositivo de entrada de áudio.")),
                }

                ui.add_space(6.0);
                let mut mic_on = self.mic.is_some();
                if ui.checkbox(&mut mic_on, crate::i18n::t("🎤 Incluir microfone")).changed() {
                    self.mic = if mic_on { audio::default_input().or_else(|| self.in_devices.first().cloned()) } else { None };
                }
                if let Some(cur) = self.mic.clone() {
                    egui::ComboBox::from_id_salt("mic_dev").width(ui.available_width()).selected_text(&cur).show_ui(ui, |ui| {
                        for d in &self.in_devices {
                            ui.selectable_value(&mut self.mic, Some(d.clone()), d);
                        }
                    });
                    ui.label(
                        RichText::new(crate::i18n::t("⚠ Mantenha desativado durante chamadas no Discord: sua voz seria reproduzida em duplicidade (pela chamada e pela transmissão)."))
                            .small()
                            .color(Color32::from_rgb(255, 190, 90)),
                    );
                } else {
                    hint(ui, crate::i18n::t("Desativado (recomendado durante chamadas de voz no Discord)."));
                }
            });
        });

        card(ui, crate::i18n::t("3. Qualidade"), |ui| {
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.auto_bitrate, true, crate::i18n::t("⚡ Automático"));
                ui.selectable_value(&mut self.auto_bitrate, false, crate::i18n::t("📌 Fixo"));
            });
            let label = if self.auto_bitrate { crate::i18n::t("Mbps (máximo)") } else { crate::i18n::t("Mbps") };
            ui.add(egui::Slider::new(&mut self.bitrate, 3..=60).text(label));
            hint(
                ui,
                if self.auto_bitrate {
                    crate::i18n::t("Inicia em 70% do máximo, reduz automaticamente quando há perda de pacotes e volta a aumentar quando a conexão se estabiliza. O ajuste acompanha o espectador com a conexão mais lenta.")
                } else {
                    crate::i18n::t("Bitrate constante. Espectadores com conexão insuficiente podem ter travamentos.")
                },
            );
            ui.horizontal(|ui| {
                ui.label("FPS:");
                for f in [30, 60, 120] {
                    ui.selectable_value(&mut self.fps, f, f.to_string());
                }
            });
            self.speed_test_ui(ui);
            egui::CollapsingHeader::new(crate::i18n::t("Guia de bitrate")).show(ui, |ui| bitrate_guide(ui, self.bitrate, &mut self.friends));
        });

        card(ui, crate::i18n::t("4. Acesso e segurança"), |ui| {
            ui.horizontal(|ui| {
                ui.label(crate::i18n::t("Senha:"));
                ui.add(
                    egui::TextEdit::singleline(&mut self.password)
                        .password(true)
                        .desired_width(ui.available_width())
                        .hint_text(crate::i18n::t("Recomendado")),
                );
            });
            ui.checkbox(&mut self.approval, crate::i18n::t("Sala de espera: aprovar cada espectador antes de exibir a transmissão"));
            ui.checkbox(&mut self.upnp, crate::i18n::t("Abrir porta automaticamente no roteador (UPnP)"));
            egui::CollapsingHeader::new(crate::i18n::t("Avançado")).show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(crate::i18n::t("Encoder:"));
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
                    ui.label(RichText::new(crate::i18n::t("Preparando a transmissão...")).size(17.0).strong());
                });
                if let Some(p) = *crate::updater::FFMPEG_PROGRESS.lock().unwrap() {
                    ui.label(crate::i18n::t("Baixando o encoder de vídeo (FFmpeg, ~90 MB). Necessário apenas na primeira transmissão."));
                    ui.add(egui::ProgressBar::new(p).show_percentage());
                } else {
                    hint(ui, crate::i18n::t("Detectando o encoder da placa de vídeo e inicializando a conexão."));
                }
            });
        } else {
            card(ui, "", |ui| {
                ui.horizontal(|ui| {
                    pill(ui, crate::i18n::t("AO VIVO"), pal().live);
                    ui.label(RichText::new(&self.live_desc).strong());
                });
                // Trocar monitor/janela sem encerrar a transmissão.
                ui.horizontal(|ui| {
                    ui.label(RichText::new(crate::i18n::t("Trocar fonte:")).small().color(pal().muted));
                    let mut chosen: Option<(Source, String)> = None;
                    egui::ComboBox::from_id_salt("live_source")
                        .width(ui.available_width() - 40.0)
                        .selected_text(&self.live_desc)
                        .show_ui(ui, |ui| {
                            for m in &self.monitors {
                                if ui.selectable_label(false, format!("🖥  {}", m.label())).clicked() {
                                    chosen = Some((Source::Monitor(m.idx), m.label()));
                                }
                            }
                            ui.separator();
                            for w in &self.windows {
                                if ui.selectable_label(false, format!("🎮  {}", w.title)).clicked() {
                                    chosen = Some((
                                        Source::Window { hwnd: w.hwnd, title: w.title.clone() },
                                        crate::tr!("Janela: {}", "Window: {}", w.title),
                                    ));
                                }
                            }
                        });
                    if ui.button("⟳").on_hover_text(crate::i18n::t("Atualizar listas de monitores, janelas e áudio")).clicked() {
                        self.refresh();
                    }
                    if let Some((src, desc)) = chosen {
                        match host::switch_source(src) {
                            Ok(()) => self.live_desc = desc,
                            Err(e) => state::log(format!("{e:#}")),
                        }
                    }
                });
                ui.add_space(6.0);
                ui.columns(4, |c| {
                    stat(&mut c[0], crate::i18n::t("Espectadores"), st.viewers.len().to_string());
                    stat(&mut c[1], crate::i18n::t("Bitrate alvo"), format!("{} Mbps", st.bitrate));
                    stat(&mut c[2], crate::i18n::t("Taxa de saída"), format!("{:.1} Mbps", st.mbps));
                    stat(&mut c[3], crate::i18n::t("Upload total"), format!("{:.0} Mbps", st.mbps * st.viewers.len() as f64));
                });
                ui.horizontal(|ui| {
                    hint(ui, &format!("Encoder: {}", st.encoder));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let label = if st.preview_open { crate::i18n::t("👁  Ocultar pré-visualização") } else { crate::i18n::t("👁  Exibir pré-visualização") };
                        if ui.button(label).clicked() {
                            self.preview = !st.preview_open;
                            host::set_preview(self.preview);
                        }
                    });
                });
            });

            card(ui, crate::i18n::t("Convite"), |ui| {
                if let Some(room) = &st.room {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(crate::i18n::t("Código da sala")).color(pal().muted));
                        ui.label(RichText::new(room).size(26.0).strong().monospace().color(pal().accent));
                        if ui.small_button("📋").on_hover_text(crate::i18n::t("Copiar código da sala")).clicked() {
                            ui.output_mut(|o| o.copied_text = room.clone());
                        }
                        let (txt, col) = if st.room_online {
                            (crate::i18n::t("online"), pal().ok)
                        } else {
                            (crate::i18n::t("conectando ao servidor..."), Color32::from_rgb(255, 190, 90))
                        };
                        ui.label(RichText::new(txt).small().color(col));
                    });
                    hint(ui, crate::i18n::t("Espectadores podem digitar o código da sala ou colar o convite completo; a conexão é liberada automaticamente."));
                    ui.add_space(4.0);
                }
                ui.horizontal(|ui| {
                    if ui
                        .add(egui::Button::new(RichText::new(crate::i18n::t("📋  Copiar convite")).strong().color(Color32::WHITE)).fill(pal().accent))
                        .clicked()
                    {
                        if let Some(code) = &st.invite {
                            ui.output_mut(|o| o.copied_text = code.clone());
                            state::log(crate::i18n::t("Convite copiado."));
                        }
                    }
                    if ui.button(crate::i18n::t("Exibir convite")).clicked() {
                        self.popup = st.invite.clone();
                    }
                });
                ui.label(crate::tr!("🔒 Código de segurança: {}", "🔒 Security code: {}", st.security_code));
                egui::CollapsingHeader::new(crate::i18n::t("Endereços de conexão")).show(ui, |ui| {
                    for r in &st.reach {
                        hint(ui, r);
                    }
                });
            });

            card(ui, crate::i18n::t("Liberar espectador"), |ui| {
                hint(
                    ui,
                    crate::i18n::t("Se um espectador não conseguir conectar, ele receberá um código de resposta (GS-R-...). Cole o código aqui para liberar a conexão através do firewall do seu roteador."),
                );
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.reply_input)
                            .hint_text("GS-R-...")
                            .desired_width(ui.available_width() - 90.0),
                    );
                    if ui.add_enabled(!self.reply_input.trim().is_empty(), egui::Button::new(crate::i18n::t("Liberar"))).clicked() {
                        self.reply_msg = Some(match host::release_viewer(&self.reply_input) {
                            Ok(_) => {
                                self.reply_input.clear();
                                (true, crate::i18n::t("Liberação enviada. O espectador deve conectar em alguns segundos.").into())
                            }
                            Err(e) => (false, format!("{e:#}")),
                        });
                    }
                });
                if let Some((ok, msg)) = &self.reply_msg {
                    let col = if *ok { pal().ok } else { Color32::from_rgb(255, 190, 90) };
                    ui.label(RichText::new(msg).small().color(col));
                }
            });

            if !st.pending.is_empty() {
                card(ui, crate::i18n::t("Sala de espera"), |ui| {
                    for (id, who) in &st.pending {
                        ui.horizontal(|ui| {
                            ui.label(format!("⏳  {who}"));
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if ui.button(crate::i18n::t("Recusar")).clicked() {
                                    host::decide(*id, false);
                                }
                                if ui
                                    .add(egui::Button::new(RichText::new(crate::i18n::t("Aprovar")).strong().color(Color32::WHITE)).fill(pal().ok))
                                    .clicked()
                                {
                                    host::decide(*id, true);
                                }
                            });
                        });
                    }
                });
            }

            let avatars = &mut self.avatars;
            card(ui, crate::i18n::t("Espectadores conectados"), |ui| {
                if st.preview_open {
                    ui.label(RichText::new(crate::i18n::t("🖥  Pré-visualização local (sem áudio)")).color(pal().muted));
                }
                if st.viewers.is_empty() {
                    hint(ui, crate::i18n::t("Nenhum espectador conectado. Compartilhe o convite e a senha."));
                }
                let warn = Color32::from_rgb(255, 190, 90);
                for v in &st.viewers {
                    egui::Frame::none().fill(pal().input).rounding(pal().radius * 0.8).inner_margin(8.0).show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal(|ui| {
                            let tex = avatars.get(ui.ctx(), &format!("h:{}", v.id), st.avatars.get(&v.id));
                            avatar_circle(ui, tex.as_ref(), &v.name, 34.0);
                            ui.vertical(|ui| {
                                ui.horizontal(|ui| {
                                    ui.label(RichText::new(&v.name).strong());
                                    if v.name != v.ip {
                                        ui.label(RichText::new(&v.ip).small().color(pal().muted));
                                    }
                                    if v.version.is_empty() {
                                        ui.label(RichText::new(crate::i18n::t("versão antiga")).small().color(warn))
                                            .on_hover_text(crate::i18n::t("Sem chat, lista da sala e jogar junto. Peça para ele abrir o GameStream de novo para atualizar."));
                                    } else if v.version != crate::updater::VERSION {
                                        ui.label(RichText::new(format!("v{}", v.version)).small().color(pal().muted));
                                    }
                                });
                                let loss_col = if v.loss >= 2.0 { warn } else { pal().muted };
                                ui.horizontal(|ui| {
                                    ui.label(RichText::new(format!("ping {} ms", v.ping_ms)).small().color(pal().muted));
                                    ui.label(RichText::new(crate::tr!("perda {:.1}%", "loss {:.1}%", v.loss)).small().color(loss_col));
                                    ui.label(RichText::new(format!("{:.1} Mbps", v.mbps)).small().color(pal().muted));
                                });
                            });
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if ui.small_button(crate::i18n::t("Remover")).on_hover_text(crate::i18n::t("Desconecta e bloqueia até o fim da transmissão")).clicked() {
                                    host::kick(&v.key);
                                }
                                if v.granted != 0 {
                                    if ui.small_button(crate::i18n::t("Revogar controle")).clicked() {
                                        host::set_control(&v.key, 0);
                                    }
                                } else if v.wants != 0 {
                                    if ui
                                        .add(egui::Button::new(RichText::new(crate::i18n::t("Permitir")).strong().color(Color32::WHITE)).fill(pal().ok))
                                        .on_hover_text(crate::i18n::t("Deixa este espectador jogar no seu PC com o que ele pediu"))
                                        .clicked()
                                    {
                                        host::set_control(&v.key, v.wants);
                                    }
                                }
                            });
                        });
                        if v.granted != 0 {
                            ui.label(RichText::new(crate::tr!("🎮 Jogando com: {}", "🎮 Playing with: {}", crate::control::ctl_desc(v.granted))).small().color(pal().ok));
                        } else if v.wants != 0 {
                            ui.label(RichText::new(crate::tr!("🎮 Pediu para jogar ({})", "🎮 Asked to play ({})", crate::control::ctl_desc(v.wants))).small().color(warn));
                        }
                    });
                    ui.add_space(4.0);
                }
                if st.viewers.iter().any(|v| v.granted != 0) {
                    if ui.button(crate::i18n::t("⛔  Revogar o controle de todos")).clicked() {
                        host::revoke_all_control();
                    }
                    hint(ui, crate::i18n::t("Quem joga usa o seu PC: teclado e mouse dele são enviados ao jogo em primeiro plano. Revogue a qualquer momento."));
                }
            });

            card(ui, crate::i18n::t("Chat"), |ui| {
                if let Some(msg) = chat_box(ui, &mut self.avatars, "h", &st.chat, &st.avatars, &mut self.chat_input) {
                    match msg.strip_prefix("react:") {
                        Some(r) => host::send_react(r),
                        None => host::send_chat(&msg),
                    }
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

        card(ui, crate::i18n::t("Conectar a uma transmissão"), |ui| {
            ui.add_enabled_ui(!running, |ui| {
                ui.label(crate::i18n::t("Convite:"));
                ui.add(
                    egui::TextEdit::multiline(&mut self.invite)
                        .desired_rows(3)
                        .desired_width(f32::INFINITY)
                        .font(egui::TextStyle::Monospace)
                        .hint_text(crate::i18n::t("Cole o convite (GS-...) ou digite o código da sala (ex.: KX7-92B)")),
                );
                ui.horizontal(|ui| {
                    ui.label(crate::i18n::t("Senha:"));
                    ui.add(egui::TextEdit::singleline(&mut self.view_password).password(true).desired_width(ui.available_width()));
                });
                ui.label(crate::i18n::t("Saída de áudio:"));
                let sel = self.view_out.clone().unwrap_or(crate::i18n::t("Padrão do Windows").into());
                egui::ComboBox::from_id_salt("view_out").width(ui.available_width()).selected_text(sel).show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.view_out, None, crate::i18n::t("Padrão do Windows"));
                    for d in &self.out_devices {
                        ui.selectable_value(&mut self.view_out, Some(d.clone()), format!("🔊 {d}"));
                    }
                });
            });
        });

        if !running {
            let recent = crate::settings::recent();
            if !recent.is_empty() {
                if self.recent_avatars.0.elapsed() > Duration::from_secs(5) {
                    self.recent_avatars =
                        (std::time::Instant::now(), recent.iter().filter_map(|r| Some((r.code.clone(), crate::settings::recent_avatar(&r.code)?))).collect());
                }
                let mut connect_to: Option<String> = None;
                let (avatars, recent_av) = (&mut self.avatars, &self.recent_avatars.1);
                card(ui, crate::i18n::t("Salas recentes"), |ui| {
                    for r in &recent {
                        ui.horizontal(|ui| {
                            let tex = avatars.get(ui.ctx(), &format!("r:{}", r.code), recent_av.get(&r.code));
                            avatar_circle(ui, tex.as_ref(), &r.name, 32.0);
                            ui.vertical(|ui| {
                                ui.label(RichText::new(if r.name.is_empty() { crate::room::display(&r.code) } else { r.name.clone() }).strong());
                                ui.label(RichText::new(format!("{}  ·  {}", crate::room::display(&r.code), ago(r.last))).small().color(pal().muted));
                            });
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if ui.small_button("🗑").on_hover_text(crate::i18n::t("Remover da lista")).clicked() {
                                    crate::settings::remove_recent(&r.code);
                                }
                                if ui
                                    .add(egui::Button::new(RichText::new(crate::i18n::t("Entrar")).strong().color(Color32::WHITE)).fill(pal().accent))
                                    .clicked()
                                {
                                    connect_to = Some(crate::room::display(&r.code));
                                }
                            });
                        });
                    }
                    hint(ui, crate::i18n::t("Funciona enquanto a pessoa estiver transmitindo (o código da sala não muda)."));
                });
                if let Some(code) = connect_to {
                    self.invite = code;
                    self.start_view();
                }
            }
        }

        if vs.phase == ViewPhase::Error && !running {
            error_box(ui, &vs.text);
        }
        if running {
            card(ui, "", |ui| {
                ui.horizontal(|ui| {
                    ui.spinner();
                    let title = if vs.reconnecting.is_some() { crate::i18n::t("Reconectando...") } else { crate::i18n::t("Conectando...") };
                    ui.label(RichText::new(title).size(17.0).strong());
                });
                if let Some(r) = &vs.reconnecting {
                    ui.label(RichText::new(r).color(pal().muted));
                    hint(ui, crate::i18n::t("A janela do vídeo continua aberta e volta sozinha quando a conexão voltar (até 3 minutos)."));
                }
                if !vs.text.is_empty() && vs.text != crate::i18n::t("Conectando...") && vs.reply_code.is_none() {
                    ui.label(RichText::new(&vs.text).color(pal().muted));
                }
                if let Some(code) = &vs.reply_code {
                    ui.add_space(6.0);
                    ui.label(RichText::new(crate::i18n::t("A conexão direta não respondeu.")).strong());
                    ui.label(
                        crate::i18n::t("Envie o código de resposta abaixo ao transmissor. Ele deve colá-lo em \"Liberar espectador\"; a conexão será concluída automaticamente."),
                    );
                    egui::Frame::none()
                        .fill(pal().input)
                        .rounding(pal().radius * 0.8)
                        .inner_margin(10.0)
                        .show(ui, |ui| {
                            ui.add(egui::Label::new(RichText::new(code).monospace().small().color(pal().muted)).wrap());
                        });
                    if ui
                        .add(egui::Button::new(RichText::new(crate::i18n::t("📋  Copiar código de resposta")).strong().color(Color32::WHITE)).fill(pal().accent))
                        .clicked()
                    {
                        ui.output_mut(|o| o.copied_text = code.clone());
                    }
                    hint(ui, crate::i18n::t("Aguardando por até 3 minutos."));
                }
            });
        }
    }

    fn view_live_ui(&mut self, ui: &mut egui::Ui, vs: &state::ViewStatus) {
        card(ui, "", |ui| {
            ui.horizontal(|ui| {
                pill(ui, crate::i18n::t("CONECTADO"), pal().ok);
                if vs.host_name.is_empty() {
                    ui.label(RichText::new(&vs.peer).strong());
                } else {
                    ui.label(RichText::new(&vs.host_name).strong());
                    ui.label(RichText::new(&vs.peer).small().color(pal().muted));
                }
            });
            if vs.verified {
                ui.label(crate::tr!("🔒 Conexão criptografada · identidade do transmissor verificada · código {}", "🔒 Encrypted connection · streamer identity verified · code {}", vs.code));
            } else {
                ui.label(
                    RichText::new(crate::tr!("⚠ Conexão criptografada, porém a identidade do transmissor não foi verificada (conexão por IP) · código {}", "⚠ Encrypted connection, but the streamer's identity was not verified (IP connection) · code {}", vs.code))
                        .color(Color32::from_rgb(255, 190, 90)),
                );
            }
            ui.add_space(6.0);
            let s = &vs.stats;
            let warn = Color32::from_rgb(255, 190, 90);
            ui.columns(4, |c| {
                stat(&mut c[0], crate::i18n::t("Ping"), format!("{} ms", s.ping_ms));
                stat(&mut c[1], crate::i18n::t("FPS"), s.fps.to_string());
                c[2].vertical(|ui| {
                    ui.label(RichText::new(crate::i18n::t("Perda de pacotes")).small().color(pal().muted));
                    let col = if s.loss >= 2.0 { warn } else { Color32::WHITE };
                    ui.label(RichText::new(format!("{:.1}%", s.loss)).size(20.0).strong().color(col));
                });
                stat(&mut c[3], crate::i18n::t("Taxa recebida"), format!("{:.1} Mbps", s.mbps));
            });
            // Indicador de áudio recebido: mostra se o som está chegando (e se está em silêncio).
            ui.horizontal(|ui| {
                ui.label(RichText::new(crate::i18n::t("Áudio recebido:")).small().color(pal().muted));
                match s.audio_db {
                    None => {
                        ui.label(RichText::new(crate::i18n::t("nenhum (o transmissor não está enviando áudio)")).small().color(Color32::from_rgb(255, 190, 90)));
                    }
                    Some(db) => {
                        let level = ((db + 60.0) / 60.0).clamp(0.0, 1.0);
                        ui.add(egui::ProgressBar::new(level).desired_width(140.0).desired_height(8.0).fill(pal().ok));
                        let txt = if db <= -60.0 {
                            crate::i18n::t("silêncio").to_string()
                        } else {
                            format!("{db:.0} dB")
                        };
                        ui.label(RichText::new(txt).small().color(pal().muted));
                    }
                }
            });
            if s.audio_db.is_some_and(|db| db > -60.0) {
                hint(ui, crate::i18n::t("Se não estiver ouvindo, confira a saída de áudio (padrão do Windows) e o volume no painel do vídeo."));
            }
            if s.loss >= 2.0 {
                hint(ui, crate::i18n::t("Perda de pacotes elevada: a conexão (ou o Wi-Fi) está instável. Com bitrate automático, o transmissor reduzirá a taxa."));
            }
        });

        if !vs.roster.is_empty() {
            let avatars = &mut self.avatars;
            card(ui, &crate::tr!("Na sala ({})", "In the room ({})", vs.roster.len()), |ui| {
                ui.horizontal_wrapped(|ui| {
                    for m in &vs.roster {
                        egui::Frame::none().fill(pal().input).rounding(pal().radius * 2.0).inner_margin(egui::Margin::symmetric(8.0, 4.0)).show(ui, |ui| {
                            ui.horizontal(|ui| {
                                let tex = avatars.get(ui.ctx(), &format!("v:{}", m.id), vs.avatars.get(&m.id));
                                avatar_circle(ui, tex.as_ref(), &m.name, 24.0);
                                let mut t = RichText::new(&m.name);
                                if m.id == vs.my_id {
                                    t = t.strong();
                                }
                                ui.label(t);
                                if m.host {
                                    ui.label(RichText::new(crate::i18n::t("📡 transmitindo")).small().color(pal().live));
                                }
                                if m.ctl != 0 {
                                    ui.label(RichText::new("🎮").small()).on_hover_text(crate::tr!("Jogando com: {}", "Playing with: {}", crate::control::ctl_desc(m.ctl)));
                                }
                                if m.id == vs.my_id {
                                    ui.label(RichText::new(crate::i18n::t("(você)")).small().color(pal().muted));
                                }
                            });
                        });
                    }
                });
            });
        }

        if vs.host_version.as_deref() == Some("") {
            card(ui, "", |ui| {
                ui.label(RichText::new(crate::i18n::t("⚠ O transmissor está numa versão antiga do GameStream")).strong().color(Color32::from_rgb(255, 190, 90)));
                hint(ui, crate::i18n::t("O vídeo e o som funcionam, mas o chat, a lista da sala e o jogar junto só aparecem quando os dois estiverem na versão nova. Peça para ele fechar e abrir o GameStream: a atualização é automática."));
            });
        } else if vs.host_version.as_deref().is_some_and(|v| v != crate::updater::VERSION) {
            hint(ui, &crate::tr!("O transmissor está na versão v{}; você, na v{}.", "The streamer is on v{}; you are on v{}.", vs.host_version.clone().unwrap_or_default(), crate::updater::VERSION));
        }

        if vs.has_control {
            card(ui, crate::i18n::t("Chat"), |ui| {
                if let Some(msg) = chat_box(ui, &mut self.avatars, "v", &vs.chat, &vs.avatars, &mut self.chat_input) {
                    match msg.strip_prefix("react:") {
                        Some(r) => viewer::send_react(r),
                        None => viewer::send_chat(&msg),
                    }
                }
            });

            card(ui, crate::i18n::t("Jogar junto (controle remoto)"), |ui| {
                hint(ui, crate::i18n::t("Opcional: peça para jogar no PC do transmissor com o seu controle ou com teclado e mouse. Ele precisa permitir."));
                let asked = vs.ctl_asked != 0;
                ui.add_enabled_ui(!asked, |ui| {
                    ui.horizontal(|ui| {
                        ui.checkbox(&mut self.want_pad, crate::i18n::t("Controle"));
                        let (txt, col) = if vs.pad_found {
                            (crate::i18n::t("(controle detectado)"), pal().ok)
                        } else {
                            (crate::i18n::t("(nenhum controle conectado)"), pal().muted)
                        };
                        ui.label(RichText::new(txt).small().color(col));
                    });
                    ui.checkbox(&mut self.want_kbm, crate::i18n::t("Teclado e mouse"));
                });
                ui.horizontal(|ui| {
                    if !asked {
                        let ok = self.want_pad || self.want_kbm;
                        if ui
                            .add_enabled(ok, egui::Button::new(RichText::new(crate::i18n::t("🎮  Pedir para jogar")).strong().color(Color32::WHITE)).fill(pal().accent))
                            .clicked()
                        {
                            viewer::request_control(self.want_pad, self.want_kbm);
                        }
                    } else if ui.button(crate::i18n::t("Parar de jogar")).clicked() {
                        viewer::request_control(false, false);
                    }
                });
                if asked && vs.ctl_granted == 0 {
                    ui.label(RichText::new(crate::i18n::t("Aguardando o transmissor permitir...")).color(Color32::from_rgb(255, 190, 90)));
                }
                if vs.ctl_granted != 0 {
                    ui.label(RichText::new(crate::tr!("✔ Liberado: {}", "✔ Allowed: {}", crate::control::ctl_desc(vs.ctl_granted))).strong().color(pal().ok));
                    if vs.ctl_granted & crate::control::CTL_PAD != 0 {
                        hint(ui, crate::i18n::t("Seu controle já está ligado ao jogo do transmissor."));
                    }
                    if vs.ctl_granted & crate::control::CTL_KBM != 0 {
                        if vs.kbm_active {
                            ui.label(RichText::new(crate::i18n::t("⌨ Teclado e mouse ATIVOS no jogo do transmissor. Aperte F8 na janela do vídeo para soltar.")).color(pal().ok));
                        } else {
                            hint(ui, crate::i18n::t("Teclado e mouse: clique na janela do vídeo e aperte F8 para começar a jogar (F8 de novo para soltar)."));
                        }
                    }
                }
            });
        }

        // Volume: a mesma informação do painel dentro da janela do vídeo (sincronizado nos dois sentidos).
        let mut ctl = state::VIEW_CTL.lock().unwrap().clone();
        card(ui, crate::i18n::t("Som"), |ui| {
            ui.horizontal(|ui| {
                let label = if ctl.muted { crate::i18n::t("Ativar som") } else { crate::i18n::t("Silenciar") };
                if ui.button(label).clicked() {
                    ctl.muted = !ctl.muted;
                }
                let mut pct = ctl.volume * 100.0;
                if ui
                    .add_enabled(!ctl.muted, egui::Slider::new(&mut pct, 0.0..=200.0).fixed_decimals(0).suffix("%").text(crate::i18n::t("volume")))
                    .changed()
                {
                    ctl.volume = pct / 100.0;
                }
            });
        });

        card(ui, crate::i18n::t("Janela de vídeo"), |ui| {
            ui.horizontal(|ui| {
                if ui.button(crate::i18n::t("Tela cheia")).clicked() {
                    ctl.fullscreen_toggles += 1;
                }
                ui.checkbox(&mut ctl.on_top, crate::i18n::t("Manter sobre as demais janelas"));
            });
            ui.add_space(4.0);
            ui.label(RichText::new(crate::i18n::t("Controles na janela de vídeo")).strong());
            for (k, v) in [
                (crate::i18n::t("Mover o mouse"), crate::i18n::t("exibe o painel de controles no canto inferior direito")),
                (crate::i18n::t("Roda do mouse"), crate::i18n::t("zoom a partir da posição do cursor (para cima amplia, para baixo reduz)")),
                (crate::i18n::t("Arrastar"), crate::i18n::t("com zoom ativo, desloca a imagem")),
                (crate::i18n::t("Botão do meio"), crate::i18n::t("restaura o tamanho original")),
                (crate::i18n::t("Duplo clique / F"), crate::i18n::t("tela cheia")),
                ("M · 9 · 0", crate::i18n::t("silenciar · diminuir · aumentar volume")),
                ("I", crate::i18n::t("fixa as estatísticas no canto da tela")),
                ("Enter", crate::i18n::t("escrever no chat (aparece em cima do vídeo para todos)")),
                ("1 – 6", crate::i18n::t("reações: 👍 😂 🔥 ❤ 😮 GG")),
                ("F8", crate::i18n::t("liga/desliga o controle por teclado e mouse (quando o transmissor permitir)")),
            ] {
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new(k).strong().color(pal().accent));
                    ui.label(RichText::new(v).color(pal().muted));
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
                    if big_button(ui, crate::i18n::t("■   Parar transmissão"), pal().live).clicked() {
                        if let Some(t) = &mut self.host_task {
                            t.stop();
                        }
                    }
                } else if big_button(ui, crate::i18n::t("▶   Iniciar transmissão"), pal().accent).clicked() {
                    self.start_host();
                }
            }
            Tab::View => {
                if self.view_running() {
                    let connected = state::VIEW.lock().unwrap().phase == ViewPhase::Connected;
                    let (label, color) =
                        if connected { (crate::i18n::t("■   Desconectar"), pal().live) } else { (crate::i18n::t("Cancelar"), pal().widget_hover) };
                    if big_button(ui, label, color).clicked() {
                        if let Some(t) = &mut self.view_task {
                            t.stop();
                        }
                    }
                } else {
                    let enabled = !self.invite.trim().is_empty();
                    ui.add_enabled_ui(enabled, |ui| {
                        if big_button(ui, crate::i18n::t("▶   Conectar"), pal().accent).clicked() {
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
            .frame(egui::Frame::window(&ctx.style()).inner_margin(22.0).rounding(pal().radius * 1.2))
            .show(ctx, |ui| {
                ui.vertical_centered(|ui| {
                    ui.label(RichText::new(crate::i18n::t("Transmissão iniciada")).size(22.0).strong());
                    ui.add_space(4.0);
                    ui.label(RichText::new(crate::i18n::t("✅ Convite copiado para a área de transferência")).color(pal().ok).strong());
                    ui.label(crate::i18n::t("Compartilhe o convite (Ctrl+V) e envie a senha por um canal separado."));
                });
                ui.add_space(8.0);
                egui::Frame::none()
                    .fill(pal().input)
                    .rounding(pal().radius * 0.8)
                    .inner_margin(10.0)
                    .show(ui, |ui| {
                        ui.add(
                            egui::Label::new(RichText::new(&code).monospace().small().color(pal().muted)).wrap(),
                        );
                    });
                ui.add_space(6.0);
                ui.label(crate::tr!("🔒 Código de segurança: {code_sec}", "🔒 Security code: {code_sec}"));
                hint(ui, crate::i18n::t("Os espectadores verão este mesmo código. Um código diferente indica interceptação da conexão."));
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if ui.button(crate::i18n::t("📋 Copiar novamente")).clicked() {
                        ui.output_mut(|o| o.copied_text = code.clone());
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui
                            .add(egui::Button::new(RichText::new("   OK   ").strong().color(Color32::WHITE)).fill(pal().accent))
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

impl App {
    /// Teste de conexão opcional: mede o upload até o servidor de encontro e sugere o bitrate.
    fn speed_test_ui(&mut self, ui: &mut egui::Ui) {
        let Some(server) = crate::room::server() else { return };
        let result = self.speed.as_ref().map(|s| s.lock().unwrap().clone());
        ui.horizontal(|ui| {
            let running = matches!(result, Some(None));
            if ui.add_enabled(!running, egui::Button::new(crate::i18n::t("📶 Testar conexão"))).clicked() {
                let slot = std::sync::Arc::new(std::sync::Mutex::new(None));
                self.speed = Some(slot.clone());
                std::thread::spawn(move || {
                    let r = crate::room::speedtest(&server, 6.0).map_err(|e| format!("{e:#}"));
                    *slot.lock().unwrap() = Some(r);
                });
            }
            if running {
                ui.spinner();
                ui.label(RichText::new(crate::i18n::t("Medindo o upload (cerca de 6 segundos)...")).small().color(pal().muted));
            }
        });
        match result {
            Some(Some(Ok(mbps))) => {
                let viewers = self.friends.max(1);
                // 80% do upload, dividido pelos espectadores esperados, entre 3 e 60 Mbps.
                let suggested = ((mbps * 0.8 / viewers as f64).floor() as u32).clamp(3, 60);
                ui.label(crate::tr!(
                    "Upload medido: {:.0} Mbps. Para {} espectador(es), bitrate sugerido: {} Mbps.",
                    "Measured upload: {:.0} Mbps. For {} viewer(s), suggested bitrate: {} Mbps.",
                    mbps, viewers, suggested
                ));
                ui.horizontal(|ui| {
                    ui.label(crate::i18n::t("Espectadores esperados:"));
                    ui.add(egui::DragValue::new(&mut self.friends).range(1..=16));
                    if ui.button(crate::tr!("Usar {} Mbps", "Use {} Mbps", suggested)).clicked() {
                        self.bitrate = suggested;
                    }
                });
                hint(ui, crate::i18n::t("Medido até o servidor mais próximo; a velocidade até cada espectador pode ser menor."));
            }
            Some(Some(Err(e))) => {
                ui.label(RichText::new(crate::tr!("Não foi possível testar: {e}", "Could not run the test: {e}")).small().color(Color32::from_rgb(255, 190, 90)));
            }
            _ => {}
        }
    }
}

fn bitrate_guide(ui: &mut egui::Ui, chosen: u32, friends: &mut u32) {
    egui::Grid::new("guide").striped(true).num_columns(2).spacing([16.0, 6.0]).show(ui, |ui| {
        ui.strong(crate::i18n::t("Mbps"));
        ui.strong(crate::i18n::t("Qualidade esperada (HEVC)"));
        ui.end_row();
        for (r, q) in [
            ("3–5", crate::i18n::t("720p adequado; 1080p perde nitidez em movimentos rápidos")),
            ("8–10", crate::i18n::t("1080p60 boa")),
            ("15", crate::i18n::t("1080p60 muito boa (recomendado)")),
            ("20–25", crate::i18n::t("1080p excelente / 1440p60 boa / ultrawide boa")),
            ("30–40", crate::i18n::t("1440p excelente / 4K60 boa")),
            ("50–60", crate::i18n::t("4K excelente (recomendado apenas em rede local)")),
        ] {
            ui.label(r);
            ui.label(q);
            ui.end_row();
        }
    });
    ui.add_space(6.0);
    ui.label(crate::tr!("• Download mínimo por espectador: {:.0} Mbps.", "• Minimum download per viewer: {:.0} Mbps.", chosen as f64 * 1.3));
    ui.horizontal(|ui| {
        ui.label(crate::i18n::t("• Upload mínimo para"));
        ui.add(egui::DragValue::new(friends).range(1..=16));
        ui.label(crate::tr!("espectador(es): {:.0} Mbps.", "viewer(s): {:.0} Mbps.", (chosen * *friends) as f64 * 1.2));
    });
    hint(ui, crate::i18n::t("Considere a velocidade de upload medida no teste de velocidade, não a de download."));
}

impl App {
    /// Atualização automática. Ao abrir: verifica (até ~6 s) e, havendo versão nova, instala e
    /// reinicia antes de mostrar o app. Com o app aberto: instala sozinho só se não houver transmissão
    /// nem conexão e o usuário estiver há 2 minutos sem mexer; senão fica o botão "Atualizar" no topo.
    /// Devolve true enquanto a tela de atualização ocupa a janela.
    fn auto_update(&mut self, ctx: &egui::Context) -> bool {
        use crate::updater::UpdateState;
        if ctx.input(|i| !i.events.is_empty() || i.pointer.is_moving()) {
            self.last_input = std::time::Instant::now();
        }
        let upd = crate::updater::UPDATE.lock().unwrap().clone();
        let idle = !self.host_running() && !self.view_running() && !self.profile_open;
        if let UpdateState::Available(rel) = &upd {
            let at_startup = self.startup.is_some();
            let quiet = idle && self.last_input.elapsed() > Duration::from_secs(120);
            let looping = crate::updater::just_updated_to().as_deref() == Some(rel.tag.as_str());
            if looping && !self.update_blocked {
                self.update_blocked = true; // não tenta de novo nesta execução; o botão continua no topo
                state::log(crate::tr!(
                    "[atualização] {} foi instalada, mas o app continua na v{}: atualização automática suspensa.",
                    "[update] {} was installed but the app is still v{}: automatic update paused.",
                    rel.tag,
                    crate::updater::VERSION
                ));
            }
            if !self.update_started && !self.update_blocked && !crate::updater::auto_update_disabled() && (at_startup || quiet) {
                self.update_started = true;
                state::log(crate::tr!("Instalando a atualização {} automaticamente...", "Installing update {} automatically...", rel.tag));
                crate::updater::apply(rel.clone());
            }
        }
        // Tela de atualização: enquanto verifica/baixa na abertura, ou durante uma instalação automática.
        let installing = self.update_started && matches!(upd, UpdateState::Available(_) | UpdateState::Downloading(..));
        let checking = self.startup.is_some_and(|t| t.elapsed() < Duration::from_secs(6))
            && matches!(upd, UpdateState::Idle | UpdateState::Checking);
        if !(installing || checking) {
            self.startup = None;
            return false;
        }
        egui::CentralPanel::default().frame(egui::Frame::none().fill(pal().bg)).show(ctx, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(ui.available_height() * 0.35);
                ui.label(RichText::new("GameStream").size(28.0).strong().color(pal().accent));
                ui.add_space(14.0);
                match &upd {
                    UpdateState::Downloading(tag, p) => {
                        ui.label(RichText::new(crate::tr!("Atualizando para {tag}...", "Updating to {tag}...")).size(16.0));
                        ui.add_space(8.0);
                        ui.add(egui::ProgressBar::new(*p).desired_width(320.0).show_percentage());
                        ui.add_space(6.0);
                        hint(ui, crate::i18n::t("O GameStream reabre sozinho em seguida."));
                    }
                    UpdateState::Available(rel) => {
                        ui.spinner();
                        ui.label(crate::tr!("Atualizando para {}...", "Updating to {}...", rel.tag));
                    }
                    _ => {
                        ui.spinner();
                        ui.label(RichText::new(crate::i18n::t("Procurando atualizações...")).color(pal().muted));
                    }
                }
            });
        });
        true
    }

    /// Janela "Seu perfil": nome (até 24 caracteres, emojis permitidos) e foto.
    fn profile_window(&mut self, ctx: &egui::Context) {
        if !self.profile_open {
            return;
        }
        let mut open = true;
        egui::Window::new(crate::i18n::t("Seu perfil"))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .open(&mut open)
            .show(ctx, |ui| {
                ui.set_width(380.0);
                ui.horizontal(|ui| {
                    let nick = if self.nick_edit.trim().is_empty() { crate::settings::nick() } else { self.nick_edit.clone() };
                    let tex = self.avatars.get(ui.ctx(), "me", self.my_avatar.as_ref());
                    avatar_circle(ui, tex.as_ref(), &nick, 72.0);
                    ui.vertical(|ui| {
                        if ui.button(crate::i18n::t("🖼  Escolher foto...")).clicked() {
                            let pick = rfd::FileDialog::new()
                                .set_title(crate::i18n::t("Escolha uma foto de perfil"))
                                .add_filter(crate::i18n::t("Imagens (PNG, JPG, WebP, GIF)"), &crate::avatar::EXTENSIONS)
                                .pick_file();
                            if let Some(f) = pick {
                                self.profile_msg = Some(match crate::avatar::import(&f) {
                                    Ok(()) => {
                                        self.my_avatar = crate::avatar::load();
                                        (true, crate::i18n::t("Foto atualizada.").into())
                                    }
                                    Err(e) => (false, format!("{e:#}")),
                                });
                            }
                        }
                        if self.my_avatar.is_some() && ui.button(crate::i18n::t("Remover foto")).clicked() {
                            crate::avatar::remove();
                            self.my_avatar = None;
                        }
                        hint(ui, crate::i18n::t("PNG, JPG, WebP ou GIF, até 8 MB. A foto é cortada em quadrado e reduzida para 128×128."));
                    });
                });
                ui.add_space(8.0);
                ui.label(crate::i18n::t("Nome (aparece no chat e na lista de quem está assistindo):"));
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.nick_edit)
                            .char_limit(24)
                            .hint_text(crate::settings::nick())
                            .desired_width(300.0),
                    );
                    ui.label(RichText::new(format!("{}/24", self.nick_edit.chars().count())).small().color(pal().muted));
                });
                hint(ui, crate::i18n::t("Até 24 caracteres; pode usar emojis. Vazio = nome do usuário do Windows."));
                if let Some((ok, msg)) = &self.profile_msg {
                    ui.label(RichText::new(msg).small().color(if *ok { pal().ok } else { Color32::from_rgb(255, 190, 90) }));
                }
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if ui.add(egui::Button::new(RichText::new(crate::i18n::t("Salvar")).strong().color(Color32::WHITE)).fill(pal().accent)).clicked() {
                        crate::settings::set_nick(&self.nick_edit);
                        self.profile_open = false;
                    }
                    hint(ui, crate::i18n::t("Vale a partir da próxima conexão ou transmissão."));
                });
            });
        if !open {
            self.profile_open = false;
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        ctx.request_repaint_after(Duration::from_millis(250));
        if self.auto_update(ctx) {
            return;
        }

        // Abre o popup (e copia o convite) assim que a transmissão fica pronta.
        let invite = state::HOST.lock().unwrap().invite.clone();
        if let Some(code) = invite {
            if self.host_running() && self.popup_done_for.as_ref() != Some(&code) {
                ctx.output_mut(|o| o.copied_text = code.clone());
                state::log(crate::i18n::t("Convite copiado para a área de transferência."));
                self.popup = Some(code.clone());
                self.popup_done_for = Some(code);
                self.tab = Tab::Host;
            }
        }

        let host_live = self.host_running();
        let view_live = self.view_running();
        egui::TopBottomPanel::top("top")
            .frame(egui::Frame::none().fill(pal().header).inner_margin(egui::Margin::symmetric(16.0, 10.0)))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("GameStream").size(20.0).strong().color(pal().accent));
                    ui.add_space(12.0);
                    ui.selectable_value(&mut self.tab, Tab::Host, RichText::new(crate::i18n::t("  Transmitir  ")).size(16.0));
                    ui.selectable_value(&mut self.tab, Tab::View, RichText::new(crate::i18n::t("  Assistir  ")).size(16.0));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.toggle_value(&mut self.show_log, "Log");
                        let nick = crate::settings::nick();
                        let tex = self.avatars.get(ui.ctx(), "me", self.my_avatar.as_ref());
                        let r = ui
                            .add(egui::Button::new(RichText::new(&nick).strong()).frame(false))
                            .on_hover_text(crate::i18n::t("Seu perfil: nome e foto que aparecem para os outros"));
                        let a = avatar_circle(ui, tex.as_ref(), &nick, 26.0);
                        if r.clicked() || a.interact(egui::Sense::click()).clicked() {
                            self.profile_open = true;
                            self.nick_edit = crate::settings::nick_raw();
                            self.profile_msg = None;
                        }
                        ui.menu_button("⚙", |ui| {
                            ui.label(RichText::new(crate::i18n::t("Idioma")).small().color(pal().muted));
                            let en = crate::settings::is_en();
                            if ui.radio(!en, "Português").clicked() {
                                crate::settings::set_en(false);
                                ui.close_menu();
                            }
                            if ui.radio(en, "English").clicked() {
                                crate::settings::set_en(true);
                                ui.close_menu();
                            }
                            ui.separator();
                            ui.label(RichText::new(crate::i18n::t("Tema")).small().color(pal().muted));
                            let cur = crate::settings::theme();
                            for th in crate::settings::Theme::ALL {
                                if ui.radio(cur == th, th.label()).clicked() {
                                    crate::settings::set_theme(th);
                                    setup_style(ui.ctx());
                                    ui.close_menu();
                                }
                            }
                        })
                        .response
                        .on_hover_text(crate::i18n::t("Idioma e tema"));
                        let upd = crate::updater::UPDATE.lock().unwrap().clone();
                        match upd {
                            crate::updater::UpdateState::Available(rel) => {
                                // Texto curto quando a barra está apertada (não cobre as abas).
                                let label = if ui.available_width() > 260.0 {
                                    crate::tr!("⬆ Atualizar para {}", "⬆ Update to {}", rel.tag)
                                } else {
                                    format!("⬆ {}", rel.tag)
                                };
                                let b = egui::Button::new(RichText::new(label).color(Color32::WHITE).strong()).fill(pal().ok);
                                if ui
                                    .add(b)
                                    .on_hover_text(crate::tr!("Atualizar para {}: baixa a nova versão e reinicia o GameStream", "Update to {}: downloads the new version and restarts GameStream", rel.tag))
                                    .clicked()
                                {
                                    crate::updater::apply(rel);
                                }
                            }
                            crate::updater::UpdateState::Downloading(tag, p) => {
                                ui.label(RichText::new(crate::tr!("Baixando {tag}... {:.0}%", "Downloading {tag}... {:.0}%", p * 100.0)).small().color(pal().muted));
                            }
                            crate::updater::UpdateState::Error(e) => {
                                ui.label(RichText::new(format!("v{}", crate::updater::VERSION)).small().color(pal().muted))
                                    .on_hover_text(crate::tr!("Não foi possível verificar atualizações: {e}", "Could not check for updates: {e}"));
                            }
                            _ => {
                                ui.label(RichText::new(format!("v{}", crate::updater::VERSION)).small().color(pal().muted));
                            }
                        }
                        if ui.available_width() < 130.0 {
                            // Sem espaço (janela estreita): o status já aparece dentro de cada aba.
                        } else if host_live {
                            pill(ui, crate::i18n::t("AO VIVO"), pal().live);
                        } else if view_live {
                            if state::VIEW.lock().unwrap().phase == ViewPhase::Connected {
                                pill(ui, crate::i18n::t("CONECTADO"), pal().ok);
                            } else {
                                pill(ui, crate::i18n::t("CONECTANDO"), Color32::from_rgb(255, 190, 90));
                            }
                        }
                    });
                });
            });

        if self.show_log {
            egui::TopBottomPanel::bottom("log").resizable(true).default_height(180.0).show(ctx, |ui| {
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new(crate::i18n::t("Detalhes técnicos")).strong());
                    if ui.small_button(crate::i18n::t("Copiar log")).clicked() {
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
            .frame(egui::Frame::none().fill(pal().header).inner_margin(egui::Margin::symmetric(16.0, 12.0)))
            .show(ctx, |ui| self.action_bar(ui));

        egui::CentralPanel::default()
            .frame(egui::Frame::none().fill(pal().bg).inner_margin(16.0))
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| match self.tab {
                    Tab::Host => self.host_ui(ui),
                    Tab::View => self.view_ui(ui),
                });
            });

        self.invite_popup(ctx);
        self.profile_window(ctx);
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

/// "há 5 min", "há 2 h", "há 3 dias".
fn ago(ts: u64) -> String {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(ts);
    let d = now.saturating_sub(ts);
    if d < 60 {
        crate::i18n::t("agora").into()
    } else if d < 3600 {
        crate::tr!("há {} min", "{} min ago", d / 60)
    } else if d < 86400 {
        crate::tr!("há {} h", "{} h ago", d / 3600)
    } else {
        crate::tr!("há {} dias", "{} days ago", d / 86400)
    }
}
