//! Windows meeting-audio capture via WASAPI:
//! - all system audio: loopback on the default render device;
//! - a single app: process loopback (`AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK`, Windows 10
//!   build 20348+), which includes the app's child processes.
//!
//! Both are requested as 48 kHz stereo f32 and converted by Windows. Loopback streams deliver no
//! packets while nothing is playing; the track writer fills those gaps with silence.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};

use wasapi::{Direction, SampleType, SessionState, StreamMode, WaveFormat};

use super::{AudioApp, Format, Prepared};
use crate::recorder::track::SampleSink;

const RATE: u32 = 48_000;
const CHANNELS: u16 = 2;
/// First Windows build with process loopback activation.
const PROCESS_LOOPBACK_MIN_BUILD: u32 = 20_348;

fn windows_build() -> Option<u32> {
    sysinfo::System::kernel_version()
        .ok()?
        .split('.')
        .next_back()?
        .trim()
        .parse()
        .ok()
}

pub fn supports_process_loopback() -> bool {
    windows_build().is_some_and(|b| b >= PROCESS_LOOPBACK_MIN_BUILD)
}

pub fn prepare(app: Option<(u32, String)>) -> Result<Prepared, String> {
    if app.is_some() && !supports_process_loopback() {
        return Err("Recording a single app requires Windows 10 build 20348 or newer.".into());
    }
    // Process loopback on an exited PID succeeds but records silence: refuse it up front.
    if let Some((pid, name)) = &app
        && !process_running(*pid)
    {
        return Err(format!(
            "{name} is no longer running. Pick it again from the list."
        ));
    }
    let label = match &app {
        Some((_, name)) => name.clone(),
        None => "All system audio".into(),
    };
    let pid = app.map(|(pid, _)| pid);
    Ok(Prepared {
        format: Format {
            sample_rate: RATE,
            channels: CHANNELS,
        },
        label,
        start: Box::new(move |sink, stop, failed| {
            std::thread::Builder::new()
                .name("earshot-capture-wasapi".into())
                .spawn(move || {
                    if let Err(e) = run(pid, sink, &stop) {
                        log::error!("loopback capture failed: {e}");
                        failed.store(true, Ordering::Relaxed);
                    }
                })
                .expect("spawn capture thread")
        }),
    })
}

fn run(
    pid: Option<u32>,
    mut sink: SampleSink,
    stop: &AtomicBool,
) -> Result<(), Box<dyn std::error::Error>> {
    wasapi::initialize_mta().ok()?;
    let format = WaveFormat::new(
        32,
        32,
        SampleType::Float,
        RATE as usize,
        CHANNELS as usize,
        None,
    );
    let mut client = match pid {
        Some(pid) => wasapi::AudioClient::new_application_loopback_client(pid, true)?,
        None => {
            let device = wasapi::DeviceEnumerator::new()?.get_default_device(Direction::Render)?;
            device.get_iaudioclient()?
        }
    };
    // Capture direction on a render device / process client = loopback.
    let mode = StreamMode::EventsShared {
        autoconvert: true,
        buffer_duration_hns: 0,
    };
    client.initialize_client(&format, Direction::Capture, mode, None)?;
    let event = client.set_get_eventhandle()?;
    let capture = client.get_audiocaptureclient()?;
    client.start_stream()?;

    let mut bytes: VecDeque<u8> = VecDeque::with_capacity(RATE as usize * 8);
    while !stop.load(Ordering::Acquire) {
        // Events stop while nothing plays; the timeout keeps the stop flag responsive.
        let _ = event.wait_for_event(100);
        while capture.get_next_packet_size()?.unwrap_or(0) > 0 {
            capture.read_from_device_to_deque(&mut bytes)?;
        }
        let usable = bytes.len() - bytes.len() % 4;
        let (a, b) = bytes.as_slices();
        // Samples never straddle the two halves in practice, but handle it via a copy if they do.
        if a.len() % 4 == 0 {
            push_f32_le(&mut sink, a);
            push_f32_le(&mut sink, &b[..usable - a.len()]);
        } else {
            let flat: Vec<u8> = bytes.iter().take(usable).copied().collect();
            push_f32_le(&mut sink, &flat);
        }
        bytes.drain(..usable);
    }
    client.stop_stream()?;
    Ok(())
}

fn push_f32_le(sink: &mut SampleSink, bytes: &[u8]) {
    let n = bytes.len() / 4;
    sink.push_iter(
        n,
        bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| f32::from_le_bytes(*c)),
    );
}

/// Processes with an audio session on any render device, grouped under the top-most ancestor
/// with the same executable (browsers and chat apps play audio from child processes).
pub fn list_audio_apps() -> Vec<AudioApp> {
    // COM must be initialized as MTA; pooled threads may already be STA, so use a fresh thread.
    let sessions = match std::thread::spawn(|| sessions().map_err(|e| e.to_string()))
        .join()
        .unwrap_or_else(|_| Err("session listing panicked".into()))
    {
        Ok(s) => s,
        Err(e) => {
            log::warn!("listing audio sessions failed: {e}");
            return Vec::new();
        }
    };
    let Ok(mut sys) = sysinfo::System::new() else {
        return Vec::new();
    };
    sys.refresh_processes(sysinfo::ProcessesToUpdate::All, true);
    let own = std::process::id();

    let mut apps: HashMap<u32, AudioApp> = HashMap::new();
    for (pid, active) in sessions {
        if pid == 0 || pid == own {
            continue;
        }
        let Some(proc_) = sys.process(sysinfo::Pid::from_u32(pid)) else {
            continue;
        };
        let exe = proc_.name().to_os_string();
        // Walk up while the parent is the same program.
        let mut root = proc_;
        while let Some(parent) = root.parent().and_then(|p| sys.process(p)) {
            if parent.name() != exe {
                break;
            }
            root = parent;
        }
        let name = pretty_name(&exe.to_string_lossy());
        let entry = apps.entry(root.pid().as_u32()).or_insert(AudioApp {
            pid: root.pid().as_u32(),
            name,
            active: false,
        });
        entry.active |= active;
    }
    let mut list: Vec<AudioApp> = apps.into_values().collect();
    list.sort_by(|a, b| {
        b.active
            .cmp(&a.active)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    list
}

fn process_running(pid: u32) -> bool {
    let Ok(mut sys) = sysinfo::System::new() else {
        return true;
    };
    let pid = sysinfo::Pid::from_u32(pid);
    sys.refresh_processes(sysinfo::ProcessesToUpdate::Some(&[pid]), true);
    sys.process(pid).is_some()
}

fn pretty_name(exe: &str) -> String {
    let stem = exe
        .strip_suffix(".exe")
        .or_else(|| exe.strip_suffix(".EXE"))
        .unwrap_or(exe);
    let mut c = stem.chars();
    match c.next() {
        Some(f) => f.to_uppercase().chain(c).collect(),
        None => exe.to_owned(),
    }
}

/// (pid, active) for every audio session on every active render device.
fn sessions() -> Result<Vec<(u32, bool)>, Box<dyn std::error::Error>> {
    wasapi::initialize_mta().ok()?;
    let enumerator = wasapi::DeviceEnumerator::new()?;
    let mut out = Vec::new();
    for device in &enumerator.get_device_collection(Direction::Render)? {
        let device = device?;
        let sessions = device
            .get_iaudiosessionmanager()?
            .get_audiosessionenumerator()?;
        for i in 0..sessions.get_count()? {
            let control = sessions.get_session(i)?;
            let state = control.get_state()?;
            if state == SessionState::Expired {
                continue;
            }
            out.push((control.get_process_id()?, state == SessionState::Active));
        }
    }
    Ok(out)
}
