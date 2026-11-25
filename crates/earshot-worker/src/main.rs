//! `earshot-worker`: the inference sidecar. Runs as a separate process so that a model crash
//! (OOM, driver fault) never takes down the main app or an in-progress recording.
//!
//! Protocol: one JSON object per line on stdin (requests) and stdout (replies). The worker
//! exits when stdin closes. M0 implements only `ping`; job messages arrive in M2.

use std::io::{self, BufRead, Write};

use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Request {
    Ping { id: u64 },
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Reply<'a> {
    Pong {
        id: u64,
        version: &'a str,
    },
    Error {
        id: Option<u64>,
        code: &'a str,
        message: String,
    },
}

fn handle(line: &str) -> Reply<'static> {
    match serde_json::from_str::<Request>(line) {
        Ok(Request::Ping { id }) => Reply::Pong {
            id,
            version: env!("CARGO_PKG_VERSION"),
        },
        Err(e) => Reply::Error {
            id: None,
            code: "bad_request",
            message: e.to_string(),
        },
    }
}

fn main() -> io::Result<()> {
    if std::env::args().any(|a| a == "--version") {
        println!("earshot-worker {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    let stdin = io::stdin().lock();
    let mut stdout = io::stdout().lock();
    for line in stdin.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let reply = serde_json::to_string(&handle(&line)).expect("reply serializes");
        writeln!(stdout, "{reply}")?;
        stdout.flush()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ping_returns_pong() {
        let v = serde_json::to_value(handle(r#"{"type":"ping","id":7}"#)).unwrap();
        assert_eq!(v["type"], "pong");
        assert_eq!(v["id"], 7);
        assert_eq!(v["version"], env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn bad_request_returns_error() {
        let v = serde_json::to_value(handle(r#"{"type":"nope"}"#)).unwrap();
        assert_eq!(v["type"], "error");
        assert_eq!(v["code"], "bad_request");
    }
}
