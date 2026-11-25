//! Microphone capture via cpal (all platforms), plus system-audio loopback on macOS 14.6+
//! (cpal records an output device through a Core Audio process tap).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SizedSample, SupportedStreamConfig};

use super::{Format, InputDevice, Prepared};
use crate::recorder::track::SampleSink;

const OPEN_TIMEOUT: Option<Duration> = Some(Duration::from_secs(5));

fn device_name(d: &cpal::Device) -> String {
    d.description()
        .map(|d| d.name().to_owned())
        .unwrap_or_else(|_| "Unknown device".into())
}

fn device_id(d: &cpal::Device) -> Option<String> {
    d.id().ok().map(|id| id.to_string())
}

pub fn list_input_devices() -> Vec<InputDevice> {
    let host = cpal::default_host();
    let default_id = host.default_input_device().as_ref().and_then(device_id);
    let Ok(devices) = host.input_devices() else {
        return Vec::new();
    };
    devices
        .filter_map(|d| {
            let id = device_id(&d)?;
            Some(InputDevice {
                is_default: Some(&id) == default_id.as_ref(),
                name: device_name(&d),
                id,
            })
        })
        .collect()
}

/// Opens an input device. `strict_name = Some(name)` means the device was chosen as the meeting
/// source: if it is missing, fail (falling back to the default mic would duplicate the mic
/// track). Otherwise a missing device falls back to the system default.
/// `like` = (sample rate, channels) is preferred when the device supports it.
pub fn prepare_input(
    device_id_wanted: Option<&str>,
    like: Option<(u32, u16)>,
    strict_name: Option<&str>,
) -> Result<Prepared, String> {
    let host = cpal::default_host();
    let found = device_id_wanted.and_then(|want| {
        host.input_devices()
            .ok()?
            .find(|d| device_id(d).as_deref() == Some(want))
    });
    let device = match (found, strict_name) {
        (Some(d), _) => d,
        (None, Some(name)) => return Err(format!("{name} is not connected.")),
        (None, None) => host.default_input_device().ok_or("No microphone found")?,
    };
    let config = match like.and_then(|(rate, ch)| preferred_config(&device, rate, ch)) {
        Some(c) => c,
        None => device
            .default_input_config()
            .map_err(|e| format!("{} unavailable: {e}", device_name(&device)))?,
    };
    Ok(prepare(device, config))
}

fn preferred_config(
    device: &cpal::Device,
    sample_rate: u32,
    channels: u16,
) -> Option<SupportedStreamConfig> {
    let ranges: Vec<_> = device.supported_input_configs().ok()?.collect();
    let fits = |r: &cpal::SupportedStreamConfigRange| {
        r.min_sample_rate() <= sample_rate && sample_rate <= r.max_sample_rate()
    };
    ranges
        .iter()
        .find(|r| r.channels() == channels && fits(r))
        .or_else(|| ranges.iter().find(|r| fits(r)))
        .map(|r| r.with_sample_rate(sample_rate))
}

#[cfg(not(windows))]
pub fn list_output_devices() -> Vec<InputDevice> {
    let host = cpal::default_host();
    let default_id = host.default_output_device().as_ref().and_then(device_id);
    let Ok(devices) = host.output_devices() else {
        return Vec::new();
    };
    devices
        .filter_map(|d| {
            let id = device_id(&d)?;
            Some(InputDevice {
                is_default: Some(&id) == default_id.as_ref(),
                name: device_name(&d),
                id,
            })
        })
        .collect()
}

/// Records what an output device plays (macOS 14.6+, via a Core Audio tap). `None` = default.
#[cfg(not(windows))]
pub fn prepare_output_loopback(device_id_wanted: Option<&str>) -> Result<Prepared, String> {
    let host = cpal::default_host();
    let device = match device_id_wanted {
        Some(want) => host
            .output_devices()
            .map_err(|e| e.to_string())?
            .find(|d| device_id(d).as_deref() == Some(want))
            .ok_or("That output device is not connected.")?,
        None => host
            .default_output_device()
            .ok_or("No output device found")?,
    };
    let config = device
        .default_output_config()
        .map_err(|e| format!("System audio unavailable: {e}"))?;
    let mut p = prepare(device, config);
    if device_id_wanted.is_none() {
        p.label = "Default system audio".into();
    }
    Ok(p)
}

fn prepare(device: cpal::Device, config: SupportedStreamConfig) -> Prepared {
    let format = Format {
        sample_rate: config.sample_rate(),
        channels: config.channels(),
    };
    let label = device_name(&device);
    Prepared {
        format,
        label,
        start: Box::new(move |sink, stop, failed| {
            std::thread::Builder::new()
                .name("earshot-capture-cpal".into())
                .spawn(move || run(device, config, sink, stop, failed))
                .expect("spawn capture thread")
        }),
    }
}

/// Owns the cpal stream on this thread (streams are not `Send` on every backend) until stopped.
fn run(
    device: cpal::Device,
    config: SupportedStreamConfig,
    sink: SampleSink,
    stop: Arc<AtomicBool>,
    failed: Arc<AtomicBool>,
) {
    use cpal::SampleFormat as F;
    let fmt = config.sample_format();
    let cfg = config.config();
    let stream = match fmt {
        F::F32 => build::<f32>(&device, cfg, sink, failed.clone()),
        F::F64 => build::<f64>(&device, cfg, sink, failed.clone()),
        F::I16 => build::<i16>(&device, cfg, sink, failed.clone()),
        F::I32 => build::<i32>(&device, cfg, sink, failed.clone()),
        F::U16 => build::<u16>(&device, cfg, sink, failed.clone()),
        F::I8 => build::<i8>(&device, cfg, sink, failed.clone()),
        F::U8 => build::<u8>(&device, cfg, sink, failed.clone()),
        other => Err(format!("unsupported sample format {other:?}")),
    };
    let stream = match stream.and_then(|s| s.play().map(|_| s).map_err(|e| e.to_string())) {
        Ok(s) => s,
        Err(e) => {
            log::error!("capture failed to start: {e}");
            failed.store(true, Ordering::Relaxed);
            return;
        }
    };
    while !stop.load(Ordering::Acquire) {
        std::thread::sleep(Duration::from_millis(50));
    }
    drop(stream);
}

fn build<T>(
    device: &cpal::Device,
    cfg: cpal::StreamConfig,
    mut sink: SampleSink,
    failed: Arc<AtomicBool>,
) -> Result<cpal::Stream, String>
where
    T: SizedSample + Send + 'static,
    f32: FromSample<T>,
{
    device
        .build_input_stream::<T, _, _>(
            cfg,
            move |data: &[T], _| {
                sink.push_iter(data.len(), data.iter().map(|s| s.to_sample::<f32>()))
            },
            move |err: cpal::Error| {
                use cpal::ErrorKind as K;
                // Overruns, reroutes and similar are reported here too but the stream keeps
                // running; only errors that end the stream count as a failure.
                match err.kind() {
                    K::DeviceNotAvailable
                    | K::StreamInvalidated
                    | K::HostUnavailable
                    | K::PermissionDenied => {
                        log::error!("capture stream stopped: {err}");
                        failed.store(true, Ordering::Relaxed);
                    }
                    _ => log::warn!("capture stream: {err}"),
                }
            },
            OPEN_TIMEOUT,
        )
        .map_err(|e| e.to_string())
}
