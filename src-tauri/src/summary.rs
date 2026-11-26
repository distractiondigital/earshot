//! Meeting summaries and titles, written by a local Qwen model in the `earshot-llm` sidecar.
//! Summary jobs share the processing queue with transcription (one model on the GPU at a time)
//! but never change the meeting's status: the transcript stays usable while a summary is written.

use std::io::{BufRead, BufReader, Write};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use earshot_protocol::{
    Backend, ErrorCode, Event, Request, Stage, SummarizeJob, Summary, SummaryTemplate,
};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager};

use crate::db::{Db, TemplateRow, UtteranceRow};
use crate::models;
use crate::processing::Current;

pub const JOB_KIND: &str = "summarize";
/// A piece of the summary being written: `{ meetingId, text }`.
pub const EVENT_TOKEN: &str = "summary://token";
/// `{ meetingId, stage, pct }`.
pub const EVENT_PROGRESS: &str = "summary://progress";
/// A meeting's summary, title or summary job changed (payload: meeting id).
pub const EVENT_CHANGED: &str = "summary://changed";

/// "never", "manual" (on demand) or "auto" (after processing).
pub const MODE_SETTING: &str = "summary.mode";
/// Title untitled meetings ("true"/"false").
pub const AUTO_TITLE_SETTING: &str = "summary.autoTitle";
/// Template id used when none is picked.
pub const TEMPLATE_SETTING: &str = "summary.template";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SummaryOptions {
    /// Write a summary (otherwise only a title).
    pub summary: bool,
    /// Title the meeting if it has no title of the user's own.
    pub title: bool,
    pub template_id: Option<i64>,
    /// Use this model instead of the selected one.
    pub model: Option<String>,
    /// Force a backend ("cpu" after a GPU problem).
    pub backend: Option<Backend>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Never,
    Manual,
    Auto,
}

pub fn mode(db: &Db) -> Mode {
    match db.get_setting(MODE_SETTING).ok().flatten().as_deref() {
        Some("never") => Mode::Never,
        Some("manual") => Mode::Manual,
        // Automatic by default: nothing happens until a summary model is downloaded.
        _ => Mode::Auto,
    }
}

fn auto_title(db: &Db) -> bool {
    db.get_setting(AUTO_TITLE_SETTING).ok().flatten().as_deref() != Some("false")
}

/// The template to use: the one asked for, else the default setting, else the first.
fn template(db: &Db, id: Option<i64>) -> Option<TemplateRow> {
    let default = db
        .get_setting(TEMPLATE_SETTING)
        .ok()
        .flatten()
        .and_then(|v| v.parse().ok());
    [id, default]
        .into_iter()
        .flatten()
        .find_map(|id| db.template(id).ok().flatten())
        .or_else(|| db.templates().ok()?.into_iter().next())
}

/// Queues a summary and/or title for a meeting.
pub fn enqueue(app: &AppHandle, meeting_id: i64, options: &SummaryOptions) -> Result<(), String> {
    let opts = serde_json::to_string(options).ok();
    app.state::<Db>()
        .enqueue_job(meeting_id, JOB_KIND, opts.as_deref())
        .map_err(|e| e.to_string())?;
    app.state::<crate::processing::Processor>().notify();
    let _ = app.emit(EVENT_CHANGED, meeting_id);
    Ok(())
}

/// After a meeting is transcribed: summarize it if summaries are automatic, or at least title it.
pub fn after_processing(app: &AppHandle, meeting_id: i64, speakers_only: bool) {
    let db = app.state::<Db>();
    if models::summary_model(app).is_none() {
        return;
    }
    let existing = db.summary(meeting_id).ok().flatten();
    // A re-run of speaker separation only renames people; keep the summary as it is.
    let summarize = mode(&db) == Mode::Auto
        && !(speakers_only && existing.is_some())
        // Never overwrite the user's own edits automatically.
        && !existing.as_ref().is_some_and(|s| s.edited);
    let title = mode(&db) != Mode::Never
        && auto_title(&db)
        && db.title_is_open(meeting_id).unwrap_or(false);
    if !summarize && !title {
        return;
    }
    let options = SummaryOptions {
        summary: summarize,
        title,
        ..Default::default()
    };
    if let Err(e) = enqueue(app, meeting_id, &options) {
        log::error!("could not queue a summary of meeting {meeting_id}: {e}");
    }
}

/// `[mm:ss] Name: text`, one line per utterance.
pub fn transcript_text(lines: &[UtteranceRow], me_name: &str) -> String {
    lines
        .iter()
        .filter(|u| !u.text.trim().is_empty())
        .map(|u| {
            // An unconfirmed voice match isn't a fact the summary should state: use "Speaker N".
            let name = match (&u.speaker_label, u.speaker_suggested) {
                (Some(label), true) => label.clone(),
                _ => crate::export::speaker_name(u, me_name),
            };
            format!(
                "[{}] {}: {}",
                crate::export::timestamp(u.start_ms),
                name,
                u.text.trim()
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn me_name(db: &Db) -> String {
    db.get_setting("speakers.meName")
        .ok()
        .flatten()
        .filter(|n| !n.trim().is_empty())
        .unwrap_or_else(|| "Me".to_owned())
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

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct Progress {
    meeting_id: i64,
    stage: Stage,
    pct: f32,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct Token<'a> {
    meeting_id: i64,
    text: &'a str,
}

enum Outcome {
    Done(Summary),
    Failed { code: ErrorCode, message: String },
    Died(String),
}

pub(crate) fn run_job(
    app: &AppHandle,
    job_id: i64,
    meeting_id: i64,
    options: &SummaryOptions,
    current: &Arc<Mutex<Option<Current>>>,
) {
    let db = app.state::<Db>();
    let fail = |msg: &str| {
        let _ = db.update_job(job_id, "failed", None, 0.0, Some(msg));
        log::error!("summary of meeting {meeting_id} failed: {msg}");
    };

    let model = match &options.model {
        Some(id) => {
            models::find(id).filter(|m| m.category == "summary" && models::is_ready(app, m))
        }
        None => models::summary_model(app),
    };
    let Some(model) = model else {
        return fail("Choose and download a summary model in Settings → Summary.");
    };
    let model_path = match models::path_of(app, model) {
        Ok(p) => p.to_string_lossy().into_owned(),
        Err(e) => return fail(&e),
    };
    let template = if options.summary {
        match template(&db, options.template_id) {
            Some(t) => Some(t),
            None => return fail("There's no summary template. Add one in Settings → Summary."),
        }
    } else {
        None
    };
    let title = options.title && db.title_is_open(meeting_id).unwrap_or(false);
    if template.is_none() && !title {
        let _ = db.update_job(job_id, "done", None, 100.0, None);
        return;
    }
    let lines = match db.transcript(meeting_id) {
        Ok(l) => l,
        Err(e) => return fail(&e.to_string()),
    };
    let transcript = transcript_text(&lines, &me_name(&db));
    if transcript.is_empty() {
        return fail("This meeting has no transcript to summarize yet.");
    }

    *current.lock().unwrap() = Some(Current {
        meeting_id,
        kind: JOB_KIND,
        stdin: None,
        child: None,
        cancelled: false,
    });
    let _ = db.update_job(job_id, "running", Some("load_model"), 0.0, None);
    let _ = app.emit(EVENT_CHANGED, meeting_id);

    let mut backend = options.backend.unwrap_or_else(|| configured_backend(&db));
    let mut note = None;
    let mut attempt = 0;
    let summary = loop {
        attempt += 1;
        let job = SummarizeJob {
            model_path: model_path.clone(),
            backend,
            context_tokens: model.context_tokens.unwrap_or(16_384),
            transcript: transcript.clone(),
            template: template.as_ref().map(|t| SummaryTemplate {
                name: t.name.clone(),
                instructions: t.instructions.clone(),
                sections: t.sections.clone(),
            }),
            title,
        };
        let outcome = run_llm(app, job_id, meeting_id, job, current);
        if current
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|c| c.cancelled)
        {
            let _ = db.update_job(job_id, "cancelled", None, 0.0, None);
            log::info!("summary of meeting {meeting_id} cancelled");
            return;
        }
        match outcome {
            Outcome::Done(s) => break s,
            Outcome::Failed {
                code: ErrorCode::GpuFailed,
                message,
            } if backend != Backend::Cpu && attempt == 1 => {
                log::warn!("GPU summary failed ({message}); retrying on the CPU");
                note = Some("The GPU couldn't run the summary model, so the CPU was used.");
                backend = Backend::Cpu;
            }
            Outcome::Failed { message, .. } => {
                return fail(&format!("The summary couldn't be written: {message}"));
            }
            Outcome::Died(detail) => {
                log::error!("summary engine died: {detail}");
                return fail(
                    "The summary engine stopped unexpectedly. Retry, or pick a smaller model in \
                     Settings → Summary if it keeps happening.",
                );
            }
        }
    };
    log::info!(
        "summarized meeting {meeting_id} in {:.1} s on {} ({} part(s))",
        summary.elapsed_ms as f64 / 1000.0,
        summary.backend_used,
        summary.parts
    );
    if let (Some(t), Some(md)) = (&template, &summary.markdown)
        && let Err(e) = db.save_summary(meeting_id, t, &model.id, md)
    {
        return fail(&format!("saving the summary failed: {e}"));
    }
    if let Some(t) = &summary.title {
        match db.set_generated_title(meeting_id, t) {
            Ok(true) => log::info!("titled meeting {meeting_id}"),
            Ok(false) => {}
            Err(e) => log::warn!("saving the title failed: {e}"),
        }
    }
    if template.is_some() && summary.markdown.is_none() {
        return fail("The model didn't write anything. Try again or pick another model.");
    }
    let _ = db.update_job(job_id, "done", None, 100.0, note);
}

/// Runs one request in a fresh `earshot-llm` process, relaying progress and streamed text.
fn run_llm(
    app: &AppHandle,
    job_id: i64,
    meeting_id: i64,
    job: SummarizeJob,
    current: &Arc<Mutex<Option<Current>>>,
) -> Outcome {
    let mut cmd = match crate::worker::command_for(crate::worker::LLM) {
        Ok(c) => c,
        Err(e) => return Outcome::Died(e),
    };
    if job.backend == Backend::Cpu {
        // Hide CUDA devices entirely, so a faulty GPU or driver can't affect a CPU run.
        cmd.env("CUDA_VISIBLE_DEVICES", "-1");
        cmd.env("GGML_VK_VISIBLE_DEVICES", "none");
    }
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return Outcome::Died(format!("failed to start the summary engine: {e}")),
    };
    let stdout = child.stdout.take().expect("piped");
    let stderr = child.stderr.take().expect("piped");
    let mut stdin = child.stdin.take().expect("piped");
    // llama.cpp is chatty: keep its output at debug level, except errors.
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            if line.contains("error") || line.contains("panicked") {
                log::warn!("llm: {line}");
            } else {
                log::debug!("llm: {line}");
            }
        }
    });

    let request = serde_json::to_string(&Request::Summarize { id: 1, job }).expect("serializes");
    if let Err(e) = writeln!(stdin, "{request}").and_then(|_| stdin.flush()) {
        let _ = child.kill();
        return Outcome::Died(format!("failed to send job: {e}"));
    }
    if let Some(c) = current.lock().unwrap().as_mut() {
        c.stdin = Some(stdin);
        c.child = Some(child);
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
                let _ = app.emit(
                    EVENT_PROGRESS,
                    Progress {
                        meeting_id,
                        stage,
                        pct,
                    },
                );
                if last_db_update.elapsed() >= Duration::from_secs(1) || pct == 0.0 {
                    last_db_update = Instant::now();
                    let stage_name = serde_json::to_value(stage)
                        .ok()
                        .and_then(|v| v.as_str().map(String::from));
                    let _ = db.update_job(
                        job_id,
                        "running",
                        stage_name.as_deref(),
                        f64::from(pct),
                        None,
                    );
                }
            }
            Event::Token { text, .. } => {
                let _ = app.emit(
                    EVENT_TOKEN,
                    Token {
                        meeting_id,
                        text: &text,
                    },
                );
            }
            Event::Summary { summary, .. } => {
                outcome = Some(Outcome::Done(summary));
                break;
            }
            Event::Error { code, message, .. } => {
                outcome = Some(Outcome::Failed { code, message });
                break;
            }
            Event::Pong { .. } | Event::Result { .. } | Event::Diarization { .. } => {}
        }
    }

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
    outcome.unwrap_or_else(|| Outcome::Died("the summary engine exited without a result".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utt(
        kind: &str,
        label: Option<&str>,
        name: Option<&str>,
        me: bool,
        ms: i64,
        text: &str,
    ) -> UtteranceRow {
        UtteranceRow {
            id: 0,
            track_kind: kind.into(),
            speaker_id: None,
            speaker_label: label.map(Into::into),
            speaker_name: name.map(Into::into),
            speaker_suggested: false,
            is_me: me,
            edited: false,
            start_ms: ms,
            end_ms: ms + 1000,
            text: text.into(),
            words: vec![],
        }
    }

    #[test]
    fn transcript_lines_use_names() {
        let lines = [
            utt("mic", Some("Me"), None, true, 5_000, " Morning all. "),
            utt(
                "system",
                Some("Speaker 1"),
                Some("Dana"),
                false,
                65_000,
                "Hi.",
            ),
            utt(
                "system",
                Some("Speaker 2"),
                None,
                false,
                3_725_000,
                "Hello.",
            ),
            utt("system", Some("Speaker 2"), None, false, 3_726_000, "  "),
        ];
        assert_eq!(
            transcript_text(&lines, "Sam"),
            "[0:05] Sam: Morning all.\n[1:05] Dana: Hi.\n[1:02:05] Speaker 2: Hello."
        );
    }

    #[test]
    fn unconfirmed_voice_matches_stay_anonymous() {
        let mut suggested = utt("system", Some("Speaker 1"), Some("Dana"), false, 0, "Hi.");
        suggested.speaker_suggested = true;
        assert_eq!(
            transcript_text(&[suggested], "Sam"),
            "[0:00] Speaker 1: Hi."
        );
    }

    #[test]
    fn mode_defaults_to_automatic() {
        let db = Db::open_in_memory().unwrap();
        assert_eq!(mode(&db), Mode::Auto);
        db.set_setting(MODE_SETTING, "manual").unwrap();
        assert_eq!(mode(&db), Mode::Manual);
        db.set_setting(MODE_SETTING, "never").unwrap();
        assert_eq!(mode(&db), Mode::Never);
    }

    #[test]
    fn template_falls_back_to_the_first() {
        let db = Db::open_in_memory().unwrap();
        let all = db.templates().unwrap();
        assert_eq!(template(&db, None).unwrap().name, "General meeting");
        let last = all.last().unwrap();
        db.set_setting(TEMPLATE_SETTING, &last.id.to_string())
            .unwrap();
        assert_eq!(template(&db, None).unwrap().id, last.id);
        assert_eq!(template(&db, Some(all[1].id)).unwrap().id, all[1].id);
        // A deleted template falls back too.
        assert_eq!(template(&db, Some(9999)).unwrap().id, last.id);
    }
}
