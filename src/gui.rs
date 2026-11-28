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

/// Abre a interface. `join` = código de sala vindo de um link (entra direto na sala).
/// `hidden` = começa só na bandeja (iniciado junto com o Windows).
/// Começou só na bandeja: o eframe mostra a janela depois do 1º quadro; ela é escondida no 2º.
static START_HIDDEN: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

pub fn run(join: Option<String>, hidden: bool) -> eframe::Result<()> {
    if hidden {
        START_HIDDEN.store(2, std::sync::atomic::Ordering::Relaxed);
    }
    if join.is_some() {
        *crate::link::PENDING.lock().unwrap() = join;
    }
    crate::link::register_scheme();
    let opts = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("GameStream")
            .with_inner_size([640.0, 820.0])
            .with_min_inner_size([520.0, 560.0])
            .with_visible(!hidden)
            .with_icon(eframe::icon_data::from_png_bytes(include_bytes!("../assets/icon.png")).unwrap_or_default()),
        ..Default::default()
    };
    eframe::run_native(
        "GameStream",
        opts,
        Box::new(|cc| {
            setup_style(&cc.egui_ctx);
            let app = App::new();
            // Recebe links abertos enquanto o app está aberto.
            app.rt.spawn(crate::link::serve(cc.egui_ctx.clone()));
            crate::tray::start(cc.egui_ctx.clone());
            Ok(Box::new(app))
        }),
    )
}

fn img_json(i: &crate::control::Image) -> serde_json::Value {
    serde_json::json!({ "b": i.brightness, "c": i.contrast, "s": i.saturation, "g": i.gamma })
}

/// Lista de presets de imagem. `cur` = None quer dizer "igual ao transmissor" (só para quem assiste).
/// Devolve Some(nova escolha) quando muda.
fn image_combo(ui: &mut egui::Ui, id: &str, cur: Option<crate::control::Image>, allow_host: bool) -> Option<Option<crate::control::Image>> {
    use crate::control::{Image, IMAGE_PRESETS};
    let text = match cur {
        None => crate::i18n::t("Igual ao transmissor").to_string(),
        Some(i) => i.describe().to_string(),
    };
    let mut out = None;
    egui::ComboBox::from_id_salt(id).selected_text(text).show_ui(ui, |ui| {
        if allow_host && ui.selectable_label(cur.is_none(), crate::i18n::t("Igual ao transmissor")).clicked() {
            out = Some(None);
        }
        for (pid, img) in IMAGE_PRESETS {
            if ui.selectable_label(cur == Some(img), Image::label(pid)).clicked() {
                out = Some(Some(img));
            }
        }
        let custom = cur.is_some_and(|i| i.preset_id().is_none());
        if ui.selectable_label(custom, Image::label("")).clicked() && !custom {
            out = Some(Some(cur.unwrap_or_default()));
        }
    });
    out
}

/// Ajuste fino (saturação, contraste, brilho, gama). Devolve o novo ajuste quando muda.
fn image_sliders(ui: &mut egui::Ui, cur: crate::control::Image) -> Option<crate::control::Image> {
    let mut img = cur;
    egui::CollapsingHeader::new(crate::i18n::t("Ajuste fino")).id_salt(ui.next_auto_id()).show(ui, |ui| {
        ui.add(egui::Slider::new(&mut img.saturation, -100..=100).text(crate::i18n::t("Saturação")));
        ui.add(egui::Slider::new(&mut img.contrast, -100..=100).text(crate::i18n::t("Contraste")));
        ui.add(egui::Slider::new(&mut img.brightness, -100..=100).text(crate::i18n::t("Brilho")));
        ui.add(egui::Slider::new(&mut img.gamma, -100..=100).text(crate::i18n::t("Gama")));
    });
    (img != cur).then_some(img)
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

/// Gráfico de linha simples (últimos minutos). `extra` = segunda série (tracejada), ex.: o alvo.
fn line_chart(ui: &mut egui::Ui, title: &str, unit: &str, values: &[f32], color: Color32, extra: Option<(&[f32], Color32)>) {
    let w = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(egui::vec2(w, 74.0), egui::Sense::hover());
    let p = ui.painter_at(rect);
    p.rect_filled(rect, pal().radius * 0.6, pal().input);
    let cur = values.last().copied().unwrap_or(0.0);
    let font = egui::FontId::proportional(12.0);
    p.text(rect.left_top() + egui::vec2(8.0, 5.0), egui::Align2::LEFT_TOP, title, font.clone(), pal().muted);
    let cur_txt = if cur >= 10.0 || cur == cur.trunc() { format!("{cur:.0} {unit}") } else { format!("{cur:.1} {unit}") };
    p.text(rect.right_top() + egui::vec2(-8.0, 5.0), egui::Align2::RIGHT_TOP, cur_txt, font.clone(), color);
    let max = values
        .iter()
        .chain(extra.map(|(e, _)| e).unwrap_or(&[]).iter())
        .copied()
        .fold(0.0f32, f32::max)
        .max(1.0)
        * 1.15;
    let plot = egui::Rect::from_min_max(rect.left_top() + egui::vec2(6.0, 22.0), rect.right_bottom() - egui::vec2(6.0, 6.0));
    p.text(plot.left_top() + egui::vec2(2.0, -1.0), egui::Align2::LEFT_TOP, format!("{max:.0}"), egui::FontId::proportional(9.0), pal().muted);
    // Janela de tempo: pelo menos 60 amostras (o gráfico já começa largo) e no máximo o histórico.
    let span = values.len().clamp(60, state::HISTORY);
    let to_pts = |vals: &[f32]| -> Vec<egui::Pos2> {
        let vals = &vals[vals.len().saturating_sub(span)..];
        let n = span.max(2) as f32 - 1.0;
        let off = span.saturating_sub(vals.len()) as f32;
        vals.iter()
            .enumerate()
            .map(|(i, v)| egui::pos2(plot.left() + (off + i as f32) / n * plot.width(), plot.bottom() - (v / max).clamp(0.0, 1.0) * plot.height()))
            .collect()
    };
    if let Some((e, c)) = extra {
        let pts = to_pts(e);
        for seg in pts.windows(2).step_by(2) {
            p.line_segment([seg[0], seg[1]], Stroke::new(1.0_f32, c));
        }
    }
    let pts = to_pts(values);
    if pts.len() >= 2 {
        // Preenchimento sob a linha: um trapézio (convexo) por trecho.
        for seg in pts.windows(2) {
            let quad = vec![seg[0], seg[1], egui::pos2(seg[1].x, plot.bottom()), egui::pos2(seg[0].x, plot.bottom())];
            p.add(egui::Shape::convex_polygon(quad, color.gamma_multiply(0.12), Stroke::NONE));
        }
        p.add(egui::Shape::line(pts, Stroke::new(1.6_f32, color)));
    } else {
        p.text(plot.center(), egui::Align2::CENTER_CENTER, crate::i18n::t("coletando dados..."), font, pal().muted);
    }
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
    follow_game: bool,
    audio_delay: u32,
    host_image: crate::control::Image,
    view_image: Option<crate::control::Image>,
    win_refreshed: std::time::Instant,
    host_task: Option<Task>,
    live_desc: String,
    tray_hint_shown: bool,
    autostart: Option<bool>,
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
    /// Salas recentes ao vivo agora (código -> ao vivo) e quando foi a última consulta.
    live_rooms: Arc<std::sync::Mutex<HashMap<String, bool>>>,
    live_checked: Option<std::time::Instant>,
    /// Sala para entrar assim que a conexão atual terminar (link aberto enquanto assistia outra).
    join_after_stop: Option<String>,
    /// Câmeras do PC (listadas em segundo plano) e a escolhida.
    cam_devices: Arc<std::sync::Mutex<Option<Vec<String>>>>,
    cam_sel: String,
    /// Preferências já salvas (para gravar só quando algo muda) e quando mudou por último.
    saved_prefs: String,
    prefs_changed: Option<std::time::Instant>,
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
            follow_game: true,
            audio_delay: 0,
            host_image: crate::control::Image::default(),
            view_image: None,
            win_refreshed: std::time::Instant::now(),
            host_task: None,
            live_desc: String::new(),
            tray_hint_shown: false,
            autostart: None,
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
            live_rooms: Arc::new(std::sync::Mutex::new(HashMap::new())),
            live_checked: None,
            saved_prefs: String::new(),
            prefs_changed: None,
            join_after_stop: None,
            cam_devices: Arc::new(std::sync::Mutex::new(None)),
            cam_sel: String::new(),
        };
        a.refresh();
        a.load_prefs();
        a.saved_prefs = a.prefs_json().to_string();
        crate::updater::check_in_background();
        a
    }

    fn refresh(&mut self) {
        self.monitors = sources::monitors();
        self.windows = sources::windows_for_picker();
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
            SrcKind::Window => match self.sel_window.clone() {
                Some((mut hwnd, title)) => {
                    // A janela escolhida pode ter fechado (ex.: a janela da partida do LoL, que some
                    // quando o jogo acaba): procura outra com o mesmo nome; se não houver, explica.
                    if !sources::window_alive(hwnd) {
                        self.windows = sources::windows_for_picker();
                        match self.windows.iter().find(|w| w.title == title) {
                            Some(w) => {
                                hwnd = w.hwnd;
                                self.sel_window = Some((hwnd, title.clone()));
                            }
                            None => {
                                self.sel_window = None;
                                state::host(|h| {
                                    h.error = Some(crate::tr!(
                                        "A janela \"{title}\" fechou. Escolha de novo na lista. No LoL, escolha \"League of Legends\" (cliente do LoL) e deixe \"Seguir o jogo\" ligado: a partida entra sozinha na transmissão.",
                                        "The window \"{title}\" closed. Pick again from the list. For LoL, pick \"League of Legends\" (LoL client) and keep \"Follow the game\" on: the match joins the stream by itself."
                                    ))
                                });
                                return;
                            }
                        }
                    }
                    // Minimizada não dá para capturar: restaura sem roubar o foco.
                    if self.windows.iter().any(|w| w.hwnd == hwnd && w.minimized) || !sources::window_shown(hwnd) {
                        sources::restore_no_activate(hwnd);
                        std::thread::sleep(Duration::from_millis(300));
                    }
                    self.live_desc = crate::tr!("Janela: {title}", "Window: {title}");
                    Source::Window { hwnd, title }
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
        host::set_follow(self.follow_game);
        host::set_image(self.host_image);
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
                    // A lista se atualiza sozinha (janelas abrem e fecham, ex.: a partida do LoL).
                    if self.win_refreshed.elapsed() > Duration::from_secs(2) {
                        self.win_refreshed = std::time::Instant::now();
                        self.windows = sources::windows_for_picker();
                    }
                    egui::ComboBox::from_id_salt("window")
                        .width(ui.available_width())
                        .selected_text(sel)
                        .show_ui(ui, |ui| {
                            for w in &self.windows {
                                let label = if w.note.is_empty() { w.title.clone() } else { format!("{}   ({})", w.title, w.note) };
                                ui.selectable_value(&mut self.sel_window, Some((w.hwnd, w.title.clone())), label);
                            }
                        });
                    hint(ui, crate::i18n::t("Captura nativa do Windows: a janela pode ser movida, redimensionada ou sobreposta sem interromper a transmissão. Evite minimizá-la."));
                    ui.checkbox(&mut self.follow_game, crate::i18n::t("🎮 Seguir o jogo (cliente do LoL, Riot Client, Steam, Battle.net...)"))
                        .on_hover_text(crate::i18n::t("Transmitindo o launcher, quando ele abrir o jogo a transmissão passa sozinha para a janela do jogo, e volta para o launcher quando o jogo fechar. Coloque o jogo em tela cheia sem bordas ou em janela."));
                }
            }
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label(crate::i18n::t("🎨 Imagem para quem assiste:"));
                if let Some(img) = image_combo(ui, "img_setup", Some(self.host_image), false) {
                    self.host_image = img.unwrap_or_default();
                    host::set_image(self.host_image);
                }
            });
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
            let trusted = crate::settings::trusted_count();
            if self.approval && trusted > 0 {
                ui.horizontal(|ui| {
                    hint(ui, &crate::tr!(
                        "{} espectador(es) que você já aprovou entram direto.",
                        "{} viewer(s) you approved before join directly.",
                        trusted
                    ));
                    if ui.small_button(crate::i18n::t("Esquecer todos")).on_hover_text(crate::i18n::t("Todos voltam a passar pela sala de espera")).clicked() {
                        crate::settings::clear_trusted();
                    }
                });
            }
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
                egui::CollapsingHeader::new(crate::i18n::t("📈 Gráfico da transmissão (últimos 6 minutos)")).default_open(false).show(ui, |ui| {
                    let out: Vec<f32> = st.history.iter().map(|s| s.mbps).collect();
                    let target: Vec<f32> = st.history.iter().map(|s| s.fps).collect();
                    line_chart(ui, crate::i18n::t("Taxa de saída (linha tracejada: bitrate alvo)"), "Mbps", &out, pal().ok, Some((&target, pal().muted)));
                });
                if let Some(f) = &st.follow {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(f).small().color(pal().accent));
                        if ui.small_button(crate::i18n::t("Desligar")).on_hover_text(crate::i18n::t("Para de trocar sozinho entre o launcher e o jogo")).clicked() {
                            self.follow_game = false;
                            host::set_follow(false);
                        }
                    });
                } else if self.src_kind == SrcKind::Window && !self.follow_game {
                    if ui.small_button(crate::i18n::t("🎮 Seguir o jogo")).on_hover_text(crate::i18n::t("Quando o launcher abrir o jogo, a transmissão troca sozinha (e volta quando o jogo fechar)")).clicked() {
                        self.follow_game = true;
                        host::set_follow(true);
                    }
                }
                if st.paused {
                    ui.label(
                        RichText::new(crate::i18n::t("⏸ Ninguém assistindo: a codificação está pausada para poupar a placa de vídeo. Volta sozinha quando alguém entrar."))
                            .small()
                            .color(pal().muted),
                    );
                }
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

            // Privacidade, gravação e replay.
            card(ui, crate::i18n::t("🎬 Privacidade, gravação e replay"), |ui| {
                ui.horizontal_wrapped(|ui| {
                    if st.privacy {
                        if ui
                            .add(egui::Button::new(RichText::new(crate::i18n::t("▶ Voltar a transmitir")).strong().color(Color32::WHITE)).fill(pal().live))
                            .on_hover_text(crate::i18n::t("Desliga o modo privacidade (Ctrl+F9)"))
                            .clicked()
                        {
                            host::set_privacy(false);
                        }
                    } else if ui
                        .button(crate::i18n::t("🙈 Modo privacidade"))
                        .on_hover_text(crate::i18n::t("Os espectadores veem \"Volto já\" em vez da sua tela, sem som, até você voltar. Atalho: Ctrl+F9 (funciona dentro do jogo)."))
                        .clicked()
                    {
                        host::set_privacy(true);
                    }
                    let rec_label = match &st.recording {
                        Some((t, _)) => {
                            let s = t.elapsed().as_secs();
                            crate::tr!("⏹ Parar gravação ({:02}:{:02})", "⏹ Stop recording ({:02}:{:02})", s / 60, s % 60)
                        }
                        None => crate::i18n::t("⏺ Gravar").to_string(),
                    };
                    if ui
                        .button(rec_label)
                        .on_hover_text(crate::i18n::t("Grava em MP4 exatamente o que os espectadores recebem, sem pesar mais na placa de vídeo. Arquivos em Vídeos\\GameStream."))
                        .clicked()
                    {
                        host::toggle_record();
                    }
                    let replay_on = crate::settings::replay();
                    if ui
                        .add_enabled(replay_on, egui::Button::new(crate::i18n::t("💾 Salvar replay (30 s)")))
                        .on_hover_text(crate::i18n::t("Salva os últimos 30 segundos da transmissão. Atalho: Ctrl+F10 (funciona dentro do jogo)."))
                        .clicked()
                    {
                        host::save_replay();
                    }
                });
                if st.privacy {
                    ui.label(RichText::new(crate::i18n::t("🙈 Modo privacidade ligado: os espectadores veem \"Volto já\" (sem imagem e sem som).")).strong().color(pal().live));
                }
                if st.recording.is_some() {
                    ui.label(RichText::new(crate::i18n::t("● Gravando")).small().color(pal().live));
                }
                ui.horizontal(|ui| {
                    let mut replay_on = crate::settings::replay();
                    if ui
                        .checkbox(&mut replay_on, crate::i18n::t("Replay instantâneo"))
                        .on_hover_text(crate::i18n::t("Guarda sempre os últimos 30 segundos na memória (cerca de 80 MB a 20 Mbps) para salvar uma jogada na hora."))
                        .changed()
                    {
                        crate::settings::set_replay(replay_on);
                    }
                    if replay_on && st.replay_secs > 0 {
                        hint(ui, &crate::tr!("{} s guardados", "{} s stored", st.replay_secs));
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui
                            .small_button(crate::i18n::t("📁 Trocar pasta"))
                            .on_hover_text(crate::i18n::t("Escolhe onde salvar as gravações e os replays"))
                            .clicked()
                        {
                            let pick = rfd::FileDialog::new()
                                .set_title(crate::i18n::t("Onde salvar as gravações e os replays"))
                                .set_directory(crate::record::display_dir())
                                .pick_folder();
                            if let Some(d) = pick {
                                let d = d.display().to_string();
                                state::log(crate::tr!("[gravação] vídeos serão salvos em {d}", "[recording] videos will be saved to {d}"));
                                crate::settings::set_videos_dir(Some(d));
                            }
                        }
                        if ui.small_button(crate::i18n::t("📂 Abrir pasta")).clicked() {
                            let dir = crate::record::videos_dir();
                            let _ = std::fs::create_dir_all(&dir);
                            let target = st.last_saved.clone().filter(|p| std::path::Path::new(p).exists());
                            let _ = match target {
                                Some(p) => std::process::Command::new("explorer").arg(format!("/select,{p}")).spawn(),
                                None => std::process::Command::new("explorer").arg(&dir).spawn(),
                            };
                        }
                    });
                });
                ui.horizontal(|ui| {
                    hint(ui, &crate::tr!("Salvando em: {}", "Saving to: {}", crate::record::display_dir().display()));
                    if crate::settings::videos_dir().is_some()
                        && ui.small_button(crate::i18n::t("Usar a pasta padrão")).on_hover_text(crate::i18n::t("Volta a salvar em Vídeos\\GameStream")).clicked()
                    {
                        crate::settings::set_videos_dir(None);
                    }
                });
                hint(ui, crate::i18n::t("Atalhos durante o jogo: Ctrl+F9 = modo privacidade · Ctrl+F10 = salvar replay."));
            });

            // Quem consegue assistir: avisa quando só quem tem IPv6 (ou VPN/rede local) consegue entrar.
            if st.reach_level > 0 || st.blocked_tries > 0 {
                card(ui, crate::i18n::t("📡 Quem consegue te assistir"), |ui| {
                    let warn = Color32::from_rgb(255, 190, 90);
                    let text = match st.reach_level {
                        1 => crate::i18n::t("⚠ Só quem tem IPv6 consegue te assistir agora. Amigos sem IPv6 ficam em \"tempo de conexão esgotado\"."),
                        2 => crate::i18n::t("⚠ Só quem está na mesma VPN (Tailscale) consegue te assistir."),
                        3 => crate::i18n::t("⚠ Só quem está na sua rede local consegue te assistir."),
                        _ => crate::i18n::t("Todos conseguem te assistir."),
                    };
                    ui.label(RichText::new(text).strong().color(warn));
                    if st.blocked_tries > 0 {
                        ui.label(RichText::new(crate::tr!(
                            "{} tentativa(s) de entrar sem caminho até você (a internet da pessoa não tem IPv6).",
                            "{} attempt(s) to join with no path to you (their internet has no IPv6).",
                            st.blocked_tries
                        )).color(warn));
                    }
                    if st.cgnat {
                        hint(ui, crate::i18n::t("Sua operadora usa CGNAT (IPv4 compartilhado): abrir porta não resolve. Peça um IPv4 público à operadora, ou usem IPv6 ou o Tailscale."));
                    } else {
                        ui.label(RichText::new(crate::i18n::t("Para liberar para todos (ligar o UPnP no modem):")).strong());
                        let gw = st.gateway.clone().unwrap_or_else(|| "192.168.0.1".into());
                        hint(ui, &crate::tr!(
                            "1. Abra http://{gw} no navegador e entre com o usuário e a senha da etiqueta do modem.\n2. Procure \"UPnP\" (em Rede, Avançado ou NAT) e ative.\n3. Pare e comece a transmissão de novo: o GameStream abre a porta sozinho.\nSem UPnP no modem: crie um redirecionamento da porta UDP 9000 para este PC.",
                            "1. Open http://{gw} in the browser and sign in with the username and password on the router's label.\n2. Look for \"UPnP\" (under Network, Advanced or NAT) and turn it on.\n3. Stop and start the stream again: GameStream opens the port by itself.\nNo UPnP on the router: forward UDP port 9000 to this PC."
                        ));
                        if ui.button(crate::tr!("🌐 Abrir a página do modem ({gw})", "🌐 Open the router page ({gw})")).clicked() {
                            let _ = std::process::Command::new("explorer").arg(format!("http://{gw}")).spawn();
                        }
                    }
                    hint(ui, crate::i18n::t("Outras saídas: o amigo ativa o IPv6 no modem ou pede para a operadora (ele confere em test-ipv6.com), ou vocês dois usam o Tailscale (gratuito)."));
                });
            }

            card(ui, crate::i18n::t("Convite"), |ui| {
                if let Some(room) = &st.room {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(crate::i18n::t("Código da sala")).color(pal().muted));
                        ui.label(RichText::new(room).size(26.0).strong().monospace().color(pal().accent));
                        if ui.small_button("📋").on_hover_text(crate::i18n::t("Copiar código da sala")).clicked() {
                            ui.output_mut(|o| o.copied_text = room.clone());
                        }
                        if let Some(link) = crate::room::parse_code(room).and_then(|c| crate::link::web_link(&c)) {
                            if ui
                                .small_button(crate::i18n::t("🔗 Copiar link"))
                                .on_hover_text(crate::tr!(
                                    "Quem clicar no link entra direto na sala (com o GameStream instalado):\n{link}",
                                    "Whoever clicks the link joins the room directly (with GameStream installed):\n{link}"
                                ))
                                .clicked()
                            {
                                ui.output_mut(|o| o.copied_text = link.clone());
                                state::log(crate::tr!("Link copiado: {link}", "Link copied: {link}"));
                            }
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
                // O convite completo (GS-...) só é necessário sem o servidor de salas: com ele no ar,
                // o código da sala e o link já entregam esse convite automaticamente.
                if !st.room_online {
                    if st.room.is_some() {
                        hint(ui, crate::i18n::t("Sem o servidor de salas no momento: envie o convite completo."));
                    }
                    if ui
                        .add(egui::Button::new(RichText::new(crate::i18n::t("📋  Copiar convite")).strong().color(Color32::WHITE)).fill(pal().accent))
                        .clicked()
                    {
                        if let Some(code) = &st.invite {
                            ui.output_mut(|o| o.copied_text = code.clone());
                            state::log(crate::i18n::t("Convite copiado."));
                        }
                    }
                }
                ui.label(crate::tr!("🔒 Código de segurança: {}", "🔒 Security code: {}", st.security_code))
                    .on_hover_text(crate::i18n::t("Os espectadores verão este mesmo código. Um código diferente indica interceptação da conexão."));
                egui::CollapsingHeader::new(crate::i18n::t("Endereços de conexão")).show(ui, |ui| {
                    for r in &st.reach {
                        hint(ui, r);
                    }
                    if st.room_online {
                        if ui
                            .small_button(crate::i18n::t("📋 Copiar convite completo"))
                            .on_hover_text(crate::i18n::t("Só é necessário se o servidor de salas sair do ar: quem colar o convite completo entra sem ele."))
                            .clicked()
                        {
                            if let Some(code) = &st.invite {
                                ui.output_mut(|o| o.copied_text = code.clone());
                                state::log(crate::i18n::t("Convite copiado."));
                            }
                        }
                    }
                });
            });

            // Com o servidor de salas no ar a liberação é automática; o código de resposta (GS-R-...)
            // só existe para quem entrou colando o convite completo.
            if !st.room_online { card(ui, crate::i18n::t("Liberar espectador"), |ui| {
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
            }); }

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
                                    .on_hover_text(crate::i18n::t("Fica lembrado: nas próximas transmissões, esta pessoa entra direto"))
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
                if st.low_active {
                    ui.label(
                        RichText::new(crate::tr!(
                            "🐢 Versão leve ativa (720p, 30 FPS, {} Mbps) para quem tem a conexão mais fraca",
                            "🐢 Light version active (720p, 30 FPS, {} Mbps) for weaker connections",
                            st.low_bitrate
                        ))
                        .small()
                        .color(pal().muted),
                    );
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
                                    if v.low {
                                        ui.label(RichText::new(crate::i18n::t("🐢 versão leve")).small().color(warn)).on_hover_text(crate::i18n::t(
                                            "A conexão dele está mais fraca que a dos outros: ele recebe uma versão mais leve (720p, 30 FPS) e os outros continuam na qualidade normal. Volta sozinho quando a conexão melhorar.",
                                        ));
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
                                if ui
                                    .small_button(crate::i18n::t("Remover"))
                                    .on_hover_text(crate::i18n::t("Desconecta, bloqueia até o fim da transmissão e esquece a aprovação (volta a passar pela sala de espera)"))
                                    .clicked()
                                {
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
            self.cam_card(ui, true, st.cam_on, st.cam_error.clone());
            card(ui, crate::i18n::t("🎨 Imagem para quem assiste"), |ui| {
                if let Some(img) = image_combo(ui, "img_host", Some(self.host_image), false) {
                    self.host_image = img.unwrap_or_default();
                    host::set_image(self.host_image);
                }
                if let Some(img) = image_sliders(ui, self.host_image) {
                    self.host_image = img;
                    host::set_image(img);
                }
                hint(ui, crate::i18n::t("Como o controle de imagem de uma TV: o ajuste é aplicado no player de quem assiste (na placa de vídeo dele), então não pesa na transmissão nem perde qualidade. Cada espectador pode trocar só para ele. A gravação fica sem o ajuste."));
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
                // Quais estão ao vivo: consulta o servidor de encontro a cada 60 s (só com a janela em foco).
                let focused = ui.ctx().input(|i| i.focused);
                if focused && self.live_checked.map_or(true, |t| t.elapsed() > Duration::from_secs(60)) {
                    self.live_checked = Some(std::time::Instant::now());
                    if let Some(server) = crate::room::server() {
                        let codes: Vec<String> = recent.iter().map(|r| r.code.clone()).collect();
                        let live = self.live_rooms.clone();
                        std::thread::spawn(move || {
                            if let Ok(m) = crate::room::status(&server, &codes) {
                                *live.lock().unwrap() = m;
                            }
                        });
                    }
                }
                let live = self.live_rooms.lock().unwrap().clone();
                let mut recent = recent;
                recent.sort_by_key(|r| !live.get(&r.code).copied().unwrap_or(false)); // ao vivo primeiro
                let mut connect_to: Option<String> = None;
                let (avatars, recent_av) = (&mut self.avatars, &self.recent_avatars.1);
                card(ui, crate::i18n::t("Salas recentes"), |ui| {
                    for r in &recent {
                        ui.horizontal(|ui| {
                            let tex = avatars.get(ui.ctx(), &format!("r:{}", r.code), recent_av.get(&r.code));
                            avatar_circle(ui, tex.as_ref(), &r.name, 32.0);
                            ui.vertical(|ui| {
                                ui.horizontal(|ui| {
                                    ui.label(RichText::new(if r.name.is_empty() { crate::room::display(&r.code) } else { r.name.clone() }).strong());
                                    match live.get(&r.code) {
                                        Some(true) => pill(ui, crate::i18n::t("AO VIVO"), pal().live),
                                        Some(false) => {
                                            ui.label(RichText::new(crate::i18n::t("offline")).small().color(pal().muted));
                                        }
                                        None => {}
                                    }
                                });
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
        if !running && vs.phase == ViewPhase::Idle {
            if let Some(n) = &vs.notice {
                card(ui, "", |ui| {
                    ui.label(RichText::new(n).size(16.0).strong());
                    hint(ui, crate::i18n::t("Quando a pessoa voltar a transmitir, a sala aparece como AO VIVO em \"Salas recentes\"; é só clicar em Entrar."));
                });
            }
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
            if vs.privacy {
                ui.label(RichText::new(crate::i18n::t("⏸ O transmissor pausou a imagem por um instante (modo privacidade). Volta já.")).strong().color(Color32::from_rgb(255, 190, 90)));
            }
            if vs.low {
                ui.label(RichText::new(crate::i18n::t("🐢 Sua conexão está mais fraca que a dos outros: você recebe a versão leve (720p, 30 FPS) para não travar. Volta à qualidade normal sozinho.")).small().color(pal().muted));
            }
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
            egui::CollapsingHeader::new(crate::i18n::t("📈 Gráfico da conexão (últimos 3 minutos)")).default_open(true).show(ui, |ui| {
                let h: Vec<state::Sample> = vs.history.iter().copied().collect();
                let col = |f: fn(&state::Sample) -> f32| h.iter().map(f).collect::<Vec<f32>>();
                let warn = Color32::from_rgb(255, 190, 90);
                ui.columns(2, |c| {
                    line_chart(&mut c[0], crate::i18n::t("Ping"), "ms", &col(|s| s.ping), pal().accent, None);
                    line_chart(&mut c[1], crate::i18n::t("Perda de pacotes"), "%", &col(|s| s.loss), warn, None);
                });
                ui.add_space(4.0);
                ui.columns(2, |c| {
                    line_chart(&mut c[0], crate::i18n::t("Taxa recebida"), "Mbps", &col(|s| s.mbps), pal().ok, None);
                    line_chart(&mut c[1], crate::i18n::t("FPS"), "", &col(|s| s.fps), Color32::from_rgb(120, 180, 255), None);
                });
                hint(ui, crate::i18n::t("Picos de ping ou de perda que se repetem indicam Wi-Fi ou conexão instável."));
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
            if s.audio_db.is_some() {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(crate::i18n::t("🔊 Sincronia do som")).small().color(pal().muted));
                    if ui
                        .add(egui::Slider::new(&mut self.audio_delay, 0..=400).suffix(" ms").step_by(10.0))
                        .on_hover_text(crate::i18n::t("Som adiantado em relação à imagem? Atrase o som até a voz e os efeitos casarem com a imagem (vale na hora)."))
                        .changed()
                    {
                        crate::audio::set_audio_delay(self.audio_delay);
                    }
                });
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
                                if m.cam {
                                    ui.label(RichText::new("📷").small()).on_hover_text(crate::i18n::t("Câmera ligada"));
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
            self.cam_card(ui, false, vs.cam_on, vs.cam_error.clone());
            card(ui, crate::i18n::t("🎨 Imagem"), |ui| {
                let host_desc = vs.host_image.describe();
                ui.horizontal(|ui| {
                    if let Some(img) = image_combo(ui, "img_view", self.view_image, true) {
                        self.view_image = img;
                        viewer::set_image_override(img);
                    }
                    if self.view_image.is_none() {
                        hint(ui, &crate::tr!("(o transmissor escolheu: {host_desc})", "(the streamer chose: {host_desc})"));
                    }
                });
                if let Some(cur) = self.view_image {
                    if let Some(img) = image_sliders(ui, cur) {
                        self.view_image = Some(img);
                        viewer::set_image_override(Some(img));
                    }
                }
                hint(ui, crate::i18n::t("Ajuste de cor só na sua tela (feito pela sua placa de vídeo, sem perder qualidade)."));
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
                (crate::i18n::t("Botão direito"), crate::i18n::t("aponta um lugar no vídeo para todos (some em 1 segundo)")),
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
    /// Card "Câmera": ligar/desligar a sua câmera e mostrar (ou não) as dos outros.
    fn cam_card(&mut self, ui: &mut egui::Ui, hosting: bool, cam_on: bool, cam_error: Option<String>) {
        // Lista as câmeras uma vez (o FFmpeg demora ~1 s); "⟳" lista de novo.
        if self.cam_devices.lock().unwrap().is_none() {
            *self.cam_devices.lock().unwrap() = Some(Vec::new());
            let list = self.cam_devices.clone();
            std::thread::spawn(move || {
                let found = crate::updater::ffmpeg_present().map(|f| crate::cam::list(&f)).unwrap_or_default();
                *list.lock().unwrap() = Some(found);
            });
        }
        let devices = self.cam_devices.lock().unwrap().clone().unwrap_or_default();
        card(ui, crate::i18n::t("📷 Câmera"), |ui| {
            if cam_on {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(crate::i18n::t("● Sua câmera está ligada")).strong().color(pal().live));
                    if ui.button(crate::i18n::t("Desligar câmera")).clicked() {
                        if hosting { host::set_camera(None) } else { viewer::set_camera(None) }
                    }
                });
            } else {
                ui.horizontal(|ui| {
                    let label = if self.cam_sel.is_empty() { crate::i18n::t("Câmera padrão").to_string() } else { self.cam_sel.clone() };
                    egui::ComboBox::from_id_salt(if hosting { "cam_h" } else { "cam_v" })
                        .width(ui.available_width() - 190.0)
                        .selected_text(label)
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut self.cam_sel, String::new(), crate::i18n::t("Câmera padrão"));
                            for d in &devices {
                                ui.selectable_value(&mut self.cam_sel, d.clone(), d);
                            }
                        });
                    if ui.small_button("⟳").on_hover_text(crate::i18n::t("Procurar câmeras de novo")).clicked() {
                        *self.cam_devices.lock().unwrap() = None;
                    }
                    if ui
                        .add(egui::Button::new(RichText::new(crate::i18n::t("📷 Ligar minha câmera")).strong().color(Color32::WHITE)).fill(pal().accent))
                        .clicked()
                    {
                        let dev = Some(self.cam_sel.clone());
                        if hosting { host::set_camera(dev) } else { viewer::set_camera(dev) }
                    }
                });
            }
            if let Some(e) = cam_error {
                ui.label(RichText::new(format!("⚠ {e}")).small().color(Color32::from_rgb(255, 190, 90)));
            }
            let mut show = crate::settings::show_cams();
            if ui.checkbox(&mut show, crate::i18n::t("Mostrar as câmeras dos outros")).changed() {
                crate::settings::set_show_cams(show);
            }
            let hidden = crate::cam::hidden_list();
            if !hidden.is_empty() {
                ui.label(RichText::new(crate::i18n::t("Vídeos desativados (só para você):")).small().color(pal().muted));
                for name in hidden {
                    ui.horizontal(|ui| {
                        ui.label(format!("🚫 {name}"));
                        if ui.small_button(crate::i18n::t("Mostrar")).clicked() {
                            crate::cam::unhide(&name);
                        }
                    });
                }
            }
            hint(ui, crate::i18n::t("As câmeras aparecem em janelinhas no canto da tela: arraste para mover (elas se encaixam sem ficar uma por cima da outra) e use a roda do mouse para mudar o tamanho. Botão direito: \"Desativar vídeo\" esconde a câmera daquela pessoa só para você. Cada câmera usa ~0,35 Mbps. Desligada por padrão."));
        });
    }

    /// Últimas escolhas da interface (sem a senha), salvas em settings.json.
    fn prefs_json(&self) -> serde_json::Value {
        let audio_sel = match &self.audio_sel {
            AudioSel::GameWindow => serde_json::json!({ "kind": "game" }),
            AudioSel::AllExceptDiscord => serde_json::json!({ "kind": "no-discord" }),
            AudioSel::DefaultOutput => serde_json::json!({ "kind": "default" }),
            AudioSel::App { name, .. } => serde_json::json!({ "kind": "app", "name": name }),
            AudioSel::Output(n) => serde_json::json!({ "kind": "output", "name": n }),
            AudioSel::Input(n) => serde_json::json!({ "kind": "input", "name": n }),
        };
        serde_json::json!({
            "tab": if self.tab == Tab::View { "view" } else { "host" },
            "source": if self.src_kind == SrcKind::Window { "window" } else { "monitor" },
            "monitor": self.monitors.get(self.sel_monitor).map(|m| m.name.clone()),
            "window": self.sel_window.as_ref().map(|(_, t)| t.clone()),
            "preview": self.preview,
            "follow_game": self.follow_game,
            "audio_delay": self.audio_delay,
            "image": img_json(&self.host_image),
            "view_image": self.view_image.as_ref().map(img_json),
            "audio": self.audio,
            "audio_sel": audio_sel,
            "audio_pcm": self.audio_pcm,
            "mic": self.mic,
            "auto_bitrate": self.auto_bitrate,
            "bitrate": self.bitrate,
            "fps": self.fps,
            "upnp": self.upnp,
            "encoder": self.encoder,
            "approval": self.approval,
            "friends": self.friends,
            "view_out": self.view_out,
            "volume": self.volume,
            "muted": self.muted,
            "want_pad": self.want_pad,
            "want_kbm": self.want_kbm,
            "cam_device": self.cam_sel,
        })
    }

    /// Restaura as últimas escolhas (o que não existir mais, como uma janela fechada, fica no padrão).
    fn load_prefs(&mut self) {
        let p = crate::settings::prefs();
        if !p.is_object() {
            return;
        }
        let b = |k: &str, d: bool| p[k].as_bool().unwrap_or(d);
        let n = |k: &str, d: u32| p[k].as_u64().map(|v| v as u32).unwrap_or(d);
        self.tab = if p["tab"] == "view" { Tab::View } else { Tab::Host };
        if let Some(name) = p["monitor"].as_str() {
            if let Some(i) = self.monitors.iter().position(|m| m.name == name) {
                self.sel_monitor = i;
            }
        }
        self.src_kind = SrcKind::Monitor;
        if p["source"] == "window" {
            if let Some(w) = p["window"].as_str().and_then(|t| self.windows.iter().find(|w| w.title == t)) {
                self.sel_window = Some((w.hwnd, w.title.clone()));
                self.src_kind = SrcKind::Window;
            }
        }
        self.last_src_kind = self.src_kind;
        self.preview = b("preview", self.preview);
        self.follow_game = b("follow_game", self.follow_game);
        self.audio_delay = n("audio_delay", self.audio_delay).min(500);
        crate::audio::set_audio_delay(self.audio_delay);
        if p["image"].is_object() {
            self.host_image = crate::control::Image::from_json(&p["image"]);
            host::set_image(self.host_image);
        }
        self.view_image = p["view_image"].is_object().then(|| crate::control::Image::from_json(&p["view_image"]));
        viewer::set_image_override(self.view_image);
        self.audio = b("audio", self.audio);
        let name = p["audio_sel"]["name"].as_str().unwrap_or("").to_string();
        self.audio_sel = match p["audio_sel"]["kind"].as_str() {
            Some("game") if self.src_kind == SrcKind::Window => AudioSel::GameWindow,
            Some("default") => AudioSel::DefaultOutput,
            Some("app") if !name.is_empty() => AudioSel::App {
                pid: self.audio_apps.iter().find(|(_, n)| *n == name).map(|(p, _)| *p).unwrap_or(0),
                name,
            },
            Some("output") if self.out_devices.contains(&name) => AudioSel::Output(name),
            Some("input") if self.in_devices.contains(&name) => AudioSel::Input(name),
            _ if self.src_kind == SrcKind::Window => AudioSel::GameWindow,
            _ => AudioSel::AllExceptDiscord,
        };
        self.audio_pcm = b("audio_pcm", self.audio_pcm);
        self.mic = p["mic"].as_str().filter(|m| self.in_devices.iter().any(|d| d == m)).map(String::from);
        self.auto_bitrate = b("auto_bitrate", self.auto_bitrate);
        self.bitrate = n("bitrate", self.bitrate).clamp(host::MIN_BITRATE, 200);
        self.fps = match n("fps", self.fps) {
            f @ (30 | 60 | 120) => f,
            _ => self.fps,
        };
        self.upnp = b("upnp", self.upnp);
        if let Some(e) = p["encoder"].as_str() {
            self.encoder = e.to_string();
        }
        self.approval = b("approval", self.approval);
        self.friends = n("friends", self.friends).clamp(1, 20);
        self.view_out = p["view_out"].as_str().filter(|d| self.out_devices.iter().any(|o| o == d)).map(String::from);
        self.volume = p["volume"].as_f64().map(|v| v as f32).unwrap_or(self.volume).clamp(0.0, 2.0);
        self.muted = b("muted", self.muted);
        self.want_pad = b("want_pad", self.want_pad);
        self.want_kbm = b("want_kbm", self.want_kbm);
        self.cam_sel = p["cam_device"].as_str().unwrap_or("").to_string();
    }

    /// Grava as escolhas quando mudam (no máximo uma vez por segundo, para não gravar a cada quadro
    /// enquanto um controle deslizante é arrastado).
    fn save_prefs_if_changed(&mut self) {
        let now = self.prefs_json().to_string();
        if now == self.saved_prefs {
            self.prefs_changed = None;
            return;
        }
        let since = *self.prefs_changed.get_or_insert_with(std::time::Instant::now);
        if since.elapsed() >= Duration::from_secs(1) {
            if let Ok(v) = serde_json::from_str(&now) {
                crate::settings::set_prefs(v);
            }
            self.saved_prefs = now;
            self.prefs_changed = None;
        }
    }

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
        // Na bandeja ninguém vê a janela: atualiza bem devagar (quase nada de processador).
        ctx.request_repaint_after(Duration::from_millis(if crate::tray::is_hidden() { 2000 } else { 250 }));
        match START_HIDDEN.load(std::sync::atomic::Ordering::Relaxed) {
            2 => {
                START_HIDDEN.store(1, std::sync::atomic::Ordering::Relaxed);
                ctx.request_repaint();
            }
            1 => {
                START_HIDDEN.store(0, std::sync::atomic::Ordering::Relaxed);
                crate::tray::hide_main();
            }
            _ => {}
        }
        // Fechar a janela: fica na bandeja (como o Discord), a não ser que a pessoa tenha pedido "Sair"
        // pelo menu da bandeja ou desligado a opção. Saindo de verdade: encerra a transmissão/sessão.
        if ctx.input(|i| i.viewport().close_requested()) {
            let quitting = crate::tray::QUIT.load(std::sync::atomic::Ordering::Relaxed);
            if !quitting && crate::settings::close_to_tray() && crate::tray::active() {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                crate::tray::hide_main();
                if !self.tray_hint_shown {
                    self.tray_hint_shown = true;
                    state::log(crate::i18n::t("O GameStream continua aberto na bandeja (perto do relógio). Para sair, clique com o botão direito no ícone e em \"Sair\"."));
                }
                return;
            }
            if let Some(t) = self.host_task.as_mut().filter(|t| t.running()) {
                t.stop();
            }
            if let Some(t) = self.view_task.as_mut().filter(|t| t.running()) {
                t.stop();
            }
            crate::tray::remove();
        }
        if self.auto_update(ctx) {
            return;
        }
        // Link "entrar na sala": vai para Assistir e conecta (troca de sala se já estiver assistindo).
        if let Some(code) = crate::link::PENDING.lock().unwrap().take() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            self.tab = Tab::View;
            if let Some(t) = self.view_task.as_mut().filter(|t| t.running()) {
                t.stop();
            }
            self.join_after_stop = Some(code);
        }
        if !self.view_running() {
            if let Some(code) = self.join_after_stop.take() {
                state::log(crate::tr!("Entrando na sala {} pelo link...", "Joining room {} from the link...", crate::room::display(&code)));
                self.invite = crate::room::display(&code);
                self.start_view();
            }
        }
        // Fechou a pré-visualização no X: vale como escolha (não reabre nesta nem nas próximas
        // transmissões); volta só pelo botão "Exibir pré-visualização" ou pela opção antes de iniciar.
        if self.host_running() && self.preview && !host::preview_wanted() {
            self.preview = false;
        }
        self.save_prefs_if_changed();


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
                            let mut snd = crate::settings::sounds();
                            if ui
                                .checkbox(&mut snd, crate::i18n::t("🔔 Sons de aviso"))
                                .on_hover_text(crate::i18n::t("Toca um som quando alguém entra, pede para jogar ou manda mensagem"))
                                .changed()
                            {
                                crate::settings::set_sounds(snd);
                                if snd {
                                    crate::sounds::play(crate::sounds::Sound::Join);
                                }
                            }
                            let mut to_tray = crate::settings::close_to_tray();
                            if ui
                                .checkbox(&mut to_tray, crate::i18n::t("🗕 Fechar deixa na bandeja"))
                                .on_hover_text(crate::i18n::t("Fechar a janela deixa o GameStream perto do relógio (continua transmitindo ou assistindo). Para sair, botão direito no ícone > Sair."))
                                .changed()
                            {
                                crate::settings::set_close_to_tray(to_tray);
                            }
                            let auto = *self.autostart.get_or_insert_with(crate::settings::autostart);
                            let mut a = auto;
                            if ui
                                .checkbox(&mut a, crate::i18n::t("🚀 Iniciar com o Windows"))
                                .on_hover_text(crate::i18n::t("Abre o GameStream na bandeja quando você liga o PC (para ver quem está ao vivo nas salas recentes)."))
                                .changed()
                            {
                                crate::settings::set_autostart(a);
                                self.autostart = Some(a);
                            }
                            if ui
                                .button(crate::i18n::t("📄 Abrir pasta de logs"))
                                .on_hover_text(crate::i18n::t("Registro do que o app fez (no máximo ~10 MB). Útil para mandar quando algo der errado."))
                                .clicked()
                            {
                                let _ = std::fs::create_dir_all(crate::logfile::dir());
                                let _ = std::process::Command::new("explorer").arg(crate::logfile::dir()).spawn();
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
