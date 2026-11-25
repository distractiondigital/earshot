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

use earshot_protocol::{
    Backend, Diarization, DiarizeJob, ErrorCode, Event, Request, Stage, TranscribeJob, Transcript,
};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager};

use crate::db::{Db, NewSpeaker, NewUtterance, NewWord};
use crate::{models, speakers};

pub const EVENT_PROGRESS: &str = "processing://progress";
pub const EVENT_CHANGED: &str = "processing://changed";

const JOB_KIND: &str = "transcribe";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JobOptions {
    /// Force a backend for this run ("cpu" after a GPU problem).
    pub backend: Option<Backend>,
    /// Transcribe with this model instead of the selected one.
    #[serde(default)]
    pub model: Option<String>,
    /// Keep the transcript and only separate speakers again.
    #[serde(default)]
    pub speakers_only: bool,
    /// Known number of remote speakers for this run (overrides the meeting's hint).
    #[serde(default)]
    pub speaker_count: Option<u32>,
    /// Sensitivity 0–100 for this run (overrides the setting).
    #[serde(default)]
    pub sensitivity: Option<f32>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct Progress {
    meeting_id: i64,
    /// 0-based index of the step being processed (one per track, plus speaker separation),
    /// and how many there are.
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
    Transcribed(Transcript),
    Diarized(Diarization),
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

    let model = match &options.model {
        Some(id) => {
            models::find(id).filter(|m| m.category == "transcription" && models::is_ready(app, m))
        }
        None => models::selected(app),
    };
    let model = match model {
        Some(m) => m,
        None if options.speakers_only => models::vad_model(), // unused: nothing is transcribed
        None if options.model.is_some() => {
            return fail("That transcription model isn't downloaded any more. Pick another one.");
        }
        None => {
            return fail("Choose and download a transcription model in Settings → Transcription.");
        }
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

    // Speaker separation runs on the meeting track when its models are downloaded.
    let diarize_models = models::diarization_models(app).and_then(|(seg, emb)| {
        Some((
            models::path_of(app, seg).ok()?,
            models::path_of(app, emb).ok()?,
        ))
    });
    let system_track = tracks.iter().find(|t| t.kind == "system").cloned();
    if options.speakers_only {
        if system_track.is_none() {
            return fail("This meeting has no meeting audio to separate speakers in.");
        }
        if diarize_models.is_none() {
            models::download_companions(app);
            return fail("The speaker models are still downloading. Try again in a minute.");
        }
    }
    let transcribe_steps = if options.speakers_only {
        0
    } else {
        tracks.len()
    };
    let steps = transcribe_steps + usize::from(system_track.is_some() && diarize_models.is_some());
    // Names the user gave speakers, to carry over to this run's speakers.
    let old_names = db.named_speakers(meeting_id).unwrap_or_default();

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
    // Each track's words: freshly transcribed, or the stored ones when only separating speakers.
    let mut track_words: Vec<(String, Vec<NewWord>)> = if options.speakers_only {
        match db.track_words(meeting_id) {
            Ok(w) => w,
            Err(e) => return fail(&e.to_string()),
        }
    } else {
        Vec::new()
    };
    let was_cancelled = || {
        current
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|c| c.cancelled)
    };
    let on_cancel = || {
        let _ = db.update_job(job_id, "cancelled", None, 0.0, None);
        // A cancelled re-run of speaker separation leaves the existing transcript in place.
        let status = if options.speakers_only {
            "ready"
        } else {
            "queued"
        };
        let _ = db.set_meeting_status(meeting_id, status, None, None);
        log::info!("processing meeting {meeting_id} cancelled");
    };

    let to_transcribe = if options.speakers_only {
        &tracks[..0]
    } else {
        &tracks[..]
    };
    for (i, track) in to_transcribe.iter().enumerate() {
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
            let step = Step {
                index: i,
                count: steps,
                track_kind: &track.kind,
            };
            let outcome = run_worker(
                app,
                job_id,
                meeting_id,
                step,
                Request::Transcribe { id: 1, job },
                current,
            );
            if was_cancelled() {
                return on_cancel();
            }
            match outcome {
                WorkerOutcome::Transcribed(t) => break t,
                WorkerOutcome::Diarized(_) => {
                    return fail("The transcription engine sent an unexpected reply.");
                }
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
        // Save each track as soon as it's done, so a later failure still leaves a transcript.
        let words = speakers::words(&transcript);
        let plain: Vec<_> = speakers::group(&words, &vec![None; words.len()])
            .into_iter()
            .map(|(_, u)| u)
            .collect();
        if let Err(e) = db.replace_track_transcript(meeting_id, &track.kind, &plain) {
            return fail(&format!("saving the transcript failed: {e}"));
        }
        track_words.push((track.kind.clone(), words));
    }

    // Who spoke when, on the meeting track.
    let mut diarization = None;
    const NO_SPEAKERS: &str = "Speakers couldn't be told apart in this meeting.";
    if let (Some(track), Some((seg, emb))) = (&system_track, &diarize_models) {
        let hint = db
            .meeting(meeting_id)
            .ok()
            .flatten()
            .and_then(|m| m.remote_speaker_count_hint)
            .filter(|&n| n > 0);
        let job = DiarizeJob {
            audio_path: track.path.clone(),
            segmentation_model_path: seg.to_string_lossy().into_owned(),
            embedding_model_path: emb.to_string_lossy().into_owned(),
            num_speakers: options
                .speaker_count
                .filter(|&n| n > 0)
                .or(hint.map(|n| n as u32)),
            threshold: clustering_threshold(
                &db,
                models::embedding_choice(app),
                options.sensitivity,
            ),
        };
        let step = Step {
            index: transcribe_steps,
            count: steps,
            track_kind: &track.kind,
        };
        let outcome = run_worker(
            app,
            job_id,
            meeting_id,
            step,
            Request::Diarize { id: 1, job },
            current,
        );
        if was_cancelled() {
            return on_cancel();
        }
        match outcome {
            WorkerOutcome::Diarized(d) => {
                log::info!(
                    "separated {} speaker(s) in meeting {meeting_id}: {:.1} s audio in {:.1} s",
                    d.num_speakers,
                    d.audio_ms as f64 / 1000.0,
                    d.elapsed_ms as f64 / 1000.0
                );
                diarization = Some(d);
            }
            WorkerOutcome::Failed { message, .. } => {
                log::error!("speaker separation failed: {message}");
                notes.push(NO_SPEAKERS.to_owned());
            }
            WorkerOutcome::Died(detail) => {
                log::error!("worker died during speaker separation: {detail}");
                notes.push(NO_SPEAKERS.to_owned());
            }
            WorkerOutcome::Transcribed(_) => notes.push(NO_SPEAKERS.to_owned()),
        }
    } else if system_track.is_some() {
        models::download_companions(app);
        notes.push(
            "Speaker labels need the speaker models, which are downloading now. Use Transcribe \
             again when they're ready."
                .to_owned(),
        );
    }

    // Assemble the final transcript: "Me" for the mic, Speaker 1…N for the meeting track.
    let Assembled {
        speakers: speaker_rows,
        tracks: final_tracks,
        removed,
    } = assemble(&track_words, diarization.as_ref());
    let mut speaker_rows = speaker_rows;
    if !old_names.is_empty() {
        let spans: Vec<(Option<usize>, i64, i64)> = final_tracks
            .iter()
            .filter(|(k, _)| k == "system")
            .flat_map(|(_, us)| us.iter().map(|u| (u.speaker, u.start_ms, u.end_ms)))
            .collect();
        let names = speakers::carry_names(&old_names, &spans, speaker_rows.len());
        for (row, name) in speaker_rows.iter_mut().zip(names) {
            if !row.is_me {
                row.name = name;
            }
        }
    }
    if removed > 0 {
        log::info!("removed {removed} mic utterance(s) that echoed the meeting audio");
    }
    let refs: Vec<(&str, &[NewUtterance])> = final_tracks
        .iter()
        .map(|(k, u)| (k.as_str(), u.as_slice()))
        .collect();
    if let Err(e) = db.save_transcript(meeting_id, &speaker_rows, &refs) {
        return fail(&format!("saving the transcript failed: {e}"));
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

/// Default for Settings → Speakers → sensitivity (0–100).
pub const DEFAULT_SENSITIVITY: f32 = 50.0;

fn clustering_threshold(db: &Db, embedding: &models::CatalogModel, run: Option<f32>) -> f32 {
    let s = run
        .or_else(|| {
            db.get_setting("speakers.sensitivity")
                .ok()
                .flatten()
                .and_then(|v| v.parse::<f32>().ok())
        })
        .unwrap_or(DEFAULT_SENSITIVITY)
        .clamp(0.0, 100.0);
    sensitivity_to_threshold(s, embedding.cluster_threshold.unwrap_or(0.5))
}

/// How far the sensitivity slider moves the threshold either side of the model's default.
const SENSITIVITY_SPAN: f32 = 0.3;

/// Sensitivity 0–100 -> clustering distance threshold around the embedding model's tuned
/// default (at 50). Higher sensitivity lowers the threshold, splitting voices into more speakers.
pub fn sensitivity_to_threshold(s: f32, default: f32) -> f32 {
    (default + SENSITIVITY_SPAN * (50.0 - s) / 50.0).max(0.05)
}

/// Builds the speaker rows and per-track utterances: Speaker 1…N for the meeting track (in order
/// of first appearance), "Me" for the mic. Returns how many mic utterances were dropped as bleed.
struct Assembled {
    speakers: Vec<NewSpeaker>,
    /// (track kind, utterances)
    tracks: Vec<(String, Vec<NewUtterance>)>,
    /// Mic utterances dropped as bleed.
    removed: usize,
}

fn assemble(tracks: &[(String, Vec<NewWord>)], diarization: Option<&Diarization>) -> Assembled {
    let mut rows = Vec::new();
    let mut out = Vec::new();
    let system = tracks.iter().find(|(k, _)| k == "system");
    let system_words: &[NewWord] = system.map(|(_, w)| w.as_slice()).unwrap_or_default();

    let mut system_utts = Vec::new();
    if system.is_some() {
        let labels = match diarization {
            Some(d) if !d.turns.is_empty() => {
                let mut l = speakers::assign(system_words, &d.turns);
                speakers::smooth(&mut l);
                l
            }
            // Nothing to separate, or separation failed: speaker unknown.
            _ => vec![None; system_words.len()],
        };
        let mut grouped = speakers::group(system_words, &labels);
        let n = speakers::renumber(&mut grouped);
        system_utts = grouped
            .into_iter()
            .map(|(l, mut u)| {
                u.speaker = l.map(|s| s as usize);
                u
            })
            .collect();
        rows.extend((1..=n).map(|i| NewSpeaker {
            label: format!("Speaker {i}"),
            is_me: false,
            name: None,
        }));
    }

    let mut removed = 0;
    if let Some((_, words)) = tracks.iter().find(|(k, _)| k == "mic") {
        let me = rows.len();
        rows.push(NewSpeaker {
            label: "Me".into(),
            is_me: true,
            name: None,
        });
        let mut mic: Vec<NewUtterance> = speakers::group(words, &vec![None; words.len()])
            .into_iter()
            .map(|(_, mut u)| {
                u.speaker = Some(me);
                u
            })
            .collect();
        removed = speakers::drop_bleed(&mut mic, system_words);
        out.push(("mic".to_owned(), mic));
    }
    if system.is_some() {
        out.push(("system".to_owned(), system_utts));
    }
    Assembled {
        speakers: rows,
        tracks: out,
        removed,
    }
}

/// Which part of a meeting's processing a worker run is, for overall progress.
#[derive(Clone, Copy)]
struct Step<'a> {
    index: usize,
    count: usize,
    track_kind: &'a str,
}

/// Runs one request in a fresh worker process and relays its progress.
fn run_worker(
    app: &AppHandle,
    job_id: i64,
    meeting_id: i64,
    step: Step,
    request: Request,
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

    let request = serde_json::to_string(&request).expect("serializes");
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
                // Weight stages within a step, then steps within the meeting.
                let (base, span) = match stage {
                    Stage::Decode => (0.0, 5.0),
                    Stage::Vad => (5.0, 5.0),
                    Stage::LoadModel => (10.0, 5.0),
                    Stage::Transcribe => (15.0, 85.0),
                    Stage::Diarize => (5.0, 95.0),
                };
                let step_pct = base + span * (pct / 100.0);
                let overall = (step.index as f32 * 100.0 + step_pct) / step.count as f32;
                let _ = app.emit(
                    EVENT_PROGRESS,
                    Progress {
                        meeting_id,
                        track: step.index,
                        tracks: step.count,
                        track_kind: step.track_kind.into(),
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
                outcome = Some(WorkerOutcome::Transcribed(transcript));
                break;
            }
            Event::Diarization { diarization, .. } => {
                outcome = Some(WorkerOutcome::Diarized(diarization));
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

#[cfg(test)]
mod tests {
    use super::*;
    use earshot_protocol::{Segment, SpeakerTurn, Word};

    fn transcript(words: &[(&str, u64, u64)]) -> Transcript {
        Transcript {
            language: "en".into(),
            backend_used: "cpu".into(),
            audio_ms: 60_000,
            elapsed_ms: 1,
            segments: vec![Segment {
                start_ms: 0,
                end_ms: 60_000,
                text: String::new(),
                words: words
                    .iter()
                    .map(|(t, s, e)| Word {
                        start_ms: *s,
                        end_ms: *e,
                        text: format!(" {t}"),
                        prob: 0.9,
                    })
                    .collect(),
            }],
        }
    }

    #[test]
    fn assemble_labels_speakers_and_me() {
        let system = transcript(&[
            ("Hello", 0, 400),
            ("all.", 400, 800),
            ("Hi", 2000, 2300),
            ("Sam.", 2300, 2600),
        ]);
        let mic = transcript(&[("Morning.", 5000, 5600)]);
        let d = Diarization {
            audio_ms: 60_000,
            elapsed_ms: 1,
            num_speakers: 2,
            turns: vec![
                SpeakerTurn {
                    start_ms: 0,
                    end_ms: 1000,
                    speaker: 4,
                },
                SpeakerTurn {
                    start_ms: 1900,
                    end_ms: 2700,
                    speaker: 1,
                },
            ],
        };
        let Assembled {
            speakers: rows,
            tracks,
            removed,
        } = assemble(
            &[
                ("mic".into(), speakers::words(&mic)),
                ("system".into(), speakers::words(&system)),
            ],
            Some(&d),
        );
        assert_eq!(removed, 0);
        assert_eq!(
            rows.iter()
                .map(|r| (r.label.as_str(), r.is_me))
                .collect::<Vec<_>>(),
            [("Speaker 1", false), ("Speaker 2", false), ("Me", true)]
        );
        let (_, mic) = tracks.iter().find(|(k, _)| k == "mic").unwrap();
        let (_, sys) = tracks.iter().find(|(k, _)| k == "system").unwrap();
        assert_eq!(mic[0].speaker, Some(2));
        assert_eq!(
            sys.iter()
                .map(|u| (u.speaker, u.text.as_str()))
                .collect::<Vec<_>>(),
            [(Some(0), "Hello all."), (Some(1), "Hi Sam.")]
        );
    }

    #[test]
    fn without_diarization_the_meeting_track_has_no_speaker() {
        let system = transcript(&[("Hello", 0, 400)]);
        let Assembled {
            speakers: rows,
            tracks,
            ..
        } = assemble(&[("system".into(), speakers::words(&system))], None);
        assert!(rows.is_empty());
        assert_eq!(tracks[0].1[0].speaker, None);
    }

    /// End-to-end check with the real worker and models; not run by default.
    /// Env: EARSHOT_E2E_WORKER (worker exe), EARSHOT_E2E_DIR (mic.flac + system.flac),
    /// EARSHOT_E2E_MODELS (dir with the whisper, VAD, segmentation and embedding files),
    /// optional EARSHOT_E2E_THRESHOLD.
    #[test]
    #[ignore]
    fn end_to_end_with_real_worker() {
        use std::io::{BufRead, BufReader, Write};
        let env = |k: &str| std::env::var(k).unwrap_or_else(|_| panic!("set {k}"));
        let (worker, dir, models) = (
            env("EARSHOT_E2E_WORKER"),
            std::path::PathBuf::from(env("EARSHOT_E2E_DIR")),
            std::path::PathBuf::from(env("EARSHOT_E2E_MODELS")),
        );
        let path = |d: &std::path::Path, f: &str| d.join(f).to_string_lossy().into_owned();
        let run = |req: Request| -> Event {
            let mut child = std::process::Command::new(&worker)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .unwrap();
            let mut stdin = child.stdin.take().unwrap();
            writeln!(stdin, "{}", serde_json::to_string(&req).unwrap()).unwrap();
            let out = BufReader::new(child.stdout.take().unwrap());
            let ev = out
                .lines()
                .map_while(Result::ok)
                .filter_map(|l| serde_json::from_str::<Event>(&l).ok())
                .find(|e| !matches!(e, Event::Progress { .. }))
                .unwrap();
            drop(stdin);
            let _ = child.wait();
            ev
        };
        let mut transcripts = Vec::new();
        for kind in ["mic", "system"] {
            let ev = run(Request::Transcribe {
                id: 1,
                job: TranscribeJob {
                    audio_path: path(&dir, &format!("{kind}.flac")),
                    model_path: path(&models, "ggml-large-v3-turbo.bin"),
                    vad_model_path: Some(path(&models, "ggml-silero-v6.2.0.bin")),
                    language: Some("en".into()),
                    backend: Backend::Auto,
                    dtw_preset: Some("large-v3-turbo".into()),
                },
            });
            let Event::Result { transcript, .. } = ev else {
                panic!("{ev:?}")
            };
            transcripts.push((kind.to_owned(), speakers::words(&transcript)));
        }
        let threshold = std::env::var("EARSHOT_E2E_THRESHOLD")
            .ok()
            .and_then(|t| t.parse().ok())
            .unwrap_or(0.5);
        let ev = run(Request::Diarize {
            id: 1,
            job: DiarizeJob {
                audio_path: path(&dir, "system.flac"),
                segmentation_model_path: path(&models, "pyannote-segmentation-3.0.onnx"),
                embedding_model_path: path(
                    &models,
                    "3dspeaker_speech_campplus_sv_en_voxceleb_16k.onnx",
                ),
                num_speakers: None,
                threshold,
            },
        });
        let Event::Diarization { diarization, .. } = ev else {
            panic!("{ev:?}")
        };
        println!(
            "diarization: {} speakers in {} ms",
            diarization.num_speakers, diarization.elapsed_ms
        );
        let Assembled {
            speakers: rows,
            tracks,
            removed,
        } = assemble(&transcripts, Some(&diarization));
        println!("removed as bleed: {removed}");
        let mut all: Vec<(&NewUtterance, &str)> = tracks
            .iter()
            .flat_map(|(_, us)| us.iter())
            .map(|u| (u, u.speaker.map_or("?", |i| rows[i].label.as_str())))
            .collect();
        all.sort_by_key(|(u, _)| u.start_ms);
        for (u, who) in all {
            println!("[{:6.1}] {who:<10} {}", u.start_ms as f64 / 1000.0, u.text);
        }
    }

    #[test]
    fn sensitivity_maps_to_threshold() {
        assert!((sensitivity_to_threshold(50.0, 1.0) - 1.0).abs() < 1e-6);
        assert!((sensitivity_to_threshold(0.0, 1.0) - 1.3).abs() < 1e-6);
        assert!((sensitivity_to_threshold(100.0, 1.0) - 0.7).abs() < 1e-6);
        assert!(sensitivity_to_threshold(100.0, 0.2) > 0.0);
    }
}
