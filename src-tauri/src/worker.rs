//! Locating and pinging the sidecars: `earshot-worker` (transcription, speakers) and `earshot-llm`
//! (summaries). All ML inference runs there, never in this process. Job execution lives in
//! `processing.rs` and `summary.rs`.

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use earshot_protocol::{Event, Request};

const PING_TIMEOUT: Duration = Duration::from_secs(5);

/// Tauri places `externalBin` sidecars next to the main executable (without the target-triple
/// suffix). In development, Cargo builds the worker into the same `target/<profile>` directory.
pub fn sidecar_path(name: &str) -> Option<PathBuf> {
    let dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
    let path = dir.join(format!("{name}{}", std::env::consts::EXE_SUFFIX));
    path.is_file().then_some(path)
}

pub const WORKER: &str = "earshot-worker";
pub const LLM: &str = "earshot-llm";

/// The transcription worker, with no console window on Windows.
pub fn command() -> Result<Command, String> {
    command_for(WORKER)
}

/// A sidecar command with no console window on Windows.
pub fn command_for(name: &str) -> Result<Command, String> {
    let path = sidecar_path(name).ok_or_else(|| format!("{name} binary not found"))?;
    #[allow(unused_mut)]
    let mut cmd = Command::new(path);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    Ok(cmd)
}

#[derive(Debug, Clone)]
pub struct WorkerInfo {
    pub version: String,
    pub gpu_backend: Option<String>,
}

/// Spawns the worker, sends a ping and returns what it reports.
pub fn ping() -> Result<WorkerInfo, String> {
    let mut cmd = command()?;
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("failed to start worker: {e}"))?;

    let mut stdin = child.stdin.take().ok_or("worker stdin unavailable")?;
    let stdout = child.stdout.take().ok_or("worker stdout unavailable")?;
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        let res = BufReader::new(stdout).read_line(&mut line).map(|_| line);
        let _ = tx.send(res);
    });

    let request = serde_json::to_string(&Request::Ping { id: 1 }).expect("serializes");
    let sent = writeln!(stdin, "{request}").and_then(|_| stdin.flush());
    drop(stdin); // EOF tells the worker to exit after replying.

    let result = match sent {
        Err(e) => Err(format!("failed to write to worker: {e}")),
        Ok(()) => match rx.recv_timeout(PING_TIMEOUT) {
            Ok(Ok(line)) => parse_pong(&line),
            Ok(Err(e)) => Err(format!("failed to read from worker: {e}")),
            Err(_) => Err("worker did not respond".into()),
        },
    };
    let _ = child.kill();
    let _ = child.wait();
    result
}

fn parse_pong(line: &str) -> Result<WorkerInfo, String> {
    match serde_json::from_str::<Event>(line.trim()) {
        Ok(Event::Pong {
            id: 1,
            version,
            gpu_backend,
        }) => Ok(WorkerInfo {
            version,
            gpu_backend,
        }),
        Ok(other) => Err(format!("unexpected worker reply: {other:?}")),
        Err(e) => Err(format!("invalid worker reply: {e}")),
    }
}

/// Cached worker info (the GPU backend never changes while the app runs).
pub fn info() -> Option<WorkerInfo> {
    static INFO: std::sync::OnceLock<Option<WorkerInfo>> = std::sync::OnceLock::new();
    INFO.get_or_init(|| {
        ping()
            .inspect_err(|e| log::warn!("worker ping failed: {e}"))
            .ok()
    })
    .clone()
}

#[cfg(test)]
mod tests {
    use super::parse_pong;

    #[test]
    fn parses_pong() {
        let i =
            parse_pong(r#"{"type":"pong","id":1,"version":"0.1.0","gpu_backend":"cuda"}"#).unwrap();
        assert_eq!(
            (i.version.as_str(), i.gpu_backend.as_deref()),
            ("0.1.0", Some("cuda"))
        );
        assert!(
            parse_pong(
                r#"{"type":"error","id":1,"code":"bad_request","message":"x","suggestion":null}"#
            )
            .is_err()
        );
        assert!(parse_pong("not json").is_err());
    }
}
