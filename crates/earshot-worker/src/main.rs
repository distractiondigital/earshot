//! `earshot-worker`: the inference sidecar. Runs as a separate process so that a model crash
//! (OOM, driver fault) never takes down the main app or an in-progress recording.
//!
//! Protocol (see `earshot-protocol`): one JSON request per line on stdin, one JSON event per line
//! on stdout. Diagnostics go to stderr. The worker exits when stdin closes.

mod audio;
mod diarize;
mod transcribe;
mod vad;

use std::io::{self, BufRead, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};

use earshot_protocol::{ErrorCode, Event, Request, Stage};

/// Messages for the stdout writer thread.
enum Out {
    Event(Event),
    Shutdown,
}

impl From<Event> for Out {
    fn from(e: Event) -> Self {
        Out::Event(e)
    }
}

struct Running {
    id: u64,
    cancel: Arc<AtomicBool>,
}

fn error(id: u64, code: ErrorCode, message: impl Into<String>) -> Event {
    Event::Error {
        id,
        code,
        message: message.into(),
        suggestion: None,
    }
}

fn handle(
    line: &str,
    out: &Sender<Out>,
    running: &Arc<Mutex<Option<Running>>>,
) -> Option<std::thread::JoinHandle<()>> {
    let req = match serde_json::from_str::<Request>(line) {
        Ok(r) => r,
        Err(e) => {
            let _ = out.send(Out::from(error(0, ErrorCode::BadRequest, e.to_string())));
            return None;
        }
    };
    match req {
        Request::Ping { id } => {
            let _ = out.send(Out::from(Event::Pong {
                id,
                version: env!("CARGO_PKG_VERSION").into(),
                gpu_backend: transcribe::gpu_backend().map(Into::into),
            }));
            None
        }
        Request::Cancel { id } => {
            if let Some(r) = running.lock().unwrap().as_ref().filter(|r| r.id == id) {
                r.cancel.store(true, Ordering::Relaxed);
            }
            None
        }
        Request::Transcribe { id, job } => start(id, out, running, move |cancel, progress| {
            transcribe::run(&job, cancel, progress)
                .map(|transcript| Event::Result { id, transcript })
        }),
        Request::Diarize { id, job } => start(id, out, running, move |cancel, progress| {
            diarize::run(&job, cancel, progress)
                .map(|diarization| Event::Diarization { id, diarization })
        }),
        Request::Summarize { id, .. } => {
            let _ = out.send(Out::from(error(
                id,
                ErrorCode::BadRequest,
                "summaries run in earshot-llm",
            )));
            None
        }
    }
}

type Progress = Box<dyn FnMut(Stage, f32) + Send>;

/// Runs a job on its own thread; only one job runs at a time.
fn start(
    id: u64,
    out: &Sender<Out>,
    running: &Arc<Mutex<Option<Running>>>,
    work: impl FnOnce(&Arc<AtomicBool>, Progress) -> Result<Event, transcribe::JobError>
    + Send
    + 'static,
) -> Option<std::thread::JoinHandle<()>> {
    let mut slot = running.lock().unwrap();
    if slot.is_some() {
        let _ = out.send(Out::from(error(
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
        let progress_out = out.clone();
        let progress: Progress = Box::new(move |stage, pct| {
            let _ = progress_out.send(Out::from(Event::Progress { id, stage, pct }));
        });
        let result = work(&cancel, progress);
        *running.lock().unwrap() = None;
        let _ = out.send(Out::from(result.unwrap_or_else(|e| Event::Error {
            id,
            code: e.code,
            message: e.message,
            suggestion: e.suggestion,
        })));
    }))
}

fn main() -> io::Result<()> {
    if std::env::args().any(|a| a == "--version") {
        println!("earshot-worker {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    // whisper.cpp / ggml diagnostics -> log -> stderr (the app captures it into its log file).
    whisper_rs::install_logging_hooks();
    struct StderrLog;
    impl log::Log for StderrLog {
        fn enabled(&self, m: &log::Metadata) -> bool {
            m.level() <= log::Level::Info
        }
        fn log(&self, r: &log::Record) {
            if self.enabled(r.metadata()) {
                eprintln!("[{}] {}", r.level(), r.args());
            }
        }
        fn flush(&self) {}
    }
    let _ = log::set_logger(&StderrLog);
    log::set_max_level(log::LevelFilter::Info);

    // A single writer thread keeps events whole on stdout.
    let (tx, rx) = mpsc::channel::<Out>();
    let writer = std::thread::spawn(move || {
        let mut stdout = io::stdout().lock();
        // Ends on an explicit shutdown: a Sender clone may be leaked inside whisper's progress
        // callback, so waiting for every sender to drop would hang forever.
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
    // stdin closed: cancel any running job and exit.
    if let Some(r) = running.lock().unwrap().as_ref() {
        r.cancel.store(true, Ordering::Relaxed);
    }
    if let Some(j) = job {
        let _ = j.join();
    }
    let _ = tx.send(Out::Shutdown);
    let _ = writer.join();
    // All events are flushed. Exit without running library teardown: the CUDA backend can hang
    // in its process-exit cleanup after a job, leaving a worker process behind.
    std::process::exit(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ping_returns_pong() {
        let (tx, rx) = mpsc::channel();
        let running = Arc::new(Mutex::new(None));
        handle(r#"{"type":"ping","id":7}"#, &tx, &running);
        match rx.recv().unwrap() {
            Out::Event(Event::Pong { id, version, .. }) => {
                assert_eq!(id, 7);
                assert_eq!(version, env!("CARGO_PKG_VERSION"));
            }
            Out::Event(e) => panic!("{e:?}"),
            Out::Shutdown => panic!("shutdown"),
        }
    }

    #[test]
    fn bad_request_returns_error() {
        let (tx, rx) = mpsc::channel();
        handle(r#"{"type":"nope"}"#, &tx, &Arc::new(Mutex::new(None)));
        assert!(matches!(
            rx.recv().unwrap(),
            Out::Event(Event::Error {
                code: ErrorCode::BadRequest,
                ..
            })
        ));
    }
}
