//! Post-call processing supervisor. Runs queued jobs one at a time, each track in its own
//! `earshot-worker` process, so a model crash (OOM, driver fault) never takes down the app or a
//! recording. Results are saved per track as soon as they arrive (resumable / idempotent).
//!
//! Failure handling:
//! - the worker reports a GPU failure -> retried once on the CPU, and the user is told;
//! - the worker dies without reporting (crash, killed) -> the job fails with a Retry option;
//! - the app itself dies mid-job -> on next launch the job is queued again.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use earshot_protocol::{Backend, ErrorCode, Event, Request, Stage, TranscribeJob, Transcript};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager};

use crate::db::{Db, NewUtterance, NewWord};
use crate::models;

pub const EVENT_PROGRESS: &str = "processing://progress";
pub const EVENT_CHANGED: &str = "processing://changed";

/// A pause longer than this starts a new utterance.
const UTTERANCE_PAUSE_MS: u64 = 1_200;
const JOB_KIND: &str = "transcribe";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JobOptions {
    /// Force a backend for this run ("cpu" after a GPU problem).
    pub backend: Option<Backend>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct Progress {
    meeting_id: i64,
    /// 0-based index of the track being processed, and how many there are.
    track: usize,
    tracks: usize,
    track_kind: String,
    stage: Stage,
    /// Overall 0–100 for the meeting.
    pct: f32,
}

/// The job currently running (so it can be cancelled).
struct Current {
    meeting_id: i64,
    stdin: Option<ChildStdin>,
    child: Option<Child>,
    cancelled: bool,
}

/// Managed state: wakes the processing thread and tracks the running job.
pub struct Processor {
    wake: Sender<()>,
    current: Arc<Mutex<Option<Current>>>,
}

impl Processor {
    /// Starts the processing thread. Jobs interrupted by a previous crash are queued again.
    pub fn start(app: &AppHandle) -> Self {
        let (wake, rx) = mpsc::channel();
        let current = Arc::new(Mutex::new(None));
        if let Ok(n) = app.state::<Db>().requeue_interrupted_jobs()
            && n > 0
        {
            log::info!("{n} interrupted processing job(s) queued again");
        }
        let (app2, current2) = (app.clone(), current.clone());
        std::thread::Builder::new()
            .name("earshot-processing".into())
            .spawn(move || run_loop(app2, rx, current2))
            .expect("spawn processing thread");
        let _ = wake.send(());
        Self { wake, current }
    }

    pub fn notify(&self) {
        let _ = self.wake.send(());
    }

    /// Cancels a meeting's queued or running transcription.
    pub fn cancel(&self, app: &AppHandle, meeting_id: i64) {
        let db = app.state::<Db>();
        let _ = db.cancel_queued_jobs(meeting_id);
        let mut cur = self.current.lock().unwrap();
        if let Some(c) = cur.as_mut().filter(|c| c.meeting_id == meeting_id) {
            c.cancelled = true;
            // Ask nicely, then make sure: the worker exits on stdin EOF anyway.
            if let Some(stdin) = c.stdin.as_mut() {
                let _ = writeln!(
                    stdin,
                    "{}",
                    serde_json::to_string(&Request::Cancel { id: 1 }).unwrap()
                );
            }
            if let Some(child) = c.child.as_mut() {
                let _ = child.kill();
            }
        }
        let _ = app.emit(EVENT_CHANGED, meeting_id);
    }
}

/// Queues transcription of a meeting.
pub fn enqueue(app: &AppHandle, meeting_id: i64, options: JobOptions) -> Result<(), String> {
    let opts = serde_json::to_string(&options).ok();
    app.state::<Db>()
        .enqueue_job(meeting_id, JOB_KIND, opts.as_deref())
        .map_err(|e| e.to_string())?;
    app.state::<Processor>().notify();
    let _ = app.emit(EVENT_CHANGED, meeting_id);
    Ok(())
}

/// Called after a recording stops (or is recovered): queue it if auto-processing is on.
pub fn auto_enqueue(app: &AppHandle, meeting_id: i64) {
    let auto = app
        .state::<Db>()
        .get_setting("transcription.autoProcess")
        .ok()
        .flatten();
    if auto.as_deref() != Some("false")
        && models::selected(app).is_some()
        && let Err(e) = enqueue(app, meeting_id, JobOptions::default())
    {
        log::error!("could not queue meeting {meeting_id}: {e}");
    }
}

fn run_loop(app: AppHandle, wake: Receiver<()>, current: Arc<Mutex<Option<Current>>>) {
    loop {
        let job = match app.state::<Db>().next_queued_job() {
            Ok(Some(j)) => j,
            Ok(None) => {
                if wake.recv().is_err() {
                    return;
                }
                continue;
            }
            Err(e) => {
                log::error!("reading job queue failed: {e}");
                std::thread::sleep(Duration::from_secs(5));
                continue;
            }
        };
        let options: JobOptions = job
            .options
            .as_deref()
            .and_then(|o| serde_json::from_str(o).ok())
            .unwrap_or_default();
        run_job(&app, job.id, job.meeting_id, &options, &current);
        *current.lock().unwrap() = None;
        let _ = app.emit(EVENT_CHANGED, job.meeting_id);
    }
}

fn configured_backend(db: &Db) -> Backend {
    match db
        .get_setting("transcription.backend")
        .ok()
        .flatten()
        .as_deref()
    {
        Some("cpu") => Backend::Cpu,
        Some("gpu") => Backend::Gpu,
        _ => Backend::Auto,
    }
}

enum WorkerOutcome {
    Done(Transcript),
    /// The worker reported an error.
    Failed {
        code: ErrorCode,
        message: String,
        suggestion: Option<String>,
    },
    /// The worker exited without a result (crashed or killed).
    Died(String),
}

fn run_job(
    app: &AppHandle,
    job_id: i64,
    meeting_id: i64,
    options: &JobOptions,
    current: &Arc<Mutex<Option<Current>>>,
) {
    let db = app.state::<Db>();
    let fail = |msg: &str| {
        let _ = db.update_job(job_id, "failed", None, 0.0, Some(msg));
        let _ = db.set_meeting_status(meeting_id, "failed", None, Some(msg));
        log::error!("processing meeting {meeting_id} failed: {msg}");
    };

    let Some(model) = models::selected(app) else {
        fail("Choose and download a transcription model in Settings → Transcription.");
        return;
    };
    let vad = models::vad_model();
    let vad_path = models::is_ready(app, vad)
        .then(|| models::path_of(app, vad).ok())
        .flatten()
        .map(|p| p.to_string_lossy().into_owned());
    let model_path = match models::path_of(app, model) {
        Ok(p) => p.to_string_lossy().into_owned(),
        Err(e) => return fail(&e),
    };
    let language = db
        .get_setting("transcription.language")
        .ok()
        .flatten()
        .filter(|l| !l.is_empty() && l != "auto");

    let tracks: Vec<_> = match db.tracks(meeting_id) {
        Ok(t) => t.into_iter().filter(|t| t.finalized).collect(),
        Err(e) => return fail(&e.to_string()),
    };
    if tracks.is_empty() {
        return fail("This meeting has no audio to transcribe.");
    }

    *current.lock().unwrap() = Some(Current {
        meeting_id,
        stdin: None,
        child: None,
        cancelled: false,
    });
    let _ = db.update_job(job_id, "running", Some("decode"), 0.0, None);
    let _ = db.set_meeting_status(meeting_id, "processing", None, None);
    let _ = app.emit(EVENT_CHANGED, meeting_id);
    let started = Instant::now();
    let mut backend = options.backend.unwrap_or_else(|| configured_backend(&db));
    let mut notes = Vec::new();
    let mut language_found = None;

    for (i, track) in tracks.iter().enumerate() {
        let mut attempt = 0;
        let transcript = loop {
            attempt += 1;
            let job = TranscribeJob {
                audio_path: track.path.clone(),
                model_path: model_path.clone(),
                vad_model_path: vad_path.clone(),
                language: language.clone(),
                backend,
                dtw_preset: model.dtw_preset.clone(),
            };
            let outcome = run_worker(
                app,
                job_id,
                meeting_id,
                i,
                tracks.len(),
                &track.kind,
                job,
                current,
            );
            if current
                .lock()
                .unwrap()
                .as_ref()
                .is_some_and(|c| c.cancelled)
            {
                let _ = db.update_job(job_id, "cancelled", None, 0.0, None);
                let _ = db.set_meeting_status(meeting_id, "queued", None, None);
                log::info!("processing meeting {meeting_id} cancelled");
                return;
            }
            match outcome {
                WorkerOutcome::Done(t) => break t,
                WorkerOutcome::Failed {
                    code: ErrorCode::GpuFailed,
                    message,
                    ..
                } if backend != Backend::Cpu && attempt == 1 => {
                    log::warn!("GPU transcription failed ({message}); retrying on the CPU");
                    notes.push(
                        "The GPU failed, so this meeting was transcribed on the CPU.".to_owned(),
                    );
                    backend = Backend::Cpu;
                }
                WorkerOutcome::Failed {
                    message,
                    suggestion,
                    ..
                } => {
                    let msg = match suggestion {
                        Some(s) => format!("{message} {s}"),
                        None => message,
                    };
                    return fail(&msg);
                }
                WorkerOutcome::Died(detail) => {
                    log::error!("worker died: {detail}");
                    return fail(
                        "The transcription engine stopped unexpectedly. Retry, or switch the compute \
                         backend to CPU in Settings → Transcription if it keeps happening.",
                    );
                }
            }
        };
        log::info!(
            "transcribed {} track of meeting {meeting_id}: {:.1} s audio in {:.1} s on {}",
            track.kind,
            transcript.audio_ms as f64 / 1000.0,
            transcript.elapsed_ms as f64 / 1000.0,
            transcript.backend_used
        );
        if track.kind == "system" || language_found.is_none() {
            language_found = Some(transcript.language.clone());
        }
        let utterances = group_utterances(&transcript);
        if let Err(e) = db.replace_track_transcript(meeting_id, &track.kind, &utterances) {
            return fail(&format!("saving the transcript failed: {e}"));
        }
    }

    if let Some(lang) = language_found {
        let _ = db.set_meeting_language(meeting_id, &lang);
    }
    let note = (!notes.is_empty()).then(|| notes.join(" "));
    let _ = db.update_job(job_id, "done", None, 100.0, note.as_deref());
    let _ = db.set_meeting_status(meeting_id, "ready", None, note.as_deref());
    log::info!(
        "meeting {meeting_id} processed in {:.1} s",
        started.elapsed().as_secs_f64()
    );
}

/// Runs one transcription in a fresh worker process and relays its progress.
#[allow(clippy::too_many_arguments)]
fn run_worker(
    app: &AppHandle,
    job_id: i64,
    meeting_id: i64,
    track_index: usize,
    track_count: usize,
    track_kind: &str,
    job: TranscribeJob,
    current: &Arc<Mutex<Option<Current>>>,
) -> WorkerOutcome {
    let mut cmd = match crate::worker::command() {
        Ok(c) => c,
        Err(e) => return WorkerOutcome::Died(e),
    };
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return WorkerOutcome::Died(format!("failed to start worker: {e}")),
    };
    let stdout = child.stdout.take().expect("piped");
    let stderr = child.stderr.take().expect("piped");
    let mut stdin = child.stdin.take().expect("piped");

    // Worker diagnostics go to the app log (errors/warnings only; whisper is chatty).
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            if line.starts_with("[ERROR]")
                || line.starts_with("[WARN]")
                || line.contains("panicked")
            {
                log::warn!("worker: {line}");
            } else {
                log::debug!("worker: {line}");
            }
        }
    });

    let request = serde_json::to_string(&Request::Transcribe { id: 1, job }).expect("serializes");
    if let Err(e) = writeln!(stdin, "{request}").and_then(|_| stdin.flush()) {
        let _ = child.kill();
        return WorkerOutcome::Died(format!("failed to send job: {e}"));
    }
    {
        let mut cur = current.lock().unwrap();
        if let Some(c) = cur.as_mut() {
            c.stdin = Some(stdin);
            c.child = Some(child);
        }
    }

    let db = app.state::<Db>();
    let mut outcome = None;
    let mut last_db_update = Instant::now();
    for line in BufReader::new(stdout).lines().map_while(Result::ok) {
        let Ok(ev) = serde_json::from_str::<Event>(&line) else {
            continue;
        };
        match ev {
            Event::Progress { stage, pct, .. } => {
                // Weight stages within a track, then tracks within the meeting.
                let (base, span) = match stage {
                    Stage::Decode => (0.0, 5.0),
                    Stage::Vad => (5.0, 5.0),
                    Stage::LoadModel => (10.0, 5.0),
                    Stage::Transcribe => (15.0, 85.0),
                };
                let track_pct = base + span * (pct / 100.0);
                let overall = (track_index as f32 * 100.0 + track_pct) / track_count as f32;
                let _ = app.emit(
                    EVENT_PROGRESS,
                    Progress {
                        meeting_id,
                        track: track_index,
                        tracks: track_count,
                        track_kind: track_kind.into(),
                        stage,
                        pct: overall,
                    },
                );
                if last_db_update.elapsed() >= Duration::from_secs(2) {
                    last_db_update = Instant::now();
                    let stage_name = serde_json::to_value(stage)
                        .ok()
                        .and_then(|v| v.as_str().map(String::from));
                    let _ = db.update_job(
                        job_id,
                        "running",
                        stage_name.as_deref(),
                        f64::from(overall),
                        None,
                    );
                }
            }
            Event::Result { transcript, .. } => {
                outcome = Some(WorkerOutcome::Done(transcript));
                break;
            }
            Event::Error {
                code,
                message,
                suggestion,
                ..
            } => {
                outcome = Some(WorkerOutcome::Failed {
                    code,
                    message,
                    suggestion,
                });
                break;
            }
            Event::Pong { .. } => {}
        }
    }

    // Close stdin (the worker exits on EOF) and reap the process.
    let (stdin, child) = {
        let mut cur = current.lock().unwrap();
        cur.as_mut()
            .map(|c| (c.stdin.take(), c.child.take()))
            .unwrap_or((None, None))
    };
    drop(stdin);
    if let Some(mut child) = child {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(50))
                }
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    break;
                }
            }
        }
    }
    outcome.unwrap_or_else(|| WorkerOutcome::Died("worker exited without a result".into()))
}

/// Splits a track's words into utterances at pauses longer than [`UTTERANCE_PAUSE_MS`].
pub fn group_utterances(t: &Transcript) -> Vec<NewUtterance> {
    let mut out: Vec<NewUtterance> = Vec::new();
    let mut prev_end: Option<u64> = None;
    for w in t.segments.iter().flat_map(|s| s.words.iter()) {
        if w.text.trim().is_empty() {
            continue;
        }
        let new = match (prev_end, out.last()) {
            (Some(end), Some(_)) => w.start_ms.saturating_sub(end) > UTTERANCE_PAUSE_MS,
            _ => true,
        };
        if new {
            out.push(NewUtterance {
                start_ms: w.start_ms as i64,
                ..Default::default()
            });
        }
        let u = out.last_mut().expect("pushed");
        u.end_ms = w.end_ms as i64;
        u.text.push_str(&w.text);
        u.words.push(NewWord {
            start_ms: w.start_ms as i64,
            end_ms: w.end_ms as i64,
            text: w.text.clone(),
            prob: w.prob,
        });
        prev_end = Some(w.end_ms);
    }
    for u in &mut out {
        u.text = u.text.trim().to_owned();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use earshot_protocol::{Segment, Word};

    fn word(text: &str, s: u64, e: u64) -> Word {
        Word {
            start_ms: s,
            end_ms: e,
            text: text.into(),
            prob: 0.9,
        }
    }

    #[test]
    fn utterances_split_on_pauses_not_segments() {
        let t = Transcript {
            language: "en".into(),
            backend_used: "cpu".into(),
            audio_ms: 30_000,
            elapsed_ms: 1,
            segments: vec![
                Segment {
                    start_ms: 0,
                    end_ms: 9000,
                    text: String::new(),
                    words: vec![word(" Hello", 0, 400), word(" there.", 400, 900)],
                },
                // New segment but no pause: same utterance. Then a 10 s pause: new utterance.
                Segment {
                    start_ms: 900,
                    end_ms: 21_000,
                    text: String::new(),
                    words: vec![
                        word(" How", 1000, 1200),
                        word(" are", 1200, 1400),
                        word(" you?", 11_400, 11_800),
                    ],
                },
            ],
        };
        let u = group_utterances(&t);
        assert_eq!(
            u.iter().map(|u| u.text.as_str()).collect::<Vec<_>>(),
            ["Hello there. How are", "you?"]
        );
        assert_eq!(
            (u[0].start_ms, u[0].end_ms, u[1].start_ms),
            (0, 1400, 11_400)
        );
        assert_eq!(u[0].words.len(), 4);
    }
}
