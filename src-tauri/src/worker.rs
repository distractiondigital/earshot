//! Client side of the `earshot-worker` sidecar (all ML inference runs there, never in this process).
//!
//! IPC is newline-delimited JSON over the child's stdin/stdout. M0 only implements a ping
//! handshake to prove the sidecar is bundled and launchable; the job protocol arrives in M2.

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

const PING_TIMEOUT: Duration = Duration::from_secs(5);

/// Tauri places `externalBin` sidecars next to the main executable (without the target-triple
/// suffix). In development, Cargo builds the worker into the same `target/<profile>` directory.
fn worker_path() -> Option<PathBuf> {
    let dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
    let path = dir.join(format!("earshot-worker{}", std::env::consts::EXE_SUFFIX));
    path.is_file().then_some(path)
}

/// Spawns the worker, sends a ping and returns the version it reports.
pub fn ping() -> Result<String, String> {
    let path = worker_path().ok_or("earshot-worker binary not found")?;
    let mut cmd = Command::new(path);
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
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

    let request = serde_json::json!({ "type": "ping", "id": 1 });
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

fn parse_pong(line: &str) -> Result<String, String> {
    let v: serde_json::Value =
        serde_json::from_str(line.trim()).map_err(|e| format!("invalid worker reply: {e}"))?;
    match (v["type"].as_str(), v["id"].as_u64(), v["version"].as_str()) {
        (Some("pong"), Some(1), Some(version)) => Ok(version.to_owned()),
        _ => Err(format!("unexpected worker reply: {}", line.trim())),
    }
}

#[cfg(test)]
mod tests {
    use super::parse_pong;

    #[test]
    fn parses_pong() {
        assert_eq!(
            parse_pong(r#"{"type":"pong","id":1,"version":"0.1.0"}"#).unwrap(),
            "0.1.0"
        );
        assert!(parse_pong(r#"{"type":"error","id":1}"#).is_err());
        assert!(parse_pong("not json").is_err());
    }
}
