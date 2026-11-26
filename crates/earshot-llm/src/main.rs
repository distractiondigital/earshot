//! `earshot-llm`: the summary sidecar (llama.cpp). Separate from `earshot-worker` because
//! whisper.cpp and llama.cpp each bundle their own ggml, which can't share one executable; and,
//! like the worker, so that a model crash never takes down the app.
//!
//! Protocol (see `earshot-protocol`): one JSON request per line on stdin, one JSON event per line
//! on stdout. Diagnostics go to stderr. Exits when stdin closes.

mod engine;
mod summarize;

use std::io::{self, BufRead, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use earshot_protocol::{Backend, ErrorCode, Event, Request, Stage, SummarizeJob, Summary};

enum Out {
    Event(Event),
    Shutdown,
}

struct Running {
    id: u64,
    cancel: Arc<AtomicBool>,
}

struct JobError {
    code: ErrorCode,
    message: String,
    suggestion: Option<String>,
}

impl JobError {
    fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            suggestion: None,
        }
    }
}

fn error(id: u64, code: ErrorCode, message: impl Into<String>) -> Event {
    Event::Error {
        id,
        code,
        message: message.into(),
        suggestion: None,
    }
}

fn run(
    job: &SummarizeJob,
    cancel: &AtomicBool,
    progress: &mut dyn FnMut(Stage, f32),
    token: &mut dyn FnMut(&str),
) -> Result<Summary, JobError> {
    let started = Instant::now();
    let gpu = match (job.backend, engine::gpu_backend()) {
        (Backend::Cpu, _) | (Backend::Auto, None) => false,
        (_, Some(_)) => true,
        (Backend::Gpu, None) => {
            return Err(JobError::new(
                ErrorCode::GpuFailed,
                "this build has no GPU support",
            ));
        }
    };
    progress(Stage::LoadModel, 0.0);
    let model = engine::load(&job.model_path, gpu).map_err(|e| {
        if gpu {
            JobError::new(
                ErrorCode::GpuFailed,
                format!("loading the model on the GPU failed: {e}"),
            )
        } else {
            JobError::new(
                ErrorCode::ModelUnreadable,
                format!("the summary model couldn't be loaded: {e}"),
            )
        }
    })?;
    progress(Stage::LoadModel, 100.0);
    // More threads than physical cores slows generation; 16 covers desktop CPUs.
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get().min(16)) as i32;
    let mut session = engine::Session::new(&model, job.context_tokens, threads, gpu, cancel)
        .map_err(|e| {
            let code = if gpu {
                ErrorCode::GpuFailed
            } else {
                ErrorCode::SummarizeFailed
            };
            JobError::new(code, e)
        })?;
    let out = summarize::run(
        &mut session,
        &job.transcript,
        job.template.as_ref(),
        job.title,
        progress,
        token,
        &|| cancel.load(Ordering::Relaxed),
    )
    .map_err(|e| {
        if cancel.load(Ordering::Relaxed) {
            JobError::new(ErrorCode::Cancelled, "cancelled")
        } else if gpu && (e.starts_with("reading") || e.starts_with("writing")) {
            JobError::new(ErrorCode::GpuFailed, e)
        } else {
            JobError::new(ErrorCode::SummarizeFailed, e)
        }
    })?;
    Ok(Summary {
        markdown: out.markdown,
        title: out.title,
        backend_used: if gpu {
            engine::gpu_backend().unwrap_or("cpu").into()
        } else {
            "cpu".into()
        },
        elapsed_ms: started.elapsed().as_millis() as u64,
        parts: out.parts,
    })
}

fn handle(
    line: &str,
    out: &Sender<Out>,
    running: &Arc<Mutex<Option<Running>>>,
) -> Option<std::thread::JoinHandle<()>> {
    let req = match serde_json::from_str::<Request>(line) {
        Ok(r) => r,
        Err(e) => {
            let _ = out.send(Out::Event(error(0, ErrorCode::BadRequest, e.to_string())));
            return None;
        }
    };
    match req {
        Request::Ping { id } => {
            let _ = out.send(Out::Event(Event::Pong {
                id,
                version: env!("CARGO_PKG_VERSION").into(),
                gpu_backend: engine::gpu_backend().map(Into::into),
            }));
            None
        }
        Request::Cancel { id } => {
            if let Some(r) = running.lock().unwrap().as_ref().filter(|r| r.id == id) {
                r.cancel.store(true, Ordering::Relaxed);
            }
            None
        }
        Request::Transcribe { id, .. } | Request::Diarize { id, .. } => {
            let _ = out.send(Out::Event(error(
                id,
                ErrorCode::BadRequest,
                "transcription runs in earshot-worker",
            )));
            None
        }
        Request::Summarize { id, job } => {
            let mut slot = running.lock().unwrap();
            if slot.is_some() {
                let _ = out.send(Out::Event(error(
                    id,
                    ErrorCode::BadRequest,
                    "a job is already running",
                )));
                return None;
            }
            let cancel = Arc::new(AtomicBool::new(false));
            *slot = Some(Running {
                id,
                cancel: cancel.clone(),
            });
            drop(slot);
            let (out, running) = (out.clone(), running.clone());
            Some(std::thread::spawn(move || {
                let (p, t) = (out.clone(), out.clone());
                let result = run(
                    &job,
                    &cancel,
                    &mut |stage, pct| {
                        let _ = p.send(Out::Event(Event::Progress { id, stage, pct }));
                    },
                    &mut |text| {
                        let _ = t.send(Out::Event(Event::Token {
                            id,
                            text: text.to_owned(),
                        }));
                    },
                );
                *running.lock().unwrap() = None;
                let ev = match result {
                    Ok(summary) => Event::Summary { id, summary },
                    Err(e) => Event::Error {
                        id,
                        code: e.code,
                        message: e.message,
                        suggestion: e.suggestion,
                    },
                };
                let _ = out.send(Out::Event(ev));
            }))
        }
    }
}

fn main() -> io::Result<()> {
    if std::env::args().any(|a| a == "--version") {
        println!("earshot-llm {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }

    let (tx, rx) = mpsc::channel::<Out>();
    let writer = std::thread::spawn(move || {
        let mut stdout = io::stdout().lock();
        while let Ok(Out::Event(ev)) = rx.recv() {
            let line = serde_json::to_string(&ev).expect("event serializes");
            if writeln!(stdout, "{line}")
                .and_then(|_| stdout.flush())
                .is_err()
            {
                break;
            }
        }
    });

    let running = Arc::new(Mutex::new(None));
    let mut job = None;
    for line in io::stdin().lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        if let Some(j) = handle(&line, &tx, &running) {
            job = Some(j);
        }
    }
    if let Some(r) = running.lock().unwrap().as_ref() {
        r.cancel.store(true, Ordering::Relaxed);
    }
    if let Some(j) = job {
        let _ = j.join();
    }
    let _ = tx.send(Out::Shutdown);
    let _ = writer.join();
    // Skip library teardown: GPU backends can hang in process-exit cleanup.
    std::process::exit(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ping_returns_pong() {
        let (tx, rx) = mpsc::channel();
        handle(
            r#"{"type":"ping","id":7}"#,
            &tx,
            &Arc::new(Mutex::new(None)),
        );
        assert!(matches!(
            rx.recv().unwrap(),
            Out::Event(Event::Pong { id: 7, .. })
        ));
    }

    #[test]
    fn transcription_requests_are_refused() {
        let (tx, rx) = mpsc::channel();
        handle(
            r#"{"type":"diarize","id":2,"job":{"audio_path":"a","segmentation_model_path":"b","embedding_model_path":"c","num_speakers":null,"threshold":0.5}}"#,
            &tx,
            &Arc::new(Mutex::new(None)),
        );
        assert!(matches!(
            rx.recv().unwrap(),
            Out::Event(Event::Error {
                code: ErrorCode::BadRequest,
                ..
            })
        ));
    }
}
