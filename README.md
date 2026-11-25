# Earshot

A fully local meeting recorder and transcriber for Windows and macOS. Records your microphone and
the meeting audio as separate tracks, transcribes them with Whisper, labels speakers with
open-source diarization, and can summarize with a local model. Nothing leaves the machine.

> Status: early development (scaffold only).

## Development

Requirements:

- Node.js 24+
- Rust (the toolchain pinned in `rust-toolchain.toml` is installed automatically by rustup)
- Windows: Visual Studio Build Tools with the C++ workload, WebView2 (preinstalled on Windows 11)
- macOS: Xcode Command Line Tools

```sh
npm ci
npm run tauri dev      # run the app with hot reload
npm run tauri build    # build installers into target/release/bundle/
cargo test --workspace # Rust tests
```

## Layout

| Path | Contents |
|---|---|
| `src/` | React + TypeScript frontend |
| `src-tauri/` | Tauri app (Rust core: storage, tray, recorder, job supervisor) |
| `src-tauri/migrations/` | SQLite schema migrations |
| `crates/earshot-worker/` | Inference sidecar (all ML runs in this separate process) |
| `scripts/` | Build helpers |

## License

MIT
