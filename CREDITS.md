# Credits

Earshot is © 2026 Distraction Digital, free and open source under the MIT License. It is built on the
work of others; thank you to all of them. The same list is in the app (**Settings → About**), and
every package that ships with the app, with its license text, is in
[THIRD_PARTY_LICENSES.md](THIRD_PARTY_LICENSES.md).

## Speech engine

| Component | By | License | What it does |
|---|---|---|---|
| [whisper.cpp](https://github.com/ggml-org/whisper.cpp) | the ggml authors | MIT | Turns speech into text, entirely on this computer. |
| [whisper-rs](https://codeberg.org/tazz4843/whisper-rs) | tazz4843 and contributors | Unlicense | Connects whisper.cpp to the app. |
| [sherpa-onnx](https://github.com/k2-fsa/sherpa-onnx) | the Next-gen Kaldi team (k2-fsa) | Apache-2.0 | Tells speakers apart, entirely on this computer. |
| [ONNX Runtime](https://onnxruntime.ai) | Microsoft | MIT | Runs the speaker models; part of sherpa-onnx. |
| [llama.cpp](https://github.com/ggml-org/llama.cpp) | the ggml authors | MIT | Runs the summary models, entirely on this computer. |
| [llama-cpp-rs](https://github.com/utilityai/llama-cpp-rs) | Utility AI and contributors | MIT or Apache-2.0 | Connects llama.cpp to the app. |

## Models (downloaded separately, not part of the app)

| Component | By | License | What it does |
|---|---|---|---|
| [Whisper](https://github.com/openai/whisper) | OpenAI | MIT | The speech recognition models, in the ggml format published by the whisper.cpp project. |
| [Silero VAD](https://github.com/snakers4/silero-vad) | Silero Team | MIT | Finds the parts of a recording that contain speech. |
| [pyannote segmentation 3.0](https://huggingface.co/pyannote/segmentation-3.0) | Hervé Bredin and pyannote contributors | MIT | Finds where each voice starts and stops, in the ONNX version published by sherpa-onnx. |
| [CAM++ and ERes2Net (3D-Speaker)](https://github.com/modelscope/3D-Speaker) | the 3D-Speaker team (Alibaba ModelScope) | Apache-2.0 | Voice models that tell speakers apart, trained on VoxCeleb. |
| [TitaNet Large](https://huggingface.co/nvidia/speakerverification_en_titanet_large) | NVIDIA | CC-BY-4.0 | Optional voice model, in the ONNX version published by sherpa-onnx. |
| [Qwen3.5 and Qwen3.6](https://huggingface.co/Qwen) | the Qwen team, Alibaba Cloud | Apache-2.0 | The summary models, in the GGUF versions published by Unsloth. |

## Audio

| Component | By | License | What it does |
|---|---|---|---|
| [cpal](https://github.com/RustAudio/cpal) | RustAudio contributors | Apache-2.0 | Microphone capture, and system audio on macOS. |
| [wasapi-rs](https://github.com/HEnquist/wasapi-rs) | Henrik Enquist | MIT | System, device and per-app audio capture on Windows. |
| [flacenc-rs](https://github.com/yotarok/flacenc-rs) | Yotaro Kubo | Apache-2.0 | Saves recordings as FLAC while they happen. |
| [Symphonia](https://github.com/pdeljanov/Symphonia) | Philip Deljanov | MPL-2.0 | Reads recordings back for transcription. |
| [rubato](https://github.com/HEnquist/rubato) | Henrik Enquist | MIT / Apache-2.0 |  |
| [rtrb](https://github.com/mgeier/rtrb) | Stjepan Glavina and Matthias Geier | MIT / Apache-2.0 |  |

## Software libraries

| Component | By | License | What it does |
|---|---|---|---|
| [Tauri](https://tauri.app) | Tauri Programme within The Commons Conservancy | MIT / Apache-2.0 | The app framework, including its updater and other plugins. |
| [SQLite](https://sqlite.org) | D. Richard Hipp and contributors | Public domain | Stores meetings and transcripts. |
| [rusqlite](https://github.com/rusqlite/rusqlite) | the rusqlite developers | MIT |  |
| [rusqlite_migration](https://github.com/cljoly/rusqlite_migration) | Clément Joly | Apache-2.0 |  |
| [reqwest](https://github.com/seanmonstar/reqwest) | Sean McArthur | MIT / Apache-2.0 | Downloads models. |
| [Tokio, Serde, RustCrypto hashes, sysinfo, nvml-wrapper](https://crates.io) | their contributors | MIT / Apache-2.0 |  |
| [React](https://react.dev) | Meta Platforms | MIT |  |
| [Radix UI](https://www.radix-ui.com) | WorkOS | MIT |  |
| [react-markdown and remark-gfm](https://github.com/remarkjs/react-markdown) | Titus Wormer and the unified collective | MIT | Shows summaries. |
| [shadcn/ui](https://ui.shadcn.com) | shadcn | MIT |  |
| [Tailwind CSS](https://tailwindcss.com) | Tailwind Labs | MIT |  |
| [class-variance-authority](https://github.com/joe-bell/cva) | Joe Bell | Apache-2.0 |  |
| [Lucide icons](https://lucide.dev) | Lucide contributors | ISC |  |
| [Geist](https://vercel.com/font) | Vercel | OFL-1.1 | The typeface. |
| [Rust, Vite, TypeScript, CMake, LLVM](https://www.rust-lang.org) | their contributors | MIT / Apache-2.0 / BSD-3-Clause | Build tools; not shipped in the app. |

---

Earshot was designed by a filmmaker, not a software developer. It was built with AI as a tool, so you can stay present in your conversations and then get back to being creative. We believe AI should be local-first wherever possible. That's why Earshot never sends your information to a data center, and why it will always be open and free.
