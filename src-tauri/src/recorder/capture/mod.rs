//! Audio sources. Each capture runs on its own thread and pushes interleaved f32 samples into a
//! [`SampleSink`]; the format is known before the thread starts so the ring buffer can be sized.

mod cpal_input;
#[cfg(windows)]
mod wasapi_loopback;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;

use serde::{Deserialize, Serialize};

use super::track::SampleSink;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Format {
    pub sample_rate: u32,
    pub channels: u16,
}

/// What to record on the meeting (system) track.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum SystemSource {
    /// Don't record a meeting track.
    None,
    /// Everything played through the default output device.
    All,
    /// One application (and its child processes). A saved default is found again by `name`
    /// when its `pid` is no longer running.
    App { pid: u32, name: String },
    /// Everything played through one specific output device (e.g. a virtual "Voice Chat" device).
    Output { id: String, name: String },
    /// A specific input device (e.g. a virtual capture endpoint carrying the call audio).
    Input { id: String, name: String },
}

/// Starts the capture thread: (sink, stop flag, failed flag) -> thread handle.
type StartFn =
    Box<dyn FnOnce(SampleSink, Arc<AtomicBool>, Arc<AtomicBool>) -> JoinHandle<()> + Send>;

/// A capture source that has been opened but not started.
pub struct Prepared {
    pub format: Format,
    /// Human-readable description, stored with the track.
    pub label: String,
    start: StartFn,
}

impl Prepared {
    /// Starts capturing into `sink`. `failed` is set if the device stops delivering (unplugged).
    pub fn start(self, sink: SampleSink) -> Capture {
        let stop = Arc::new(AtomicBool::new(false));
        let failed = Arc::new(AtomicBool::new(false));
        let join = (self.start)(sink, stop.clone(), failed.clone());
        Capture {
            stop,
            failed,
            join: Some(join),
        }
    }
}

/// A running capture thread.
pub struct Capture {
    stop: Arc<AtomicBool>,
    pub failed: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
}

impl Drop for Capture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InputDevice {
    pub id: String,
    pub name: String,
    pub is_default: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioApp {
    pub pid: u32,
    pub name: String,
    /// True when the app is producing sound right now.
    pub active: bool,
}

pub fn list_input_devices() -> Vec<InputDevice> {
    cpal_input::list_input_devices()
}

/// Output devices whose playback can be recorded (loopback).
pub fn list_output_devices() -> Vec<InputDevice> {
    #[cfg(windows)]
    {
        wasapi_loopback::list_output_devices()
    }
    #[cfg(not(windows))]
    {
        cpal_input::list_output_devices()
    }
}

/// Opens the microphone (`None` = system default; a saved device that is unplugged falls back
/// to the default).
pub fn prepare_mic(device_id: Option<&str>) -> Result<Prepared, String> {
    cpal_input::prepare_input(device_id, None, None)
}

/// Opens the mic preferring the given format (continuing an existing track).
pub fn prepare_mic_like(
    device_id: Option<&str>,
    sample_rate: u32,
    channels: u16,
) -> Result<Prepared, String> {
    cpal_input::prepare_input(device_id, Some((sample_rate, channels)), None)
}

/// Opens the meeting-audio source.
pub fn prepare_system(source: &SystemSource) -> Result<Option<Prepared>, String> {
    prepare_system_like(source, None)
}

/// Opens the meeting-audio source, preferring `like` = (sample rate, channels) where the source
/// lets us choose (continuing an existing track file).
pub fn prepare_system_like(
    source: &SystemSource,
    like: Option<(u32, u16)>,
) -> Result<Option<Prepared>, String> {
    match source {
        SystemSource::None => Ok(None),
        SystemSource::Input { id, name } => {
            cpal_input::prepare_input(Some(id), like, Some(name)).map(Some)
        }
        #[cfg(windows)]
        SystemSource::All => wasapi_loopback::prepare(wasapi_loopback::Target::Default).map(Some),
        #[cfg(windows)]
        SystemSource::Output { id, name } => {
            wasapi_loopback::prepare(wasapi_loopback::Target::Device {
                id: id.clone(),
                name: name.clone(),
            })
            .map(Some)
        }
        #[cfg(windows)]
        SystemSource::App { pid, name } => {
            let pid = wasapi_loopback::resolve_app(*pid, name)?;
            wasapi_loopback::prepare(wasapi_loopback::Target::App {
                pid,
                name: name.clone(),
            })
            .map(Some)
        }
        #[cfg(not(windows))]
        SystemSource::All => cpal_input::prepare_output_loopback(None).map(Some),
        #[cfg(not(windows))]
        SystemSource::Output { id, .. } => cpal_input::prepare_output_loopback(Some(id)).map(Some),
        #[cfg(not(windows))]
        SystemSource::App { .. } => {
            Err("Recording a single app is not supported on this system yet.".into())
        }
    }
}

/// Whether recording a single app is available on this OS.
pub fn supports_app_capture() -> bool {
    #[cfg(windows)]
    {
        wasapi_loopback::supports_process_loopback()
    }
    #[cfg(not(windows))]
    {
        false
    }
}

/// Apps that currently have an audio session (candidates for single-app capture).
pub fn list_audio_apps() -> Vec<AudioApp> {
    #[cfg(windows)]
    {
        wasapi_loopback::list_audio_apps()
    }
    #[cfg(not(windows))]
    {
        Vec::new()
    }
}
