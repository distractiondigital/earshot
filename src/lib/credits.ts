/** Third-party work credited on the About screen. Licenses checked against each package's own metadata. */
export type Credit = {
  name: string;
  by: string;
  license: string;
  url: string;
  note?: string;
};

export type CreditGroup = { title: string; items: Credit[] };

export const credits: CreditGroup[] = [
  {
    title: "Speech engine",
    items: [
      {
        name: "whisper.cpp",
        by: "the ggml authors",
        license: "MIT",
        url: "https://github.com/ggml-org/whisper.cpp",
        note: "Turns speech into text, entirely on this computer.",
      },
      {
        name: "whisper-rs",
        by: "tazz4843 and contributors",
        license: "Unlicense",
        url: "https://codeberg.org/tazz4843/whisper-rs",
        note: "Connects whisper.cpp to the app.",
      },
    ],
  },
  {
    title: "Models (downloaded separately, not part of the app)",
    items: [
      {
        name: "Whisper",
        by: "OpenAI",
        license: "MIT",
        url: "https://github.com/openai/whisper",
        note: "The speech recognition models, in the ggml format published by the whisper.cpp project.",
      },
      {
        name: "Silero VAD",
        by: "Silero Team",
        license: "MIT",
        url: "https://github.com/snakers4/silero-vad",
        note: "Finds the parts of a recording that contain speech.",
      },
    ],
  },
  {
    title: "Audio",
    items: [
      {
        name: "cpal",
        by: "RustAudio contributors",
        license: "Apache-2.0",
        url: "https://github.com/RustAudio/cpal",
        note: "Microphone capture, and system audio on macOS.",
      },
      {
        name: "wasapi-rs",
        by: "Henrik Enquist",
        license: "MIT",
        url: "https://github.com/HEnquist/wasapi-rs",
        note: "System, device and per-app audio capture on Windows.",
      },
      {
        name: "flacenc-rs",
        by: "Yotaro Kubo",
        license: "Apache-2.0",
        url: "https://github.com/yotarok/flacenc-rs",
        note: "Saves recordings as FLAC while they happen.",
      },
      {
        name: "Symphonia",
        by: "Philip Deljanov",
        license: "MPL-2.0",
        url: "https://github.com/pdeljanov/Symphonia",
        note: "Reads recordings back for transcription.",
      },
      {
        name: "rubato",
        by: "Henrik Enquist",
        license: "MIT / Apache-2.0",
        url: "https://github.com/HEnquist/rubato",
      },
      {
        name: "rtrb",
        by: "Stjepan Glavina and Matthias Geier",
        license: "MIT / Apache-2.0",
        url: "https://github.com/mgeier/rtrb",
      },
    ],
  },
  {
    title: "Software libraries",
    items: [
      {
        name: "Tauri",
        by: "Tauri Programme within The Commons Conservancy",
        license: "MIT / Apache-2.0",
        url: "https://tauri.app",
        note: "The app framework, including its updater and other plugins.",
      },
      {
        name: "SQLite",
        by: "D. Richard Hipp and contributors",
        license: "Public domain",
        url: "https://sqlite.org",
        note: "Stores meetings and transcripts.",
      },
      {
        name: "rusqlite",
        by: "the rusqlite developers",
        license: "MIT",
        url: "https://github.com/rusqlite/rusqlite",
      },
      {
        name: "rusqlite_migration",
        by: "Clément Joly",
        license: "Apache-2.0",
        url: "https://github.com/cljoly/rusqlite_migration",
      },
      {
        name: "reqwest",
        by: "Sean McArthur",
        license: "MIT / Apache-2.0",
        url: "https://github.com/seanmonstar/reqwest",
        note: "Downloads models.",
      },
      {
        name: "Tokio, Serde, RustCrypto hashes, sysinfo, nvml-wrapper",
        by: "their contributors",
        license: "MIT / Apache-2.0",
        url: "https://crates.io",
      },
      {
        name: "React",
        by: "Meta Platforms",
        license: "MIT",
        url: "https://react.dev",
      },
      {
        name: "Radix UI",
        by: "WorkOS",
        license: "MIT",
        url: "https://www.radix-ui.com",
      },
      {
        name: "shadcn/ui",
        by: "shadcn",
        license: "MIT",
        url: "https://ui.shadcn.com",
      },
      {
        name: "Tailwind CSS",
        by: "Tailwind Labs",
        license: "MIT",
        url: "https://tailwindcss.com",
      },
      {
        name: "class-variance-authority",
        by: "Joe Bell",
        license: "Apache-2.0",
        url: "https://github.com/joe-bell/cva",
      },
      {
        name: "Lucide icons",
        by: "Lucide contributors",
        license: "ISC",
        url: "https://lucide.dev",
      },
      {
        name: "Geist",
        by: "Vercel",
        license: "OFL-1.1",
        url: "https://vercel.com/font",
        note: "The typeface.",
      },
      {
        name: "Rust, Vite, TypeScript, CMake, LLVM",
        by: "their contributors",
        license: "MIT / Apache-2.0 / BSD-3-Clause",
        url: "https://www.rust-lang.org",
        note: "Build tools; not shipped in the app.",
      },
    ],
  },
];

/** Closing note on the About screen. */
export const aboutNote =
  "Earshot was designed by a filmmaker, not a software developer. It was built with AI as a tool, " +
  "so you can stay present in your conversations and then get back to being creative. " +
  "We believe AI should be local-first wherever possible. That's why Earshot never sends your " +
  "information to a data center, and why it will always be open and free.";
