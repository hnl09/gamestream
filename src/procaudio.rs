//! Captura de áudio por processo (Windows 10 2004+ / Windows 11): "process loopback" do WASAPI.
//! Permite gravar só o som de um programa (e dos processos filhos), ou tudo MENOS um programa
//! (ex.: todo o áudio exceto o Discord, evitando o retorno de voz para participantes da chamada).

use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Mutex,
    },
    time::Duration,
};

use anyhow::{bail, Context, Result};
use windows::{
    core::{implement, Interface, IUnknown, HRESULT},
    Win32::{
        Foundation::{CloseHandle, WAIT_OBJECT_0},
        Media::Audio::*,
        System::{
            Com::{CoCreateInstance, CoInitializeEx, CoUninitialize, IAgileObject, CLSCTX_ALL, COINIT_MULTITHREADED},
            Diagnostics::ToolHelp::{
                CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
            },
            Threading::{CreateEventW, WaitForSingleObject},
        },
    },
};

pub const RATE: u32 = 48_000;
pub const CHANNELS: u16 = 2;

// ---------- processos ----------

#[derive(Clone, Debug)]
pub struct ProcInfo {
    pub pid: u32,
    pub parent: u32,
    pub exe: String,
}

pub fn processes() -> Vec<ProcInfo> {
    let mut out = Vec::new();
    unsafe {
        let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else { return out };
        let mut e = PROCESSENTRY32W { dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        let mut ok = Process32FirstW(snap, &mut e).is_ok();
        while ok {
            let len = e.szExeFile.iter().position(|&c| c == 0).unwrap_or(e.szExeFile.len());
            out.push(ProcInfo {
                pid: e.th32ProcessID,
                parent: e.th32ParentProcessID,
                exe: String::from_utf16_lossy(&e.szExeFile[..len]),
            });
            ok = Process32NextW(snap, &mut e).is_ok();
        }
        let _ = CloseHandle(snap);
    }
    out
}

/// Processo "raiz" do Discord (os outros processos do Discord são filhos dele).
pub fn discord_root_pid() -> Option<u32> {
    let procs = processes();
    let is_discord = |exe: &str| {
        let e = exe.to_ascii_lowercase();
        e == "discord.exe" || e == "discordptb.exe" || e == "discordcanary.exe"
    };
    let pids: HashMap<u32, &ProcInfo> = procs.iter().map(|p| (p.pid, p)).collect();
    procs
        .iter()
        .filter(|p| is_discord(&p.exe))
        .find(|p| !pids.get(&p.parent).is_some_and(|parent| is_discord(&parent.exe)))
        .map(|p| p.pid)
}

/// Programas que estão com sessão de áudio aberta agora (os que aparecem no mixer de volume).
pub fn audio_apps() -> Vec<(u32, String)> {
    let names: HashMap<u32, String> = processes().into_iter().map(|p| (p.pid, p.exe)).collect();
    let me = std::process::id();
    let mut out: Vec<(u32, String)> = Vec::new();
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let r = (|| -> windows::core::Result<()> {
            let en: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
            let devs = en.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)?;
            for d in 0..devs.GetCount()? {
                let dev = devs.Item(d)?;
                let Ok(mgr) = dev.Activate::<IAudioSessionManager2>(CLSCTX_ALL, None) else { continue };
                let sessions = mgr.GetSessionEnumerator()?;
                for i in 0..sessions.GetCount()? {
                    let Ok(s) = sessions.GetSession(i) else { continue };
                    let Ok(s2) = s.cast::<IAudioSessionControl2>() else { continue };
                    let Ok(pid) = s2.GetProcessId() else { continue };
                    if pid == 0 || pid == me {
                        continue;
                    }
                    let name = names.get(&pid).cloned().unwrap_or_else(|| crate::tr!("processo {pid}", "process {pid}"));
                    if !out.iter().any(|(_, n)| n.eq_ignore_ascii_case(&name)) {
                        out.push((pid, name));
                    }
                }
            }
            Ok(())
        })();
        let _ = r;
    }
    out.sort_by(|a, b| a.1.to_lowercase().cmp(&b.1.to_lowercase()));
    out
}

/// Acha o pid atual de um programa pelo nome do .exe (o pid salvo pode ter mudado).
pub fn pid_by_exe(exe: &str) -> Option<u32> {
    audio_apps()
        .into_iter()
        .find(|(_, n)| n.eq_ignore_ascii_case(exe))
        .map(|(p, _)| p)
        .or_else(|| processes().into_iter().find(|p| p.exe.eq_ignore_ascii_case(exe)).map(|p| p.pid))
}

// ---------- captura ----------

#[implement(IActivateAudioInterfaceCompletionHandler, IAgileObject)]
struct Completion(Mutex<Option<mpsc::Sender<()>>>);

// Objeto "ágil": o Windows pode chamar o callback de qualquer thread.
impl windows::Win32::System::Com::IAgileObject_Impl for Completion_Impl {}

impl IActivateAudioInterfaceCompletionHandler_Impl for Completion_Impl {
    fn ActivateCompleted(&self, _op: Option<&IActivateAudioInterfaceAsyncOperation>) -> windows::core::Result<()> {
        if let Some(tx) = self.0.lock().unwrap().take() {
            let _ = tx.send(());
        }
        Ok(())
    }
}

/// Mesmo layout de um PROPVARIANT com VT_BLOB (x64): vt + 3 reservados, depois BLOB { cb, ptr }.
#[repr(C)]
struct BlobPropVariant {
    vt: u16,
    r1: u16,
    r2: u16,
    r3: u16,
    cb: u32,
    data: *const u8,
}

const VT_BLOB: u16 = 65;

unsafe fn activate(pid: u32, include: bool) -> Result<IAudioClient> {
    let params = AUDIOCLIENT_ACTIVATION_PARAMS {
        ActivationType: AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK,
        Anonymous: AUDIOCLIENT_ACTIVATION_PARAMS_0 {
            ProcessLoopbackParams: AUDIOCLIENT_PROCESS_LOOPBACK_PARAMS {
                TargetProcessId: pid,
                ProcessLoopbackMode: if include {
                    PROCESS_LOOPBACK_MODE_INCLUDE_TARGET_PROCESS_TREE
                } else {
                    PROCESS_LOOPBACK_MODE_EXCLUDE_TARGET_PROCESS_TREE
                },
            },
        },
    };
    let pv = BlobPropVariant {
        vt: VT_BLOB,
        r1: 0,
        r2: 0,
        r3: 0,
        cb: std::mem::size_of::<AUDIOCLIENT_ACTIVATION_PARAMS>() as u32,
        data: &params as *const _ as *const u8,
    };
    let (tx, rx) = mpsc::channel();
    let handler: IActivateAudioInterfaceCompletionHandler = Completion(Mutex::new(Some(tx))).into();
    let op = ActivateAudioInterfaceAsync(
        VIRTUAL_AUDIO_DEVICE_PROCESS_LOOPBACK,
        &IAudioClient::IID,
        Some(&pv as *const BlobPropVariant as *const _),
        &handler,
    )?;
    rx.recv_timeout(Duration::from_secs(5)).context(crate::i18n::t("o Windows não respondeu à solicitação de captura por aplicativo"))?;
    let mut hr = HRESULT(0);
    let mut unk: Option<IUnknown> = None;
    op.GetActivateResult(&mut hr, &mut unk)?;
    hr.ok().context(crate::i18n::t("captura por aplicativo não suportada (requer Windows 10 versão 2004 ou superior)"))?;
    Ok(unk.context("captura por processo sem interface")?.cast()?)
}

/// Captura o áudio de um processo (include = só ele e filhos; !include = tudo menos ele e filhos).
/// Entrega amostras f32 intercaladas, 48 kHz estéreo, para `sink`. Para quando `stop` vira true.
pub fn spawn_process_capture(
    pid: u32,
    include: bool,
    stop: Arc<AtomicBool>,
    mut sink: impl FnMut(&[f32]) + Send + 'static,
) -> Result<std::thread::JoinHandle<()>> {
    let (ready_tx, ready_rx) = mpsc::channel::<Result<()>>();
    let t = std::thread::spawn(move || unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        let setup = (|| -> Result<(IAudioClient, IAudioCaptureClient, windows::Win32::Foundation::HANDLE)> {
            let client = activate(pid, include)?;
            let block = CHANNELS * 4;
            let fmt = WAVEFORMATEX {
                wFormatTag: windows::Win32::Media::Multimedia::WAVE_FORMAT_IEEE_FLOAT as u16,
                nChannels: CHANNELS,
                nSamplesPerSec: RATE,
                nAvgBytesPerSec: RATE * block as u32,
                nBlockAlign: block,
                wBitsPerSample: 32,
                cbSize: 0,
            };
            client.Initialize(
                AUDCLNT_SHAREMODE_SHARED,
                AUDCLNT_STREAMFLAGS_LOOPBACK | AUDCLNT_STREAMFLAGS_EVENTCALLBACK | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM,
                200_000, // 20 ms
                0,
                &fmt,
                None,
            )?;
            let ev = CreateEventW(None, false, false, None)?;
            client.SetEventHandle(ev)?;
            let cap: IAudioCaptureClient = client.GetService()?;
            client.Start()?;
            Ok((client, cap, ev))
        })();
        let (client, cap, ev) = match setup {
            Ok(v) => {
                let _ = ready_tx.send(Ok(()));
                v
            }
            Err(e) => {
                let _ = ready_tx.send(Err(e));
                CoUninitialize();
                return;
            }
        };
        let mut buf: Vec<f32> = Vec::new();
        while !stop.load(Ordering::Relaxed) {
            if WaitForSingleObject(ev, 100) != WAIT_OBJECT_0 {
                continue;
            }
            loop {
                let Ok(n) = cap.GetNextPacketSize() else { break };
                if n == 0 {
                    break;
                }
                let mut data: *mut u8 = std::ptr::null_mut();
                let mut frames = 0u32;
                let mut flags = 0u32;
                if cap.GetBuffer(&mut data, &mut frames, &mut flags, None, None).is_err() {
                    break;
                }
                let samples = frames as usize * CHANNELS as usize;
                buf.clear();
                if flags & (AUDCLNT_BUFFERFLAGS_SILENT.0 as u32) != 0 || data.is_null() {
                    buf.resize(samples, 0.0);
                } else {
                    buf.extend_from_slice(std::slice::from_raw_parts(data as *const f32, samples));
                }
                let _ = cap.ReleaseBuffer(frames);
                sink(&buf);
            }
        }
        let _ = client.Stop();
        let _ = CloseHandle(ev);
        drop(cap);
        drop(client);
        CoUninitialize();
    });
    match ready_rx.recv() {
        Ok(Ok(())) => Ok(t),
        Ok(Err(e)) => Err(e),
        Err(_) => bail!(crate::i18n::t("a thread de captura de áudio terminou inesperadamente")),
    }
}
