//! Fontes de captura: monitores (saídas DXGI, mesma numeração do ddagrab) e janelas.
//! Janela = recorte da área da janela no monitor onde ela está (continua zero-copy na GPU).

use std::ffi::c_void;

use anyhow::{bail, Context, Result};
use windows::Win32::{
    Foundation::{BOOL, HWND, LPARAM, POINT, RECT, TRUE},
    Graphics::{
        Dwm::{DwmGetWindowAttribute, DWMWA_CLOAKED},
        Dxgi::{CreateDXGIFactory1, IDXGIFactory1},
        Gdi::ClientToScreen,
    },
    UI::WindowsAndMessaging::{
        EnumWindows, GetClientRect, GetWindowLongW, GetWindowTextW, IsIconic, IsWindow, IsWindowVisible,
        GWL_EXSTYLE, WS_EX_TOOLWINDOW,
    },
};

#[derive(Clone, Debug, PartialEq)]
pub enum Source {
    Monitor(u32),
    Window { hwnd: isize, title: String },
}

#[derive(Clone, Debug)]
pub struct MonitorInfo {
    pub idx: u32,
    pub name: String,
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl MonitorInfo {
    pub fn label(&self) -> String {
        let primary = if self.x == 0 && self.y == 0 { " (principal)" } else { "" };
        format!("Monitor {} — {}x{}{}", self.idx + 1, self.w, self.h, primary)
    }
}

#[derive(Clone, Debug)]
pub struct WindowInfo {
    pub hwnd: isize,
    pub title: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Capture {
    pub output_idx: u32,
    /// (offset_x, offset_y, largura, altura) dentro do monitor.
    pub crop: Option<(i32, i32, i32, i32)>,
    /// Captura nativa da janela (Windows Graphics Capture): segue a janela e funciona mesmo coberta.
    pub hwnd: Option<isize>,
}

/// Processo dono da janela (para capturar só o som dele).
pub fn window_pid(hwnd: isize) -> Option<u32> {
    let mut pid = 0u32;
    unsafe {
        windows::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId(HWND(hwnd as *mut c_void), Some(&mut pid));
    }
    (pid != 0).then_some(pid)
}

/// Monitores do adaptador principal, na mesma ordem que o ddagrab (`output_idx`).
pub fn monitors() -> Vec<MonitorInfo> {
    let mut out = Vec::new();
    unsafe {
        let Ok(f) = CreateDXGIFactory1::<IDXGIFactory1>() else { return out };
        let Ok(ad) = f.EnumAdapters1(0) else { return out };
        let mut i = 0;
        while let Ok(o) = ad.EnumOutputs(i) {
            if let Ok(d) = o.GetDesc() {
                let r = d.DesktopCoordinates;
                let name = String::from_utf16_lossy(&d.DeviceName).trim_end_matches('\0').to_string();
                out.push(MonitorInfo { idx: i, name, x: r.left, y: r.top, w: r.right - r.left, h: r.bottom - r.top });
            }
            i += 1;
        }
    }
    out
}

unsafe extern "system" fn enum_cb(hwnd: HWND, lp: LPARAM) -> BOOL {
    let v = &mut *(lp.0 as *mut Vec<WindowInfo>);
    if !IsWindowVisible(hwnd).as_bool() || IsIconic(hwnd).as_bool() {
        return TRUE;
    }
    if (GetWindowLongW(hwnd, GWL_EXSTYLE) as u32) & WS_EX_TOOLWINDOW.0 != 0 {
        return TRUE;
    }
    let mut cloaked = 0u32;
    let _ = DwmGetWindowAttribute(hwnd, DWMWA_CLOAKED, &mut cloaked as *mut u32 as *mut c_void, 4);
    if cloaked != 0 {
        return TRUE;
    }
    let mut buf = [0u16; 256];
    let n = GetWindowTextW(hwnd, &mut buf);
    if n <= 0 {
        return TRUE;
    }
    let mut r = RECT::default();
    if GetClientRect(hwnd, &mut r).is_err() || r.right - r.left < 200 || r.bottom - r.top < 150 {
        return TRUE;
    }
    let title = String::from_utf16_lossy(&buf[..n as usize]);
    if title == "GameStream" || title == "Program Manager" {
        return TRUE;
    }
    v.push(WindowInfo { hwnd: hwnd.0 as isize, title });
    TRUE
}

/// Janelas visíveis (jogos, apps) que podem ser transmitidas.
pub fn windows() -> Vec<WindowInfo> {
    let mut v: Vec<WindowInfo> = Vec::new();
    unsafe {
        let _ = EnumWindows(Some(enum_cb), LPARAM(&mut v as *mut Vec<WindowInfo> as isize));
    }
    v
}

pub fn resolve(src: &Source) -> Result<Capture> {
    match src {
        Source::Monitor(i) => Ok(Capture { output_idx: *i, crop: None, hwnd: None }),
        Source::Window { hwnd, title } => unsafe {
            let h = HWND(*hwnd as *mut c_void);
            if !IsWindow(h).as_bool() {
                bail!("a janela \"{title}\" não existe mais");
            }
            if IsIconic(h).as_bool() {
                bail!("a janela \"{title}\" está minimizada");
            }
            let mut r = RECT::default();
            GetClientRect(h, &mut r)?;
            let mut p = POINT { x: 0, y: 0 };
            let _ = ClientToScreen(h, &mut p);
            let (w, hh) = (r.right - r.left, r.bottom - r.top);
            let (cx, cy) = (p.x + w / 2, p.y + hh / 2);
            let mons = monitors();
            let m = mons
                .iter()
                .find(|m| cx >= m.x && cx < m.x + m.w && cy >= m.y && cy < m.y + m.h)
                .context("a janela está fora dos monitores do adaptador principal")?;
            let ox = (p.x - m.x).max(0);
            let oy = (p.y - m.y).max(0);
            let cw = w.min(m.w - ox) & !1; // HEVC precisa de dimensões pares
            let ch = hh.min(m.h - oy) & !1;
            if cw < 64 || ch < 64 {
                bail!("a janela \"{title}\" é pequena demais");
            }
            Ok(Capture { output_idx: m.idx, crop: Some((ox, oy, cw, ch)), hwnd: None })
        },
    }
}

/// Acha uma janela pelo título (parte do nome, sem diferenciar maiúsculas). Usado na linha de comando.
pub fn find_window(part: &str) -> Option<WindowInfo> {
    let p = part.to_lowercase();
    windows().into_iter().find(|w| w.title.to_lowercase().contains(&p))
}
