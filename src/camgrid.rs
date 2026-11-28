//! Grade invisível das janelinhas de câmera.
//!
//! A área de trabalho de cada monitor é dividida em células (80×60 px a 100% de escala). Cada câmera
//! ocupa um bloco quadrado de k×k células (4:3). A pessoa arrasta a janelinha livremente; ao soltar
//! o mouse ela encaixa no bloco livre mais próximo, sem nunca ficar por cima de outra câmera. A roda
//! do mouse sobre a câmera aumenta ou diminui o bloco (uma célula por vez). A grade não aparece.

use std::{
    sync::{LazyLock, Mutex, OnceLock},
    time::Duration,
};

/// Tamanho do bloco (em células) de uma câmera nova, e os limites da roda do mouse.
pub const DEFAULT_K: i32 = 4;
pub const MIN_K: i32 = 2;
pub const MAX_K: i32 = 8;

/// Posição de uma câmera na grade: canto superior esquerdo (coluna, linha) e lado do bloco.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pos {
    pub col: i32,
    pub row: i32,
    pub k: i32,
}

impl Pos {
    fn overlaps(&self, o: &Pos) -> bool {
        self.col < o.col + o.k && o.col < self.col + self.k && self.row < o.row + o.k && o.row < self.row + self.k
    }
}

/// Bloco livre (dentro da grade, sem sobrepor `others`) mais perto de `want`.
/// Empate: o mais à direita e depois o mais acima (as câmeras empilham no canto superior direito).
pub fn nearest_free(want_col: i32, want_row: i32, k: i32, others: &[Pos], cols: i32, rows: i32) -> Option<Pos> {
    let mut best: Option<(i64, Pos)> = None;
    for row in 0..=(rows - k) {
        for col in 0..=(cols - k) {
            let p = Pos { col, row, k };
            if others.iter().any(|o| p.overlaps(o)) {
                continue;
            }
            let (dc, dr) = ((col - want_col) as i64, (row - want_row) as i64);
            let d = dc * dc + dr * dr;
            let better = match &best {
                None => true,
                Some((bd, bp)) => d < *bd || (d == *bd && (col > bp.col || (col == bp.col && row < bp.row))),
            };
            if better {
                best = Some((d, p));
            }
        }
    }
    best.map(|(_, p)| p)
}

/// Onde entra uma câmera nova: canto superior direito, empilhando para baixo.
pub fn initial(k: i32, others: &[Pos], cols: i32, rows: i32) -> Option<Pos> {
    for col in (0..=(cols - k)).rev() {
        for row in 0..=(rows - k) {
            let p = Pos { col, row, k };
            if !others.iter().any(|o| p.overlaps(o)) {
                return Some(p);
            }
        }
    }
    None
}

/// Novo bloco ao aumentar/diminuir: mantém o centro e, se não couber, vai para o livre mais perto.
pub fn resized(p: Pos, delta: i32, others: &[Pos], cols: i32, rows: i32) -> Option<Pos> {
    let k = (p.k + delta).clamp(MIN_K, MAX_K.min(cols).min(rows));
    if k == p.k {
        return None;
    }
    let shift = (p.k - k) / 2;
    nearest_free(p.col + shift, p.row + shift, k, others, cols, rows)
}

/// Grade de um monitor, em pixels físicos.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Grid {
    ox: i32,
    oy: i32,
    cw: i32,
    ch: i32,
    cols: i32,
    rows: i32,
    gap: i32,
}

impl Grid {
    fn rect(&self, p: Pos) -> (i32, i32, i32, i32) {
        (
            self.ox + p.col * self.cw + self.gap / 2,
            self.oy + p.row * self.ch + self.gap / 2,
            p.k * self.cw - self.gap,
            p.k * self.ch - self.gap,
        )
    }

    /// Célula mais perto do canto superior esquerdo de uma janela em (x, y).
    fn cell_at(&self, x: i32, y: i32) -> (i32, i32) {
        let c = ((x - self.gap / 2 - self.ox) as f32 / self.cw as f32).round() as i32;
        let r = ((y - self.gap / 2 - self.oy) as f32 / self.ch as f32).round() as i32;
        (c, r)
    }
}

struct Entry {
    id: u64,
    pid: Option<u32>,
    hwnd: isize,
    mon: isize,
    pos: Pos,
    /// Retângulo que aplicamos por último (x, y, w, h).
    applied: Option<(i32, i32, i32, i32)>,
    resize: i32,
}

static REG: LazyLock<Mutex<(u64, Vec<Entry>)>> = LazyLock::new(|| Mutex::new((0, Vec::new())));

fn others(list: &[Entry], id: u64, mon: isize) -> Vec<Pos> {
    list.iter().filter(|e| e.id != id && e.mon == mon).map(|e| e.pos).collect()
}

/// Reserva um lugar para uma câmera nova. Devolve o id e a geometria inicial para o mpv
/// ("LxA+X+Y"), ou None se não for possível descobrir o monitor.
pub fn reserve() -> (u64, Option<String>) {
    start_thread();
    let mon = win::primary_monitor();
    let grid = win::grid(mon);
    let mut reg = REG.lock().unwrap();
    reg.0 += 1;
    let id = reg.0;
    let pos = grid
        .and_then(|g| initial(DEFAULT_K, &others(&reg.1, id, mon), g.cols, g.rows))
        .unwrap_or(Pos { col: 0, row: 0, k: DEFAULT_K });
    let geometry = grid.map(|g| {
        let (x, y, w, h) = g.rect(pos);
        format!("{w}x{h}+{x}+{y}")
    });
    reg.1.push(Entry { id, pid: None, hwnd: 0, mon, pos, applied: None, resize: 0 });
    (id, geometry)
}

/// O processo do mpv da câmera `id` já existe: a janela dele será encontrada e posicionada.
pub fn attach(id: u64, pid: u32) {
    if let Some(e) = REG.lock().unwrap().1.iter_mut().find(|e| e.id == id) {
        e.pid = Some(pid);
    }
}

/// Roda do mouse sobre a câmera: +1 aumenta, -1 diminui.
pub fn resize(id: u64, delta: i32) {
    if let Some(e) = REG.lock().unwrap().1.iter_mut().find(|e| e.id == id) {
        e.resize += delta.signum();
    }
}

/// A janela fechou: libera o lugar.
pub fn release(id: u64) {
    REG.lock().unwrap().1.retain(|e| e.id != id);
}

fn start_thread() {
    static STARTED: OnceLock<()> = OnceLock::new();
    STARTED.get_or_init(|| {
        std::thread::Builder::new().name("camgrid".into()).spawn(manager).ok();
    });
}

/// Acompanha as janelas: encontra as novas, encaixa as arrastadas e aplica os redimensionamentos.
fn manager() {
    win::dpi_aware_thread();
    loop {
        std::thread::sleep(Duration::from_millis(100));
        let mouse_down = win::mouse_down();
        let mut moves: Vec<(isize, (i32, i32, i32, i32))> = Vec::new();
        {
            let mut reg = REG.lock().unwrap();
            let list = &mut reg.1;
            for i in 0..list.len() {
                if list[i].hwnd == 0 {
                    let Some(pid) = list[i].pid else { continue };
                    let Some(h) = win::find_mpv_window(pid) else { continue };
                    list[i].hwnd = h;
                }
                let (id, hwnd) = (list[i].id, list[i].hwnd);
                let Some(actual) = win::window_rect(hwnd) else { continue };
                let mut target: Option<(isize, Pos)> = None;

                if list[i].resize != 0 && !mouse_down {
                    let d = std::mem::take(&mut list[i].resize);
                    let mon = list[i].mon;
                    if let Some(g) = win::grid(mon) {
                        if let Some(p) = resized(list[i].pos, d, &others(list, id, mon), g.cols, g.rows) {
                            target = Some((mon, p));
                        }
                    }
                } else if list[i].applied.is_none() {
                    target = Some((list[i].mon, list[i].pos));
                } else if Some(actual) != list[i].applied && !mouse_down && !win::is_fullscreen(hwnd, actual) {
                    let applied = list[i].applied.unwrap();
                    if (actual.2, actual.3) != (applied.2, applied.3) {
                        // O próprio mpv mudou o tamanho (ex.: chegou o primeiro quadro): volta ao bloco.
                        target = Some((list[i].mon, list[i].pos));
                    } else {
                        // A pessoa arrastou e soltou: encaixa no bloco livre mais perto.
                        let mon = win::monitor_of(actual);
                        if let Some(g) = win::grid(mon) {
                            let (c, r) = g.cell_at(actual.0, actual.1);
                            let k = list[i].pos.k.min(g.cols).min(g.rows);
                            let p = nearest_free(c, r, k, &others(list, id, mon), g.cols, g.rows);
                            target = Some(match p {
                                Some(p) => (mon, p),
                                None => (list[i].mon, list[i].pos), // tela cheia de câmeras: volta
                            });
                        }
                    }
                }

                if let Some((mon, p)) = target {
                    if let Some(g) = win::grid(mon) {
                        let r = g.rect(p);
                        list[i].mon = mon;
                        list[i].pos = p;
                        list[i].applied = Some(r);
                        if r != actual {
                            moves.push((hwnd, r));
                        }
                    }
                }
            }
        }
        for (h, r) in moves {
            win::move_window(h, r);
        }
    }
}

mod win {
    use super::Grid;
    use windows::Win32::{
        Foundation::{BOOL, HWND, LPARAM, POINT, RECT},
        Graphics::Gdi::{GetMonitorInfoW, MonitorFromPoint, MonitorFromRect, HMONITOR, MONITORINFO, MONITOR_DEFAULTTONEAREST, MONITOR_DEFAULTTOPRIMARY},
        UI::{
            HiDpi::{GetDpiForMonitor, SetThreadDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, MDT_EFFECTIVE_DPI},
            Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON, VK_RBUTTON},
            WindowsAndMessaging::{
                EnumWindows, GetClassNameW, GetWindowRect, GetWindowThreadProcessId, IsWindow, IsWindowVisible, SetWindowPos, SWP_NOACTIVATE,
                SWP_NOOWNERZORDER, SWP_NOZORDER,
            },
        },
    };

    pub fn dpi_aware_thread() {
        unsafe {
            let _ = SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        }
    }

    pub fn mouse_down() -> bool {
        unsafe { (GetAsyncKeyState(VK_LBUTTON.0 as i32) as u16 & 0x8000) != 0 || (GetAsyncKeyState(VK_RBUTTON.0 as i32) as u16 & 0x8000) != 0 }
    }

    pub fn primary_monitor() -> isize {
        unsafe { MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY).0 as isize }
    }

    pub fn monitor_of(r: (i32, i32, i32, i32)) -> isize {
        let rc = RECT { left: r.0, top: r.1, right: r.0 + r.2, bottom: r.1 + r.3 };
        unsafe { MonitorFromRect(&rc, MONITOR_DEFAULTTONEAREST).0 as isize }
    }

    fn info(mon: isize) -> Option<MONITORINFO> {
        let mut mi = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        unsafe { GetMonitorInfoW(HMONITOR(mon as _), &mut mi).as_bool().then_some(mi) }
    }

    pub fn grid(mon: isize) -> Option<Grid> {
        let wa = info(mon)?.rcWork;
        let (mut dx, mut dy) = (96u32, 96u32);
        let scale = unsafe {
            if GetDpiForMonitor(HMONITOR(mon as _), MDT_EFFECTIVE_DPI, &mut dx, &mut dy).is_ok() {
                dx as f32 / 96.0
            } else {
                1.0
            }
        };
        let px = |v: f32| (v * scale).round() as i32;
        let (cw, ch, margin, gap) = (px(80.0), px(60.0), px(12.0), px(8.0));
        let cols = (wa.right - wa.left - 2 * margin) / cw;
        let rows = (wa.bottom - wa.top - 2 * margin) / ch;
        (cols >= super::MIN_K && rows >= super::MIN_K).then_some(Grid { ox: wa.left + margin, oy: wa.top + margin, cw, ch, cols, rows, gap })
    }

    pub fn is_fullscreen(_hwnd: isize, r: (i32, i32, i32, i32)) -> bool {
        let Some(mi) = info(monitor_of(r)) else { return false };
        let m = mi.rcMonitor;
        r.2 >= m.right - m.left && r.3 >= m.bottom - m.top
    }

    pub fn window_rect(h: isize) -> Option<(i32, i32, i32, i32)> {
        let hwnd = HWND(h as _);
        unsafe {
            if !IsWindow(hwnd).as_bool() {
                return None;
            }
            let mut r = RECT::default();
            GetWindowRect(hwnd, &mut r).ok()?;
            Some((r.left, r.top, r.right - r.left, r.bottom - r.top))
        }
    }

    pub fn move_window(h: isize, r: (i32, i32, i32, i32)) {
        unsafe {
            let _ = SetWindowPos(HWND(h as _), None, r.0, r.1, r.2, r.3, SWP_NOZORDER | SWP_NOACTIVATE | SWP_NOOWNERZORDER);
        }
    }

    /// Janela visível de classe "mpv" do processo `pid`.
    pub fn find_mpv_window(pid: u32) -> Option<isize> {
        struct Find {
            pid: u32,
            found: Option<isize>,
        }
        unsafe extern "system" fn cb(h: HWND, l: LPARAM) -> BOOL {
            let f = &mut *(l.0 as *mut Find);
            let mut p = 0u32;
            GetWindowThreadProcessId(h, Some(&mut p));
            if p == f.pid && IsWindowVisible(h).as_bool() {
                let mut buf = [0u16; 16];
                let n = GetClassNameW(h, &mut buf) as usize;
                if String::from_utf16_lossy(&buf[..n]) == "mpv" {
                    f.found = Some(h.0 as isize);
                    return BOOL(0);
                }
            }
            BOOL(1)
        }
        let mut f = Find { pid, found: None };
        unsafe {
            let _ = EnumWindows(Some(cb), LPARAM(&mut f as *mut Find as isize));
        }
        f.found
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stacks_top_right_downwards() {
        let a = initial(4, &[], 20, 12).unwrap();
        assert_eq!(a, Pos { col: 16, row: 0, k: 4 });
        let b = initial(4, &[a], 20, 12).unwrap();
        assert_eq!(b, Pos { col: 16, row: 4, k: 4 });
        let c = initial(4, &[a, b], 20, 12).unwrap();
        assert_eq!(c, Pos { col: 16, row: 8, k: 4 });
    }

    #[test]
    fn drop_never_overlaps() {
        let a = Pos { col: 16, row: 0, k: 4 };
        // Soltou em cima da outra: vai para o lado livre mais perto.
        let p = nearest_free(15, 1, 4, &[a], 20, 12).unwrap();
        assert!(!p.overlaps(&a));
        assert_eq!(p, Pos { col: 15, row: 4, k: 4 });
        // Soltou fora da tela: fica dentro da grade.
        let q = nearest_free(40, -5, 4, &[], 20, 12).unwrap();
        assert_eq!(q, Pos { col: 16, row: 0, k: 4 });
    }

    #[test]
    fn resize_keeps_center_and_limits() {
        let p = Pos { col: 8, row: 4, k: 4 };
        assert_eq!(resized(p, 1, &[], 20, 12), Some(Pos { col: 8, row: 4, k: 5 }));
        assert_eq!(resized(p, 2, &[], 20, 12), Some(Pos { col: 7, row: 3, k: 6 }));
        assert_eq!(resized(Pos { col: 0, row: 0, k: MIN_K }, -1, &[], 20, 12), None);
        // Crescer encostado em outra câmera: desloca para não sobrepor.
        let other = Pos { col: 12, row: 4, k: 4 };
        let r = resized(p, 2, &[other], 20, 12).unwrap();
        assert_eq!(r.k, 6);
        assert!(!r.overlaps(&other));
        // Sem espaço nenhum: não muda.
        let full = [Pos { col: 0, row: 0, k: 4 }, Pos { col: 4, row: 0, k: 4 }];
        assert_eq!(resized(Pos { col: 0, row: 0, k: 4 }, 1, &full[1..], 8, 4), None);
    }

    #[test]
    fn grid_snaps_to_cells() {
        let g = Grid { ox: 12, oy: 12, cw: 80, ch: 60, cols: 20, rows: 12, gap: 8 };
        let (x, y, w, h) = g.rect(Pos { col: 2, row: 3, k: 4 });
        assert_eq!((x, y, w, h), (176, 196, 312, 232));
        assert_eq!(g.cell_at(x, y), (2, 3));
        assert_eq!(g.cell_at(x + 30, y - 25), (2, 3));
        assert_eq!(g.cell_at(x + 45, y + 35), (3, 4));
    }
}
