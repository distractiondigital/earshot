# Development

How to run Earshot from source, build installers and publish releases. For what the app does, see the [README](../README.md).

## Requirements and setup

Requirements:

- Node.js 24+
- Rust (the toolchain pinned in `rust-toolchain.toml` is installed automatically by rustup)
- CMake and LLVM/libclang (to build whisper.cpp and llama.cpp for the sidecars)
- Windows: Visual Studio Build Tools with the C++ workload, WebView2 (preinstalled on Windows 11)
- macOS: Xcode Command Line Tools
- Optional GPU backend on Windows: the NVIDIA CUDA Toolkit (13.x) or the Vulkan SDK. The build
  picks CUDA, then Vulkan, then CPU; `--gpu cuda|vulkan|cpu` (or `EARSHOT_GPU`) chooses one.

On Windows, double-click `dev.cmd` (or run `npm run tauri:dev`) to start "Earshot Dev" with hot
reload. It uses its own data folder and never updates itself.

The worker links sherpa-onnx (speaker labels) as a prebuilt library. `npm run build:worker`
downloads it from the sherpa-onnx GitHub release, checks its SHA-256 and extracts it under
`target/`; run it once before plain `cargo` commands (`cargo test`, `cargo clippy`) that build the
worker.

```sh
npm ci
npm run tauri:dev      # run "Earshot Dev" with hot reload (separate data folder, no updater)
npm run build:worker   # build both sidecars (tauri:dev does this too)
cargo test --workspace # Rust tests
```

## Releases and updates

Installed copies update themselves from this repository's GitHub releases (signed `latest.json`).
To ship a version:

```sh
npm run release -- 0.2.0   # bumps versions, commits, tags v0.2.0
git push --follow-tags     # CI builds, signs and publishes installers + latest.json
```

`.github/workflows/release.yml` builds the three installers, then publishes them with a
`latest.json` that has an entry per updater channel: `windows-x86_64` (standard),
`windows-x86_64-cuda` (the CUDA build is compiled with `EARSHOT_UPDATER_TARGET` set to it),
`darwin-aarch64` and `darwin-x86_64`. Running the workflow by hand builds the installers as
workflow artifacts without publishing anything.

Repository secrets:

| Secret | Needed for |
|---|---|
| `TAURI_SIGNING_PRIVATE_KEY` | Required for tagged releases: signs the updates |
| `WINDOWS_CERTIFICATE`, `WINDOWS_CERTIFICATE_PASSWORD` | Optional: Authenticode signing (base64 .pfx) |
| `APPLE_CERTIFICATE`, `APPLE_CERTIFICATE_PASSWORD`, `APPLE_SIGNING_IDENTITY` | Optional: Developer ID signing (base64 .p12) |
| `APPLE_ID`, `APPLE_PASSWORD`, `APPLE_TEAM_ID` | Optional: notarization (app-specific password) |

Without the Windows secrets the installers are unsigned; without the Apple ones the app is
ad-hoc signed with the hardened runtime and `src-tauri/Entitlements.plist` (microphone), so the
signed path only adds the certificate and notarization.

Building installers locally (`npm run tauri build`) requires the updater signing key in
`TAURI_SIGNING_PRIVATE_KEY`; pass `--config '{"bundle":{"createUpdaterArtifacts":false}}'` to skip it.
On Windows the build copies the Visual C++ runtime (and, for CUDA release builds, cuBLAS; for
Vulkan builds, the Vulkan loader) into `src-tauri/runtime/`, which is installed next to the app.

## Third-party licenses

`THIRD_PARTY_LICENSES.md` lists every library and runtime that ships with the app, with its
license text; Settings → About shows the same list. `CREDITS.md` is generated from the About
screen's credits (`src/lib/credits.ts`). Regenerate all three after changing dependencies or credits:

```sh
node scripts/licenses.mjs          # CI runs it with --check
```

## Layout

| Path | Contents |
|---|---|
| `src/` | React + TypeScript frontend |
| `src-tauri/` | Tauri app (Rust core: storage, tray, recorder, job supervisor) |
| `src-tauri/migrations/` | SQLite schema migrations |
| `crates/earshot-worker/` | Transcription and speaker sidecar (ML runs in separate processes) |
| `crates/earshot-llm/` | Summary sidecar (llama.cpp; separate because it bundles its own ggml) |
| `crates/earshot-protocol/` | JSON protocol between the app and the sidecars |
| `scripts/` | Build helpers; `scripts/ci/` release helpers |
