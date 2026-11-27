//! Controle remoto ("jogar junto"): o espectador joga no PC do transmissor, com permissão dele.
//!
//! Transmissor: as teclas e o mouse recebidos são injetados com SendInput (por scancode, que é o que
//! os jogos leem) e o controle vira um controle de Xbox 360 virtual (driver ViGEmBus).
//! Espectador: o controle é lido pelo XInput; teclado e mouse são capturados por hooks de baixo nível
//! somente enquanto o modo de controle estiver ligado (F8) e a janela do vídeo estiver em primeiro
//! plano. Nesse modo as teclas e o mouse não chegam ao PC do espectador.

use std::{
    collections::HashSet,
    sync::{
        atomic::{AtomicBool, AtomicU32, Ordering},
        LazyLock, Mutex,
    },
};

use anyhow::{Context, Result};
use windows::Win32::{
    Foundation::{HINSTANCE, LPARAM, LRESULT, POINT, WPARAM},
    System::{LibraryLoader::GetModuleHandleW, Threading::GetCurrentThreadId},
    UI::{
        Input::{
            KeyboardAndMouse::{
                SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP,
                KEYEVENTF_SCANCODE, MOUSEEVENTF_HWHEEL, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN,
                MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_WHEEL,
                MOUSEEVENTF_XDOWN, MOUSEEVENTF_XUP, MOUSEINPUT, MOUSE_EVENT_FLAGS,
            },
            XboxController::{XInputGetState, XINPUT_STATE},
        },
        WindowsAndMessaging::{
            CallNextHookEx, DispatchMessageW, GetCursorPos, GetForegroundWindow, GetMessageW, GetWindowThreadProcessId,
            PostThreadMessageW, SetWindowsHookExW, TranslateMessage, UnhookWindowsHookEx, KBDLLHOOKSTRUCT, MSG,
            MSLLHOOKSTRUCT, WH_KEYBOARD_LL, WH_MOUSE_LL, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MBUTTONDOWN, WM_MBUTTONUP,
            WM_MOUSEHWHEEL, WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_QUIT, WM_RBUTTONDOWN, WM_RBUTTONUP, WM_XBUTTONDOWN,
            WM_XBUTTONUP,
        },
    },
};

use crate::control::Pad;

// =====================================================================================
// Transmissor: injeção
// =====================================================================================

/// Teclas que nunca são repassadas ao PC do transmissor (abririam o menu Iniciar etc.).
fn blocked_key(sc: u16, ext: bool) -> bool {
    ext && matches!(sc, 0x5B | 0x5C | 0x5D) // Win esquerda, Win direita, tecla Menu
}

fn send_inputs(inputs: &[INPUT]) {
    unsafe {
        SendInput(inputs, std::mem::size_of::<INPUT>() as i32);
    }
}

fn key_input(sc: u16, ext: bool, down: bool) -> INPUT {
    let mut flags = KEYEVENTF_SCANCODE;
    if ext {
        flags |= KEYEVENTF_EXTENDEDKEY;
    }
    if !down {
        flags |= KEYEVENTF_KEYUP;
    }
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 { ki: KEYBDINPUT { wVk: Default::default(), wScan: sc, dwFlags: flags, time: 0, dwExtraInfo: 0 } },
    }
}

fn mouse_input(flags: MOUSE_EVENT_FLAGS, dx: i32, dy: i32, data: i32) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT { dx, dy, mouseData: data as u32, dwFlags: flags, time: 0, dwExtraInfo: 0 },
        },
    }
}

/// Botões do mouse: 0 esquerdo, 1 direito, 2 meio, 3 e 4 laterais.
fn button_input(b: u8, down: bool) -> Option<INPUT> {
    let (flags, data) = match (b, down) {
        (0, true) => (MOUSEEVENTF_LEFTDOWN, 0),
        (0, false) => (MOUSEEVENTF_LEFTUP, 0),
        (1, true) => (MOUSEEVENTF_RIGHTDOWN, 0),
        (1, false) => (MOUSEEVENTF_RIGHTUP, 0),
        (2, true) => (MOUSEEVENTF_MIDDLEDOWN, 0),
        (2, false) => (MOUSEEVENTF_MIDDLEUP, 0),
        (3, true) => (MOUSEEVENTF_XDOWN, 1),
        (3, false) => (MOUSEEVENTF_XUP, 1),
        (4, true) => (MOUSEEVENTF_XDOWN, 2),
        (4, false) => (MOUSEEVENTF_XUP, 2),
        _ => return None,
    };
    Some(mouse_input(flags, 0, 0, data))
}

/// Entrada de um espectador no PC do transmissor. Lembra o que está pressionado para soltar tudo
/// quando a permissão é revogada ou a conexão cai (senão uma tecla ficaria "presa").
#[derive(Default)]
pub struct Injector {
    keys: HashSet<(u16, bool)>,
    buttons: u8,
    pad: Option<vigem_client::Xbox360Wired<vigem_client::Client>>,
    last_pad: Option<Pad>,
}

impl Injector {
    pub fn key(&mut self, sc: u16, ext: bool, down: bool) {
        if sc == 0 || blocked_key(sc, ext) {
            return;
        }
        if down {
            self.keys.insert((sc, ext));
        } else if !self.keys.remove(&(sc, ext)) {
            return;
        }
        send_inputs(&[key_input(sc, ext, down)]);
    }

    pub fn button(&mut self, b: u8, down: bool) {
        if b > 4 {
            return;
        }
        let bit = 1u8 << b;
        if down {
            self.buttons |= bit;
        } else if self.buttons & bit == 0 {
            return;
        } else {
            self.buttons &= !bit;
        }
        if let Some(i) = button_input(b, down) {
            send_inputs(&[i]);
        }
    }

    pub fn wheel(&mut self, delta: i32, horizontal: bool) {
        let delta = delta.clamp(-1200, 1200);
        send_inputs(&[mouse_input(if horizontal { MOUSEEVENTF_HWHEEL } else { MOUSEEVENTF_WHEEL }, 0, 0, delta)]);
    }

    pub fn mouse_move(&mut self, dx: i32, dy: i32) {
        if dx != 0 || dy != 0 {
            send_inputs(&[mouse_input(MOUSEEVENTF_MOVE, dx, dy, 0)]);
        }
    }

    /// Atualiza o controle virtual (conecta um controle de Xbox 360 na primeira vez).
    pub fn pad(&mut self, p: &Pad) -> Result<()> {
        if self.pad.is_none() {
            let client = vigem_client::Client::connect().map_err(|e| match e {
                vigem_client::Error::BusNotFound => anyhow::anyhow!(crate::i18n::t(
                    "o driver ViGEmBus não está instalado neste PC (necessário para controles virtuais). Baixe em github.com/nefarius/ViGEmBus/releases"
                )),
                e => anyhow::anyhow!("ViGEmBus: {e:?}"),
            })?;
            let mut t = vigem_client::Xbox360Wired::new(client, vigem_client::TargetId::XBOX360_WIRED);
            t.plugin().map_err(|e| anyhow::anyhow!("ViGEmBus: {e:?}"))?;
            t.wait_ready().map_err(|e| anyhow::anyhow!("ViGEmBus: {e:?}"))?;
            self.pad = Some(t);
        }
        if self.last_pad.as_ref() == Some(p) {
            return Ok(());
        }
        let g = vigem_client::XGamepad {
            buttons: vigem_client::XButtons(p.buttons),
            left_trigger: p.lt,
            right_trigger: p.rt,
            thumb_lx: p.lx,
            thumb_ly: p.ly,
            thumb_rx: p.rx,
            thumb_ry: p.ry,
        };
        self.pad.as_mut().context("pad")?.update(&g).map_err(|e| anyhow::anyhow!("ViGEmBus: {e:?}"))?;
        self.last_pad = Some(*p);
        Ok(())
    }

    /// Solta todas as teclas e botões do mouse.
    pub fn release_kbm(&mut self) {
        let mut inputs: Vec<INPUT> = self.keys.drain().map(|(sc, ext)| key_input(sc, ext, false)).collect();
        for b in 0..5u8 {
            if self.buttons & (1 << b) != 0 {
                inputs.extend(button_input(b, false));
            }
        }
        self.buttons = 0;
        if !inputs.is_empty() {
            send_inputs(&inputs);
        }
    }

    /// Desconecta o controle virtual.
    pub fn release_pad(&mut self) {
        if let Some(mut t) = self.pad.take() {
            let _ = t.update(&vigem_client::XGamepad::default());
            let _ = t.unplug();
        }
        self.last_pad = None;
    }
}

impl Drop for Injector {
    fn drop(&mut self) {
        self.release_kbm();
        self.release_pad();
    }
}

// =====================================================================================
// Espectador: captura
// =====================================================================================

/// Evento de entrada capturado no PC do espectador.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum InputEv {
    Key { sc: u16, ext: bool, down: bool },
    Button { b: u8, down: bool },
    Wheel { delta: i32, horizontal: bool },
    Move { dx: i32, dy: i32 },
    /// Modo de controle por teclado e mouse ligado/desligado (F8).
    Capturing(bool),
}

/// Teclado e mouse liberados pelo transmissor.
static KBM_ALLOWED: AtomicBool = AtomicBool::new(false);
/// Modo de controle ligado (F8) — enquanto ligado, teclado e mouse vão para o transmissor.
static CAPTURING: AtomicBool = AtomicBool::new(false);
/// Processo da janela do vídeo (mpv): a captura só acontece com ela em primeiro plano.
static VIDEO_PID: AtomicU32 = AtomicU32::new(0);
static HOOK_THREAD: AtomicU32 = AtomicU32::new(0);
static EVENTS: LazyLock<Mutex<Option<tokio::sync::mpsc::UnboundedSender<InputEv>>>> = LazyLock::new(|| Mutex::new(None));
/// Teclas/botões pressionados durante a captura (para soltar no transmissor ao desligar).
static HELD: LazyLock<Mutex<(HashSet<(u16, bool)>, u8)>> = LazyLock::new(|| Mutex::new((HashSet::new(), 0)));

const VK_F8: u32 = 0x77;
const LLKHF_EXTENDED: u32 = 0x01;
const LLKHF_INJECTED: u32 = 0x10;
const LLKHF_UP: u32 = 0x80;
const LLMHF_INJECTED: u32 = 0x01;

fn emit(ev: InputEv) {
    if let Some(tx) = EVENTS.lock().unwrap().as_ref() {
        let _ = tx.send(ev);
    }
}

fn video_in_front() -> bool {
    let pid = VIDEO_PID.load(Ordering::Relaxed);
    if pid == 0 {
        return false;
    }
    let mut fg = 0u32;
    unsafe {
        GetWindowThreadProcessId(GetForegroundWindow(), Some(&mut fg));
    }
    fg == pid
}

/// Desliga o modo de controle e solta no transmissor o que ainda estiver pressionado.
fn stop_capturing() {
    if CAPTURING.swap(false, Ordering::Relaxed) {
        let (keys, buttons) = {
            let mut h = HELD.lock().unwrap();
            (h.0.drain().collect::<Vec<_>>(), std::mem::take(&mut h.1))
        };
        for (sc, ext) in keys {
            emit(InputEv::Key { sc, ext, down: false });
        }
        for b in 0..5u8 {
            if buttons & (1 << b) != 0 {
                emit(InputEv::Button { b, down: false });
            }
        }
        emit(InputEv::Capturing(false));
    }
}

unsafe extern "system" fn kb_proc(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if code >= 0 && KBM_ALLOWED.load(Ordering::Relaxed) {
        let k = &*(lp.0 as *const KBDLLHOOKSTRUCT);
        if k.flags.0 & LLKHF_INJECTED == 0 {
            let down = k.flags.0 & LLKHF_UP == 0;
            if k.vkCode == VK_F8 && video_in_front() {
                if down {
                    if CAPTURING.load(Ordering::Relaxed) {
                        stop_capturing();
                    } else {
                        CAPTURING.store(true, Ordering::Relaxed);
                        emit(InputEv::Capturing(true));
                    }
                }
                return LRESULT(1);
            }
            if CAPTURING.load(Ordering::Relaxed) {
                if !video_in_front() {
                    stop_capturing();
                } else {
                    let sc = k.scanCode as u16;
                    let ext = k.flags.0 & LLKHF_EXTENDED != 0;
                    {
                        let mut h = HELD.lock().unwrap();
                        if down {
                            h.0.insert((sc, ext));
                        } else {
                            h.0.remove(&(sc, ext));
                        }
                    }
                    emit(InputEv::Key { sc, ext, down });
                    return LRESULT(1);
                }
            }
        }
    }
    CallNextHookEx(None, code, wp, lp)
}

unsafe extern "system" fn mouse_proc(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if code >= 0 && CAPTURING.load(Ordering::Relaxed) {
        let m = &*(lp.0 as *const MSLLHOOKSTRUCT);
        if m.flags & LLMHF_INJECTED == 0 {
            if !video_in_front() {
                stop_capturing();
                return CallNextHookEx(None, code, wp, lp);
            }
            let hi = (m.mouseData >> 16) as u16;
            let ev = match wp.0 as u32 {
                WM_MOUSEMOVE => {
                    // O movimento é engolido (o cursor fica parado); a diferença é o deslocamento.
                    let mut cur = POINT::default();
                    let _ = GetCursorPos(&mut cur);
                    Some(InputEv::Move { dx: m.pt.x - cur.x, dy: m.pt.y - cur.y })
                }
                WM_LBUTTONDOWN => Some(InputEv::Button { b: 0, down: true }),
                WM_LBUTTONUP => Some(InputEv::Button { b: 0, down: false }),
                WM_RBUTTONDOWN => Some(InputEv::Button { b: 1, down: true }),
                WM_RBUTTONUP => Some(InputEv::Button { b: 1, down: false }),
                WM_MBUTTONDOWN => Some(InputEv::Button { b: 2, down: true }),
                WM_MBUTTONUP => Some(InputEv::Button { b: 2, down: false }),
                WM_XBUTTONDOWN => Some(InputEv::Button { b: if hi == 2 { 4 } else { 3 }, down: true }),
                WM_XBUTTONUP => Some(InputEv::Button { b: if hi == 2 { 4 } else { 3 }, down: false }),
                WM_MOUSEWHEEL => Some(InputEv::Wheel { delta: hi as i16 as i32, horizontal: false }),
                WM_MOUSEHWHEEL => Some(InputEv::Wheel { delta: hi as i16 as i32, horizontal: true }),
                _ => None,
            };
            if let Some(ev) = ev {
                if let InputEv::Button { b, down } = ev {
                    let mut h = HELD.lock().unwrap();
                    if down {
                        h.1 |= 1 << b;
                    } else {
                        h.1 &= !(1 << b);
                    }
                }
                emit(ev);
                return LRESULT(1);
            }
        }
    }
    CallNextHookEx(None, code, wp, lp)
}

/// Liga os hooks de teclado/mouse (quando o transmissor libera teclado e mouse).
/// `tx` recebe os eventos; `video_pid` é o processo da janela do vídeo.
pub fn start_kbm_capture(tx: tokio::sync::mpsc::UnboundedSender<InputEv>, video_pid: u32) {
    *EVENTS.lock().unwrap() = Some(tx);
    VIDEO_PID.store(video_pid, Ordering::Relaxed);
    KBM_ALLOWED.store(true, Ordering::Relaxed);
    if HOOK_THREAD.load(Ordering::Relaxed) != 0 {
        return;
    }
    std::thread::spawn(|| unsafe {
        HOOK_THREAD.store(GetCurrentThreadId(), Ordering::Relaxed);
        let hmod = GetModuleHandleW(None).map(|h| HINSTANCE(h.0)).unwrap_or_default();
        let kb = SetWindowsHookExW(WH_KEYBOARD_LL, Some(kb_proc), hmod, 0);
        let ms = SetWindowsHookExW(WH_MOUSE_LL, Some(mouse_proc), hmod, 0);
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        if let Ok(h) = kb {
            let _ = UnhookWindowsHookEx(h);
        }
        if let Ok(h) = ms {
            let _ = UnhookWindowsHookEx(h);
        }
        HOOK_THREAD.store(0, Ordering::Relaxed);
    });
}

/// Desliga a captura de teclado/mouse e remove os hooks.
pub fn stop_kbm_capture() {
    stop_capturing();
    KBM_ALLOWED.store(false, Ordering::Relaxed);
    *EVENTS.lock().unwrap() = None;
    let tid = HOOK_THREAD.load(Ordering::Relaxed);
    if tid != 0 {
        unsafe {
            let _ = PostThreadMessageW(tid, WM_QUIT, WPARAM(0), LPARAM(0));
        }
    }
}

pub fn kbm_capturing() -> bool {
    CAPTURING.load(Ordering::Relaxed)
}

/// Lê o primeiro controle XInput conectado (Xbox, ou PlayStation via Steam/DS4Windows).
pub fn read_pad() -> Option<Pad> {
    for i in 0..4 {
        let mut st = XINPUT_STATE::default();
        if unsafe { XInputGetState(i, &mut st) } == 0 {
            let g = st.Gamepad;
            return Some(Pad {
                buttons: g.wButtons.0,
                lt: g.bLeftTrigger,
                rt: g.bRightTrigger,
                lx: g.sThumbLX,
                ly: g.sThumbLY,
                rx: g.sThumbRX,
                ry: g.sThumbRY,
            });
        }
    }
    None
}
