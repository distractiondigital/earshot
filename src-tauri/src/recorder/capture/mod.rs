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
    /// One application (and its child processes).
    App { pid: u32, name: String },
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

/// Opens the microphone (`None` = system default).
pub fn prepare_mic(device_id: Option<&str>) -> Result<Prepared, String> {
    cpal_input::prepare_mic(device_id)
}

/// Opens the mic preferring the given format (continuing an existing track).
pub fn prepare_mic_like(
    device_id: Option<&str>,
    sample_rate: u32,
    channels: u16,
) -> Result<Prepared, String> {
    cpal_input::prepare_mic_like(device_id, sample_rate, channels)
}

/// Opens the meeting-audio source.
pub fn prepare_system(source: &SystemSource) -> Result<Option<Prepared>, String> {
    match source {
        SystemSource::None => Ok(None),
        #[cfg(windows)]
        SystemSource::All => wasapi_loopback::prepare(None).map(Some),
        #[cfg(windows)]
        SystemSource::App { pid, name } => {
            wasapi_loopback::prepare(Some((*pid, name.clone()))).map(Some)
        }
        #[cfg(not(windows))]
        SystemSource::All => cpal_input::prepare_system_loopback().map(Some),
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
