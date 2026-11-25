# Earshot

A fully local meeting recorder and transcriber for Windows and macOS. Records your microphone and
the meeting audio as separate tracks, transcribes them with Whisper, labels speakers with
open-source diarization, and can summarize with a local model. Nothing leaves the machine.

> Status: early development. Recording works (mic + meeting audio as separate crash-safe tracks); transcription is next.

## Development

Requirements:

- Node.js 24+
- Rust (the toolchain pinned in `rust-toolchain.toml` is installed automatically by rustup)
- Windows: Visual Studio Build Tools with the C++ workload, WebView2 (preinstalled on Windows 11)
- macOS: Xcode Command Line Tools

On Windows, double-click `dev.cmd` (or run `npm run tauri:dev`) to start "Earshot Dev" with hot
reload. It uses its own data folder and never updates itself.

```sh
npm ci
npm run tauri:dev      # run "Earshot Dev" with hot reload (separate data folder, no updater)
cargo test --workspace # Rust tests
```

## Releases and updates

Installed copies update themselves from this repository's GitHub releases (signed `latest.json`).
To ship a version:

```sh
npm run release -- 0.2.0   # bumps versions, commits, tags v0.2.0
git push --follow-tags     # CI builds, signs and publishes installers + latest.json
```

Building installers locally (`npm run tauri build`) requires the updater signing key in
`TAURI_SIGNING_PRIVATE_KEY`; pass `--config '{"bundle":{"createUpdaterArtifacts":false}}'` to skip it.

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
