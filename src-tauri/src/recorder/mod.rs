//! Meeting recorder: captures the mic and meeting audio as separate crash-safe FLAC tracks.
//!
//! Lifecycle: `start` creates the meeting row (status `recording`) and one writer + capture per
//! track. `stop` finalizes the files and queues the meeting for processing. A crash leaves the
//! meeting in `recording`; on next launch it is marked `incomplete` and [`Recorder::recover`]
//! repairs the files from whatever reached the disk.

pub mod capture;
pub mod flac;
pub mod track;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager};

use crate::db::{Db, now_ms};
use capture::{Capture, Prepared, SystemSource};
use track::{Clock, Level, TrackWriter};

pub const EVENT_STATE: &str = "recorder://state";
pub const EVENT_LEVELS: &str = "recorder://levels";
pub const EVENT_STOPPED: &str = "recorder://stopped";

/// Seconds of audio each ring buffer holds before samples are dropped.
const RING_SECONDS: u32 = 10;
const LEVELS_INTERVAL: Duration = Duration::from_millis(66);
/// The hotkey / tray continue an interrupted meeting instead of starting a new one within this
/// time of the interruption.
pub const QUICK_CONTINUE_WINDOW: Duration = Duration::from_secs(15 * 60);

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StartOptions {
    /// Microphone device id; `None` uses the system default.
    pub mic_device: Option<String>,
    /// `None` uses the saved default (default system audio).
    pub system: Option<SystemSource>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecorderState {
    /// `idle`, `recording` or `paused`.
    pub status: &'static str,
    pub meeting_id: Option<i64>,
    pub started_at: Option<i64>,
    pub elapsed_ms: u64,
    pub mic_source: Option<String>,
    pub system_source: Option<String>,
    /// Sources that could not be opened (recording continues with the rest).
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct Levels {
    mic: Option<f32>,
    system: Option<f32>,
    elapsed_ms: u64,
    mic_failed: bool,
    system_failed: bool,
}

struct LiveTrack {
    kind: &'static str,
    track_id: i64,
    label: String,
    // Field order matters: the capture is dropped (stopped) before the writer is finished.
    capture: Option<Capture>,
    writer: TrackWriter,
}

struct Session {
    meeting_id: i64,
    started_at: i64,
    clock: Arc<Clock>,
    tracks: Vec<LiveTrack>,
    warnings: Vec<String>,
    levels_stop: Arc<AtomicBool>,
    levels_join: Option<JoinHandle<()>>,
}

#[derive(Default)]
pub struct Recorder {
    session: Mutex<Option<Session>>,
}

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

/// Folder that holds one sub-folder per meeting.
pub fn recordings_dir(app: &AppHandle) -> Result<PathBuf, String> {
    let db = app.state::<Db>();
    if let Some(dir) = db
        .get_setting("recording.dir")
        .map_err(err)?
        .filter(|d| !d.is_empty())
    {
        return Ok(PathBuf::from(dir));
    }
    Ok(app.path().app_data_dir().map_err(err)?.join("recordings"))
}

impl Recorder {
    fn lock(&self) -> MutexGuard<'_, Option<Session>> {
        self.session.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn is_active(&self) -> bool {
        self.lock().is_some()
    }

    pub fn state(&self) -> RecorderState {
        let guard = self.lock();
        match guard.as_ref() {
            None => RecorderState {
                status: "idle",
                meeting_id: None,
                started_at: None,
                elapsed_ms: 0,
                mic_source: None,
                system_source: None,
                warnings: Vec::new(),
            },
            Some(s) => RecorderState {
                status: if s.clock.is_paused() {
                    "paused"
                } else {
                    "recording"
                },
                meeting_id: Some(s.meeting_id),
                started_at: Some(s.started_at),
                elapsed_ms: s.clock.elapsed().as_millis() as u64,
                mic_source: s
                    .tracks
                    .iter()
                    .find(|t| t.kind == "mic")
                    .map(|t| t.label.clone()),
                system_source: s
                    .tracks
                    .iter()
                    .find(|t| t.kind == "system")
                    .map(|t| t.label.clone()),
                warnings: s.warnings.clone(),
            },
        }
    }

    fn emit_state(&self, app: &AppHandle) {
        let state = self.state();
        crate::tray::set_recording(app, state.status != "idle");
        let _ = app.emit(EVENT_STATE, &state);
    }

    pub fn start(&self, app: &AppHandle, opts: StartOptions) -> Result<RecorderState, String> {
        let mut guard = self.lock();
        if guard.is_some() {
            return Err("Already recording".into());
        }
        let db = app.state::<Db>();

        let mic_device = opts
            .mic_device
            .or_else(|| db.get_setting("recording.micDevice").ok().flatten());
        let system = match opts.system {
            Some(s) => s,
            None => saved_system_source(
                db.get_setting("recording.systemSource")
                    .map_err(err)?
                    .as_deref(),
            ),
        };
        if let SystemSource::Input { id, name } = &system {
            let mic_id = mic_device.clone().filter(|d| !d.is_empty()).or_else(|| {
                capture::list_input_devices()
                    .into_iter()
                    .find(|d| d.is_default)
                    .map(|d| d.id)
            });
            if mic_id.as_deref() == Some(id.as_str()) {
                return Err(format!(
                    "{name} is also your microphone. Pick a different meeting audio source or microphone."
                ));
            }
        }
        let system_spec = serde_json::to_string(&system).ok();

        let mut warnings = Vec::new();
        let mut prepared: Vec<(&'static str, Prepared)> = Vec::new();
        match capture::prepare_mic(mic_device.as_deref().filter(|d| !d.is_empty())) {
            Ok(p) => prepared.push(("mic", p)),
            Err(e) => warnings.push(format!("Microphone: {e}")),
        }
        match capture::prepare_system(&system) {
            Ok(Some(p)) => prepared.push(("system", p)),
            Ok(None) => {}
            Err(e) => warnings.push(format!("Meeting audio: {e}")),
        }
        if prepared.is_empty() {
            return Err(warnings.join("\n"));
        }

        let started_at = now_ms();
        let meeting_id = db.create_meeting(started_at, "").map_err(err)?;
        let dir = recordings_dir(app)?.join(format!("meeting-{meeting_id}"));
        std::fs::create_dir_all(&dir)
            .map_err(|e| format!("Cannot create {}: {e}", dir.display()))?;

        let clock = Arc::new(Clock::start());
        let mut tracks = Vec::new();
        for (kind, p) in prepared {
            let spec = if kind == "system" {
                system_spec.as_deref()
            } else {
                None
            };
            match start_track(&db, meeting_id, kind, p, spec, &dir, clock.clone()) {
                Ok(t) => tracks.push(t),
                Err(e) => warnings.push(format!("{kind}: {e}")),
            }
        }
        if tracks.is_empty() {
            let _ =
                db.set_meeting_status(meeting_id, "failed", Some(0), Some(&warnings.join("\n")));
            return Err(warnings.join("\n"));
        }

        let (levels_stop, levels_join) = spawn_levels(app.clone(), clock.clone(), &tracks);
        *guard = Some(Session {
            meeting_id,
            started_at,
            clock,
            tracks,
            warnings,
            levels_stop,
            levels_join: Some(levels_join),
        });
        drop(guard);
        log::info!("recording started: meeting {meeting_id}");
        self.emit_state(app);
        crate::pill::show(app);
        Ok(self.state())
    }

    pub fn set_paused(&self, app: &AppHandle, paused: bool) -> Result<RecorderState, String> {
        {
            let guard = self.lock();
            let s = guard.as_ref().ok_or("Not recording")?;
            if paused {
                s.clock.pause()
            } else {
                s.clock.resume()
            }
        }
        self.emit_state(app);
        Ok(self.state())
    }

    /// Stops recording, finalizes all tracks and returns the meeting id.
    pub fn stop(&self, app: &AppHandle) -> Result<i64, String> {
        let mut session = self.lock().take().ok_or("Not recording")?;
        session.levels_stop.store(true, Ordering::Release);
        if let Some(j) = session.levels_join.take() {
            let _ = j.join();
        }
        let db = app.state::<Db>();
        let duration_ms = session.clock.elapsed().as_millis() as i64;
        let mut errors = Vec::new();
        for mut t in session.tracks {
            drop(t.capture.take()); // stop capturing before draining the writer
            let dropped = t.writer.overflow.load(Ordering::Relaxed);
            if dropped > 0 {
                log::warn!(
                    "{} track dropped {dropped} samples (writer fell behind)",
                    t.kind
                );
            }
            match t.writer.finish() {
                Ok(stats) => {
                    let ms = (stats.frames * 1000 / u64::from(stats.sample_rate)) as i64;
                    if let Err(e) = db.finalize_track(t.track_id, ms) {
                        errors.push(e.to_string());
                    }
                }
                Err(e) => errors.push(format!("{}: {e}", t.kind)),
            }
        }
        // A failed finalize leaves recoverable files behind: offer recovery instead of failing.
        let (status, error) = if errors.is_empty() {
            ("queued", None)
        } else {
            ("incomplete", Some(errors.join("\n")))
        };
        db.set_meeting_status(
            session.meeting_id,
            status,
            Some(duration_ms),
            error.as_deref(),
        )
        .map_err(err)?;
        log::info!(
            "recording stopped: meeting {} ({duration_ms} ms)",
            session.meeting_id
        );
        crate::pill::hide(app);
        self.emit_state(app);
        let _ = app.emit(EVENT_STOPPED, session.meeting_id);
        if status == "queued" {
            crate::processing::auto_enqueue(app, session.meeting_id);
        }
        Ok(session.meeting_id)
    }

    /// Continues an interrupted meeting in the same track files. Each track is reopened after its
    /// last intact frame; the clock resumes at the recorded length plus the time the app was down,
    /// so the writers pad the outage with silence and the tracks stay aligned with real time.
    pub fn resume(&self, app: &AppHandle, meeting_id: i64) -> Result<RecorderState, String> {
        let mut guard = self.lock();
        if guard.is_some() {
            return Err("Already recording".into());
        }
        let db = app.state::<Db>();
        let meeting = db
            .meeting(meeting_id)
            .map_err(err)?
            .ok_or("Meeting not found")?;
        if meeting.status != "incomplete" {
            return Err(format!("Meeting is {}, not interrupted", meeting.status));
        }
        let rows = db.tracks(meeting_id).map_err(err)?;
        let stopped_at = last_write_ms(&rows).unwrap_or_else(now_ms);

        let mut warnings = Vec::new();
        let mut opened = Vec::new();
        for t in rows {
            match flac::FlacWriter::resume(Path::new(&t.path)) {
                Ok(w) => opened.push((t, w)),
                Err(e) => warnings.push(format!("{}: cannot reopen {}: {e}", t.kind, t.path)),
            }
        }
        if opened.is_empty() {
            return Err(format!("Nothing to continue.\n{}", warnings.join("\n")));
        }
        let recorded = opened
            .iter()
            .map(|(_, w)| {
                Duration::from_secs_f64(w.frames_written() as f64 / f64::from(w.sample_rate()))
            })
            .max()
            .unwrap_or_default();
        let gap = Duration::from_millis(now_ms().saturating_sub(stopped_at).max(0) as u64);
        let clock = Arc::new(Clock::start_at(recorded + gap));

        let mic_device = db
            .get_setting("recording.micDevice")
            .ok()
            .flatten()
            .filter(|d| !d.is_empty());
        let mut tracks = Vec::new();
        for (row, w) in opened {
            let kind: &'static str = if row.kind == "mic" { "mic" } else { "system" };
            let prepared = if kind == "mic" {
                capture::prepare_mic_like(mic_device.as_deref(), w.sample_rate(), w.channels())
            } else {
                let source = row
                    .source_spec
                    .as_deref()
                    .and_then(|j| serde_json::from_str::<SystemSource>(j).ok())
                    .map(Ok)
                    .unwrap_or_else(|| system_source_for(row.source.as_deref(), &mut warnings));
                source.and_then(|s| {
                    capture::prepare_system_like(&s, Some((w.sample_rate(), w.channels())))?
                        .ok_or_else(|| "no source".into())
                })
            };
            let p = match prepared {
                Ok(p) if p.format.sample_rate == w.sample_rate() => p,
                Ok(p) => {
                    warnings.push(format!(
                        "{kind}: the device now runs at {} Hz instead of {} Hz; this track was not continued",
                        p.format.sample_rate,
                        w.sample_rate()
                    ));
                    close_track(&db, row.id, w);
                    continue;
                }
                Err(e) => {
                    warnings.push(format!("{kind}: {e}"));
                    close_track(&db, row.id, w);
                    continue;
                }
            };
            let label = p.label.clone();
            let (sink, consumer, overflow) =
                track::ring(p.format.sample_rate, p.format.channels, RING_SECONDS);
            let writer = TrackWriter::spawn(
                kind,
                w,
                p.format.channels,
                consumer,
                overflow,
                clock.clone(),
            )
            .map_err(err)?;
            db.reopen_track(row.id).map_err(err)?;
            tracks.push(LiveTrack {
                kind,
                track_id: row.id,
                label,
                capture: Some(p.start(sink)),
                writer,
            });
        }
        if tracks.is_empty() {
            let _ = recover(&db, meeting_id);
            return Err(format!(
                "Could not continue recording.\n{}",
                warnings.join("\n")
            ));
        }
        db.set_meeting_status(meeting_id, "recording", None, None)
            .map_err(err)?;

        let (levels_stop, levels_join) = spawn_levels(app.clone(), clock.clone(), &tracks);
        *guard = Some(Session {
            meeting_id,
            started_at: meeting.started_at,
            clock,
            tracks,
            warnings,
            levels_stop,
            levels_join: Some(levels_join),
        });
        drop(guard);
        log::info!(
            "recording continued: meeting {meeting_id} after {:.1} s outage",
            gap.as_secs_f64()
        );
        self.emit_state(app);
        crate::pill::show(app);
        Ok(self.state())
    }

    /// Hotkey / tray: stop if recording; otherwise continue a meeting interrupted in the last
    /// few minutes, or start a new one with the saved defaults.
    pub fn toggle(&self, app: &AppHandle) {
        let result = if self.is_active() {
            self.stop(app).map(|_| ())
        } else if let Some(i) = interrupted(&app.state::<Db>())
            .filter(|i| now_ms() - i.stopped_at < QUICK_CONTINUE_WINDOW.as_millis() as i64)
        {
            self.resume(app, i.meeting_id).map(|_| ())
        } else {
            self.start(app, StartOptions::default()).map(|_| ())
        };
        if let Err(e) = result {
            log::error!("toggle recording failed: {e}");
            let _ = app.emit("recorder://error", e);
        }
    }
}

/// The saved default meeting source: "all" (or unset), "none", or a JSON `SystemSource`.
pub fn saved_system_source(value: Option<&str>) -> SystemSource {
    match value {
        None | Some("") | Some("all") => SystemSource::All,
        Some("none") => SystemSource::None,
        Some(json) => serde_json::from_str(json).unwrap_or(SystemSource::All),
    }
}

/// Finalizes a reopened track that will not be continued, keeping what was recorded.
fn close_track(db: &Db, track_id: i64, w: flac::FlacWriter) {
    let rate = u64::from(w.sample_rate().max(1));
    match w.finish() {
        Ok(frames) => {
            let _ = db.finalize_track(track_id, (frames * 1000 / rate) as i64);
        }
        Err(e) => log::error!("finalizing track {track_id} failed: {e}"),
    }
}

/// Picks the meeting-audio source to continue with, from the label stored with the track.
fn system_source_for(
    label: Option<&str>,
    warnings: &mut Vec<String>,
) -> Result<SystemSource, String> {
    match label {
        // "All system audio" is the label older tracks were saved with.
        None | Some("Default system audio" | "All system audio") => Ok(SystemSource::All),
        Some(name) => {
            if let Some(app) = capture::list_audio_apps()
                .into_iter()
                .find(|a| a.name == name)
            {
                Ok(SystemSource::App {
                    pid: app.pid,
                    name: app.name,
                })
            } else {
                warnings.push(format!(
                    "{name} isn't playing audio right now; recording the default system audio instead."
                ));
                Ok(SystemSource::All)
            }
        }
    }
}

/// When track files were last written (≈ when the app stopped), in Unix ms.
fn last_write_ms(tracks: &[crate::db::TrackRow]) -> Option<i64> {
    tracks
        .iter()
        .filter_map(|t| std::fs::metadata(&t.path).and_then(|m| m.modified()).ok())
        .filter_map(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .max()
}

/// An interrupted meeting that can be continued or saved.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Interrupted {
    pub meeting_id: i64,
    pub title: String,
    pub started_at: i64,
    /// When audio stopped being written (Unix ms).
    pub stopped_at: i64,
}

/// The most recently interrupted meeting, if any.
pub fn interrupted(db: &Db) -> Option<Interrupted> {
    let m = db.latest_meeting_with_status("incomplete").ok().flatten()?;
    let tracks = db.tracks(m.id).ok()?;
    Some(Interrupted {
        meeting_id: m.id,
        title: m.title,
        started_at: m.started_at,
        stopped_at: last_write_ms(&tracks).unwrap_or(m.started_at),
    })
}

#[allow(clippy::too_many_arguments)]
fn start_track(
    db: &Db,
    meeting_id: i64,
    kind: &'static str,
    p: Prepared,
    spec: Option<&str>,
    dir: &Path,
    clock: Arc<Clock>,
) -> Result<LiveTrack, String> {
    let path = dir.join(format!("{kind}.flac"));
    let fmt = p.format;
    let label = p.label.clone();
    let (sink, consumer, overflow) = track::ring(fmt.sample_rate, fmt.channels, RING_SECONDS);
    let flac = flac::FlacWriter::create(&path, fmt.sample_rate, fmt.channels).map_err(err)?;
    let writer =
        TrackWriter::spawn(kind, flac, fmt.channels, consumer, overflow, clock).map_err(err)?;
    let track_id = db
        .add_track(
            meeting_id,
            kind,
            &path.to_string_lossy(),
            fmt.sample_rate,
            fmt.channels,
            &label,
            spec,
        )
        .map_err(err)?;
    let capture = p.start(sink);
    Ok(LiveTrack {
        kind,
        track_id,
        label,
        capture: Some(capture),
        writer,
    })
}

fn spawn_levels(
    app: AppHandle,
    clock: Arc<Clock>,
    tracks: &[LiveTrack],
) -> (Arc<AtomicBool>, JoinHandle<()>) {
    let stop = Arc::new(AtomicBool::new(false));
    let find = |kind: &str| -> Option<(Arc<Level>, Arc<AtomicBool>)> {
        tracks.iter().find(|t| t.kind == kind).map(|t| {
            (
                t.writer.level.clone(),
                t.capture
                    .as_ref()
                    .map(|c| c.failed.clone())
                    .unwrap_or_default(),
            )
        })
    };
    let (mic, system) = (find("mic"), find("system"));
    let stop_t = stop.clone();
    let join = std::thread::Builder::new()
        .name("earshot-levels".into())
        .spawn(move || {
            while !stop_t.load(Ordering::Acquire) {
                let levels = Levels {
                    mic: mic.as_ref().map(|(l, _)| l.get()),
                    system: system.as_ref().map(|(l, _)| l.get()),
                    elapsed_ms: clock.elapsed().as_millis() as u64,
                    mic_failed: mic.as_ref().is_some_and(|(_, f)| f.load(Ordering::Relaxed)),
                    system_failed: system
                        .as_ref()
                        .is_some_and(|(_, f)| f.load(Ordering::Relaxed)),
                };
                let _ = app.emit(EVENT_LEVELS, &levels);
                std::thread::sleep(LEVELS_INTERVAL);
            }
        })
        .expect("spawn levels thread");
    (stop, join)
}

/// Repairs the tracks of an interrupted meeting and queues it for processing.
pub fn recover(db: &Db, meeting_id: i64) -> Result<i64, String> {
    let status = db
        .meeting_status(meeting_id)
        .map_err(err)?
        .ok_or("Meeting not found")?;
    if status != "incomplete" {
        return Err(format!("Meeting is {status}, not incomplete"));
    }
    let mut longest = 0i64;
    let mut recovered_any = false;
    for t in db.tracks(meeting_id).map_err(err)? {
        let path = Path::new(&t.path);
        if !path.exists() {
            log::warn!("track file missing: {}", t.path);
            continue;
        }
        let ms = if t.finalized {
            t.duration_ms
        } else {
            let r = flac::recover(path).map_err(|e| format!("{}: {e}", t.kind))?;
            let ms = (r.frames * 1000 / u64::from(r.sample_rate.max(1))) as i64;
            db.finalize_track(t.id, ms).map_err(err)?;
            ms
        };
        recovered_any = true;
        longest = longest.max(ms);
    }
    if !recovered_any {
        db.set_meeting_status(
            meeting_id,
            "failed",
            Some(0),
            Some("No audio files were found."),
        )
        .map_err(err)?;
        return Err("No audio files were found for this meeting.".into());
    }
    db.set_meeting_status(meeting_id, "queued", Some(longest), None)
        .map_err(err)?;
    Ok(longest)
}
