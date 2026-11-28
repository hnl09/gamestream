//! "Seguir o jogo": transmitindo a janela de um launcher (cliente do League of Legends, Riot Client,
//! Steam, Battle.net...), quando ele abre o jogo a transmissão passa sozinha para a janela do jogo, e
//! volta para o launcher quando o jogo fecha — como o Discord faz com o LoL.
//!
//! Como reconhece o jogo: um processo NOVO, aberto pela mesma "família" do launcher (descendente do
//! processo que abriu o launcher), com uma janela grande e normal (não popup, não overlay), que fica
//! do mesmo tamanho por 2 s (a tela de carregamento do LoL já conta).

use std::collections::{HashMap, HashSet};

use crate::{procaudio::ProcInfo, sources};

/// Processos que não fazem parte da "família" de um jogo (subir até eles iria longe demais).
const STOP: &[&str] = &[
    "explorer.exe", "svchost.exe", "services.exe", "wininit.exe", "winlogon.exe", "sihost.exe", "runtimebroker.exe",
    "cmd.exe", "powershell.exe", "pwsh.exe", "windowsterminal.exe", "openconsole.exe", "conhost.exe", "gamestream.exe",
    "dllhost.exe", "taskhostw.exe", "userinit.exe", "applicationframehost.exe", "startmenuexperiencehost.exe", "smss.exe",
    "csrss.exe", "system",
];

/// Menor janela considerada "o jogo".
const MIN_W: i32 = 800;
const MIN_H: i32 = 450;

/// Processo mais alto da família de `pid` (sobe pelos pais até um processo do sistema).
pub fn family_root(pid: u32, procs: &[ProcInfo]) -> u32 {
    let map: HashMap<u32, &ProcInfo> = procs.iter().map(|p| (p.pid, p)).collect();
    let mut cur = pid;
    for _ in 0..8 {
        let Some(p) = map.get(&cur) else { break };
        let Some(parent) = map.get(&p.parent) else { break };
        if parent.pid == cur || parent.pid == 0 || STOP.contains(&parent.exe.to_ascii_lowercase().as_str()) {
            break;
        }
        cur = parent.pid;
    }
    cur
}

/// `root` e todos os processos abaixo dele.
pub fn descendants(root: u32, procs: &[ProcInfo]) -> HashSet<u32> {
    let mut set = HashSet::from([root]);
    loop {
        let before = set.len();
        for p in procs {
            if set.contains(&p.parent) && p.pid != p.parent {
                set.insert(p.pid);
            }
        }
        if set.len() == before {
            return set;
        }
    }
}

fn exe_of(pid: u32, procs: &[ProcInfo]) -> String {
    procs.iter().find(|p| p.pid == pid).map(|p| p.exe.clone()).unwrap_or_default()
}

/// O que fazer agora.
pub enum Action {
    None,
    /// Trocar a transmissão para esta janela (e a mensagem para o log).
    Switch(sources::Source, String),
    /// Mensagem para o log, sem trocar.
    Note(String),
}

#[derive(Default)]
pub struct State {
    /// Launcher que a pessoa escolheu transmitir: (janela, título, exe).
    launcher: Option<(isize, String, String)>,
    root: u32,
    /// Processos da família que já existiam (não são "o jogo").
    baseline: HashSet<u32>,
    /// Jogo sendo transmitido agora: (janela, processo).
    game: Option<(isize, u32)>,
    /// Janela candidata a jogo: (janela, tamanho, quantas vezes seguidas foi vista igual).
    cand: Option<(isize, (i32, i32), u32)>,
    waiting_since: Option<std::time::Instant>,
}

impl State {
    /// Começa a acompanhar a fonte atual (só janelas; monitor já mostra tudo).
    pub fn new(src: &sources::Source) -> State {
        let sources::Source::Window { hwnd, title } = src else { return State::default() };
        let Some(pid) = sources::window_pid(*hwnd) else { return State::default() };
        let procs = crate::procaudio::processes();
        let root = family_root(pid, &procs);
        State {
            launcher: Some((*hwnd, title.clone(), exe_of(pid, &procs))),
            root,
            baseline: descendants(root, &procs),
            ..Default::default()
        }
    }

    pub fn active(&self) -> bool {
        self.launcher.is_some()
    }

    /// Título do jogo sendo transmitido (para a interface).
    pub fn launcher_title(&self) -> Option<&str> {
        self.launcher.as_ref().map(|l| l.1.as_str())
    }

    /// Uma rodada (a cada ~1 s).
    pub fn step(&mut self) -> Action {
        let Some((l_hwnd, l_title, l_exe)) = self.launcher.clone() else { return Action::None };
        let procs = crate::procaudio::processes();
        match self.game {
            // A janela transmitida fechou (ex.: escolheram a janela da partida e ela acabou): vai para
            // a maior janela da mesma família (o cliente) e continua seguindo o jogo a partir dela.
            None if !sources::window_alive(l_hwnd) => {
                let family = descendants(self.root, &procs);
                let next = sources::windows()
                    .into_iter()
                    .filter(|w| sources::window_pid(w.hwnd).is_some_and(|p| family.contains(&p)) && sources::is_plain_window(w.hwnd))
                    .max_by_key(|w| sources::client_size(w.hwnd).map(|(a, b)| a * b).unwrap_or(0));
                match next {
                    Some(w) => {
                        let src = sources::Source::Window { hwnd: w.hwnd, title: w.title.clone() };
                        let msg = crate::tr!(
                            "[seguir o jogo] \"{l_title}\" fechou: transmitindo \"{}\" (e seguindo o próximo jogo).",
                            "[follow game] \"{l_title}\" closed: streaming \"{}\" (and following the next game).",
                            w.title
                        );
                        let root = self.root;
                        *self = State::new(&src);
                        if !self.active() {
                            self.root = root;
                        }
                        Action::Switch(src, msg)
                    }
                    None => {
                        if self.waiting_since.is_none() {
                            self.waiting_since = Some(std::time::Instant::now());
                            Action::Note(crate::tr!(
                                "[seguir o jogo] \"{l_title}\" fechou; esperando uma janela do launcher aparecer...",
                                "[follow game] \"{l_title}\" closed; waiting for a launcher window to show up..."
                            ))
                        } else {
                            Action::None
                        }
                    }
                }
            }
            None => {
                let family = descendants(self.root, &procs);
                let best = sources::windows()
                    .into_iter()
                    .filter_map(|w| {
                        let pid = sources::window_pid(w.hwnd)?;
                        if !family.contains(&pid) || self.baseline.contains(&pid) || !sources::is_plain_window(w.hwnd) {
                            return None;
                        }
                        let (cw, ch) = sources::client_size(w.hwnd)?;
                        (cw >= MIN_W && ch >= MIN_H).then_some((w, pid, (cw, ch)))
                    })
                    .max_by_key(|(_, _, (cw, ch))| cw * ch);
                let Some((w, pid, size)) = best else {
                    self.cand = None;
                    return Action::None;
                };
                let seen = match self.cand {
                    Some((h, s, n)) if h == w.hwnd && s == size => n + 1,
                    _ => 1,
                };
                self.cand = Some((w.hwnd, size, seen));
                if seen < 2 {
                    return Action::None;
                }
                self.cand = None;
                self.game = Some((w.hwnd, pid));
                let msg = crate::tr!(
                    "[seguir o jogo] \"{l_title}\" abriu \"{}\": transmitindo o jogo.",
                    "[follow game] \"{l_title}\" opened \"{}\": streaming the game.",
                    w.title
                );
                Action::Switch(sources::Source::Window { hwnd: w.hwnd, title: w.title }, msg)
            }
            Some((g_hwnd, g_pid)) => {
                let alive = sources::window_alive(g_hwnd) && procs.iter().any(|p| p.pid == g_pid);
                if alive {
                    self.waiting_since = None;
                    return Action::None;
                }
                // O jogo fechou: volta para o launcher (a mesma janela, ou uma nova do mesmo programa
                // se o cliente foi fechado durante a partida e abriu de novo).
                let back = if sources::window_shown(l_hwnd) {
                    Some(sources::WindowInfo { hwnd: l_hwnd, title: l_title.clone(), minimized: false, note: String::new() })
                } else {
                    sources::windows()
                        .into_iter()
                        .filter(|w| sources::window_pid(w.hwnd).is_some_and(|p| exe_of(p, &procs).eq_ignore_ascii_case(&l_exe)))
                        .max_by_key(|w| sources::client_size(w.hwnd).map(|(a, b)| a * b).unwrap_or(0))
                };
                match back {
                    Some(w) => {
                        let src = sources::Source::Window { hwnd: w.hwnd, title: w.title.clone() };
                        let msg = crate::tr!(
                            "[seguir o jogo] o jogo fechou: voltando para \"{}\".",
                            "[follow game] the game closed: back to \"{}\".",
                            w.title
                        );
                        *self = State::new(&src);
                        Action::Switch(src, msg)
                    }
                    None => {
                        let first = self.waiting_since.is_none();
                        let since = *self.waiting_since.get_or_insert_with(std::time::Instant::now);
                        if since.elapsed() > std::time::Duration::from_secs(180) {
                            *self = State::default();
                            return Action::Note(crate::i18n::t("[seguir o jogo] o launcher não voltou; escolha a fonte de novo se quiser.").into());
                        }
                        if first {
                            Action::Note(crate::tr!(
                                "[seguir o jogo] o jogo fechou; esperando \"{l_title}\" aparecer de novo...",
                                "[follow game] the game closed; waiting for \"{l_title}\" to show up again..."
                            ))
                        } else {
                            Action::None
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(pid: u32, parent: u32, exe: &str) -> ProcInfo {
        ProcInfo { pid, parent, exe: exe.into() }
    }

    #[test]
    fn league_family() {
        // explorer -> RiotClientServices -> LeagueClient -> LeagueClientUx (janela do cliente)
        //                                               -> League of Legends (o jogo)
        let procs = vec![
            p(10, 1, "explorer.exe"),
            p(20, 10, "RiotClientServices.exe"),
            p(30, 20, "LeagueClient.exe"),
            p(40, 30, "LeagueClientUx.exe"),
            p(41, 40, "LeagueClientUxRender.exe"),
            p(50, 30, "League of Legends.exe"),
            p(60, 10, "Discord.exe"),
        ];
        let root = family_root(40, &procs);
        assert_eq!(root, 20);
        let fam = descendants(root, &procs);
        assert!(fam.contains(&50) && fam.contains(&41));
        assert!(!fam.contains(&60) && !fam.contains(&10));
    }

    #[test]
    fn stops_at_system_processes() {
        let procs = vec![p(10, 1, "explorer.exe"), p(20, 10, "steam.exe"), p(30, 20, "game.exe"), p(5, 4, "cmd.exe"), p(6, 5, "tool.exe")];
        assert_eq!(family_root(30, &procs), 20);
        assert_eq!(family_root(6, &procs), 6);
        // Pai que já morreu: para no próprio processo.
        assert_eq!(family_root(99, &[p(99, 12345, "x.exe")]), 99);
    }
}
