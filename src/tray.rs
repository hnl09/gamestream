//! Ícone na bandeja do Windows (perto do relógio) e atalhos globais de quem transmite.
//!
//! Fechar a janela deixa o GameStream na bandeja (como o Discord): ele continua transmitindo ou
//! assistindo, e as "Salas recentes" continuam sendo acompanhadas. Clique no ícone: abre a janela.
//! Botão direito: menu (Abrir / Sair). Enquanto transmite, os atalhos globais ficam ativos:
//!   Ctrl+F9  = modo privacidade (os espectadores veem "Volto já"),
//!   Ctrl+F10 = salvar o replay (os últimos 30 s).

use std::sync::{
    atomic::{AtomicBool, AtomicIsize, Ordering},
    OnceLock,
};

use windows::{
    core::{w, PCWSTR},
    Win32::{
        Foundation::{BOOL, HWND, LPARAM, LRESULT, POINT, WPARAM},
        System::LibraryLoader::GetModuleHandleW,
        UI::{
            Input::KeyboardAndMouse::{RegisterHotKey, UnregisterHotKey, MOD_CONTROL, MOD_NOREPEAT, VK_F10, VK_F9},
            Shell::{Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY, NOTIFYICONDATAW},
            WindowsAndMessaging::{
                AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, DispatchMessageW, EnumWindows, GetCursorPos,
                GetMessageW, GetSystemMetrics, GetWindowTextW, GetWindowThreadProcessId, IsIconic, LoadIconW, LoadImageW, PostMessageW,
                RegisterClassW, RegisterWindowMessageW, SetForegroundWindow, SetTimer, ShowWindow, TrackPopupMenu, TranslateMessage,
                HICON, IDI_APPLICATION, IMAGE_ICON, LR_DEFAULTCOLOR, MF_SEPARATOR, MF_STRING, MSG, SM_CXSMICON, SM_CYSMICON,
                SW_HIDE, SW_RESTORE, SW_SHOW, TPM_BOTTOMALIGN, TPM_RETURNCMD, TPM_RIGHTBUTTON, WINDOW_EX_STYLE, WM_APP, WM_CLOSE,
                WM_HOTKEY, WM_LBUTTONDBLCLK, WM_LBUTTONUP, WM_RBUTTONUP, WM_TIMER, WNDCLASSW, WS_OVERLAPPED,
            },
        },
    },
};

const WM_TRAY: u32 = WM_APP + 1;
const HK_PRIVACY: i32 = 1;
const HK_REPLAY: i32 = 2;
const CMD_OPEN: usize = 10;
const CMD_QUIT: usize = 11;

static CTX: OnceLock<eframe::egui::Context> = OnceLock::new();
static TRAY_HWND: AtomicIsize = AtomicIsize::new(0);
/// O usuário pediu para sair pelo menu da bandeja: a interface encerra tudo e fecha.
pub static QUIT: AtomicBool = AtomicBool::new(false);
/// Atalhos registrados (só enquanto transmite).
static HOTKEYS: AtomicBool = AtomicBool::new(false);

/// Cria o ícone da bandeja (uma vez). `ctx` acorda a interface quando algo acontece.
pub fn start(ctx: eframe::egui::Context) {
    if CTX.set(ctx).is_err() {
        return;
    }
    std::thread::Builder::new().name("tray".into()).spawn(run).ok();
}

/// A janela principal do GameStream (deste processo).
fn main_window() -> Option<HWND> {
    struct Find {
        pid: u32,
        found: Option<isize>,
    }
    unsafe extern "system" fn cb(h: HWND, l: LPARAM) -> BOOL {
        let f = &mut *(l.0 as *mut Find);
        let mut p = 0u32;
        GetWindowThreadProcessId(h, Some(&mut p));
        if p == f.pid {
            let mut buf = [0u16; 32];
            let n = GetWindowTextW(h, &mut buf) as usize;
            if String::from_utf16_lossy(&buf[..n]) == "GameStream" {
                f.found = Some(h.0 as isize);
                return BOOL(0);
            }
        }
        BOOL(1)
    }
    let mut f = Find { pid: std::process::id(), found: None };
    unsafe {
        let _ = EnumWindows(Some(cb), LPARAM(&mut f as *mut Find as isize));
    }
    f.found.map(|h| HWND(h as _))
}

/// Mostra (e traz para a frente) a janela principal.
pub fn show_main() {
    if let Some(h) = main_window() {
        unsafe {
            let _ = ShowWindow(h, if IsIconic(h).as_bool() { SW_RESTORE } else { SW_SHOW });
            let _ = SetForegroundWindow(h);
        }
    }
    if let Some(ctx) = CTX.get() {
        ctx.request_repaint();
    }
}

/// Esconde a janela principal (o app continua na bandeja).
pub fn hide_main() {
    if let Some(h) = main_window() {
        unsafe {
            let _ = ShowWindow(h, SW_HIDE);
        }
    }
}

/// O ícone da bandeja existe (a janela pode ser escondida sem "sumir" o app).
pub fn active() -> bool {
    TRAY_HWND.load(Ordering::Relaxed) != 0
}

fn wide(s: &str, out: &mut [u16]) {
    let v: Vec<u16> = s.encode_utf16().take(out.len() - 1).collect();
    out[..v.len()].copy_from_slice(&v);
    out[v.len()] = 0;
}

fn icon() -> HICON {
    unsafe {
        let inst = GetModuleHandleW(None).unwrap_or_default();
        let (cx, cy) = (GetSystemMetrics(SM_CXSMICON), GetSystemMetrics(SM_CYSMICON));
        match LoadImageW(inst, PCWSTR(1 as *const u16), IMAGE_ICON, cx, cy, LR_DEFAULTCOLOR) {
            Ok(h) => HICON(h.0),
            Err(_) => LoadIconW(None, IDI_APPLICATION).unwrap_or_default(),
        }
    }
}

fn notify_data(hwnd: HWND, tip: &str) -> NOTIFYICONDATAW {
    let mut nid = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: 1,
        uFlags: NIF_ICON | NIF_MESSAGE | NIF_TIP,
        uCallbackMessage: WM_TRAY,
        hIcon: icon(),
        ..Default::default()
    };
    wide(tip, &mut nid.szTip);
    nid
}

/// Texto ao passar o mouse no ícone: o que o GameStream está fazendo agora.
fn tooltip() -> String {
    let (hosting, viewers, privacy, rec) = crate::state::host(|h| (h.invite.is_some(), h.viewers.len(), h.privacy, h.recording.is_some()));
    let watching = crate::state::view(|v| v.phase == crate::state::ViewPhase::Connected);
    let mut s = String::from("GameStream");
    if hosting {
        s += &crate::tr!(" — transmitindo ({viewers} assistindo)", " — streaming ({viewers} watching)");
        if privacy {
            s += crate::i18n::t(" · privacidade");
        }
        if rec {
            s += crate::i18n::t(" · gravando");
        }
    } else if watching {
        s += crate::i18n::t(" — assistindo");
    }
    s
}

fn add_icon(hwnd: HWND) {
    let nid = notify_data(hwnd, &tooltip());
    unsafe {
        let _ = Shell_NotifyIconW(NIM_ADD, &nid);
    }
}

/// Registra os atalhos só enquanto transmite (não "rouba" Ctrl+F9/F10 dos outros programas à toa).
fn sync_hotkeys(hwnd: HWND) {
    let want = crate::state::host(|h| h.invite.is_some());
    if want == HOTKEYS.load(Ordering::Relaxed) {
        return;
    }
    HOTKEYS.store(want, Ordering::Relaxed);
    unsafe {
        if want {
            if RegisterHotKey(hwnd, HK_PRIVACY, MOD_CONTROL | MOD_NOREPEAT, VK_F9.0 as u32).is_err() {
                crate::state::log(crate::i18n::t("[atalho] Ctrl+F9 já é usado por outro programa; use o botão do modo privacidade."));
            }
            if RegisterHotKey(hwnd, HK_REPLAY, MOD_CONTROL | MOD_NOREPEAT, VK_F10.0 as u32).is_err() {
                crate::state::log(crate::i18n::t("[atalho] Ctrl+F10 já é usado por outro programa; use o botão de salvar replay."));
            }
        } else {
            let _ = UnregisterHotKey(hwnd, HK_PRIVACY);
            let _ = UnregisterHotKey(hwnd, HK_REPLAY);
        }
    }
}

fn menu(hwnd: HWND) {
    unsafe {
        let Ok(m) = CreatePopupMenu() else { return };
        let open: Vec<u16> = crate::i18n::t("Abrir GameStream").encode_utf16().chain([0]).collect();
        let quit_s: Vec<u16> = crate::i18n::t("Sair").encode_utf16().chain([0]).collect();
        let _ = AppendMenuW(m, MF_STRING, CMD_OPEN, PCWSTR(open.as_ptr()));
        let _ = AppendMenuW(m, MF_SEPARATOR, 0, PCWSTR::null());
        let _ = AppendMenuW(m, MF_STRING, CMD_QUIT, PCWSTR(quit_s.as_ptr()));
        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        // Necessário para o menu fechar ao clicar fora dele.
        let _ = SetForegroundWindow(hwnd);
        let cmd = TrackPopupMenu(m, TPM_RETURNCMD | TPM_RIGHTBUTTON | TPM_BOTTOMALIGN, pt.x, pt.y, 0, hwnd, None);
        let _ = DestroyMenu(m);
        match cmd.0 as usize {
            CMD_OPEN => show_main(),
            CMD_QUIT => quit(),
            _ => {}
        }
    }
}

/// Sair de verdade: a interface encerra a transmissão/sessão e fecha.
fn quit() {
    QUIT.store(true, Ordering::Relaxed);
    // A interface só processa quadros com a janela visível.
    show_main();
    if let Some(h) = main_window() {
        unsafe {
            let _ = PostMessageW(h, WM_CLOSE, WPARAM(0), LPARAM(0));
        }
    }
}

static TASKBAR_CREATED: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_TRAY => {
            match lp.0 as u32 {
                WM_LBUTTONUP | WM_LBUTTONDBLCLK => show_main(),
                WM_RBUTTONUP => menu(hwnd),
                _ => {}
            }
            LRESULT(0)
        }
        WM_HOTKEY => {
            match wp.0 as i32 {
                HK_PRIVACY => crate::host::toggle_privacy(),
                HK_REPLAY => crate::host::save_replay(),
                _ => {}
            }
            if let Some(ctx) = CTX.get() {
                ctx.request_repaint();
            }
            LRESULT(0)
        }
        WM_TIMER => {
            sync_hotkeys(hwnd);
            let nid = notify_data(hwnd, &tooltip());
            let _ = Shell_NotifyIconW(NIM_MODIFY, &nid);
            LRESULT(0)
        }
        // O Explorer reiniciou: o ícone precisa ser criado de novo.
        m if m != 0 && m == TASKBAR_CREATED.load(Ordering::Relaxed) => {
            add_icon(hwnd);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

fn run() {
    unsafe {
        let inst = GetModuleHandleW(None).unwrap_or_default();
        let class = w!("GameStreamTray");
        let wc = WNDCLASSW { lpfnWndProc: Some(wndproc), hInstance: inst.into(), lpszClassName: class, ..Default::default() };
        RegisterClassW(&wc);
        TASKBAR_CREATED.store(RegisterWindowMessageW(w!("TaskbarCreated")), Ordering::Relaxed);
        let Ok(hwnd) = CreateWindowExW(WINDOW_EX_STYLE(0), class, w!("GameStream tray"), WS_OVERLAPPED, 0, 0, 0, 0, None, None, inst, None) else {
            return;
        };
        TRAY_HWND.store(hwnd.0 as isize, Ordering::Relaxed);
        add_icon(hwnd);
        SetTimer(hwnd, 1, 1000, None);
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        let nid = notify_data(hwnd, "");
        let _ = Shell_NotifyIconW(NIM_DELETE, &nid);
    }
}

/// Remove o ícone ao sair (senão ele fica "fantasma" até passar o mouse).
pub fn remove() {
    let h = TRAY_HWND.swap(0, Ordering::Relaxed);
    if h != 0 {
        let nid = notify_data(HWND(h as _), "");
        unsafe {
            let _ = Shell_NotifyIconW(NIM_DELETE, &nid);
        }
    }
}
