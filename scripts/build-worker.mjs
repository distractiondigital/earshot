// Builds the earshot-worker sidecar and copies it to src-tauri/binaries/ with the
// target-triple suffix that Tauri's `externalBin` expects.
//
// Usage: node scripts/build-worker.mjs [--release] [--target <triple>] [--cpu]
// The Tauri CLI sets TAURI_ENV_TARGET_TRIPLE for before*Command hooks; it is used when present.
// `universal-apple-darwin` builds both macOS architectures and merges them with `lipo`.
//
// GPU backend for whisper.cpp:
//   macOS                -> Metal
//   Windows / Linux      -> CUDA when the CUDA Toolkit is installed (CUDA_PATH or nvcc on PATH),
//                           otherwise CPU. `--cpu` or EARSHOT_GPU=cpu forces CPU.
//   CMAKE_CUDA_ARCHITECTURES defaults to "native" (the GPU in this machine); set it to a list
//   (e.g. "75;80;86;89;120") for builds meant for other machines.
// GPU builds use their own target directory so switching backends doesn't rebuild whisper.cpp
// in the main one.

import { execFileSync } from "node:child_process";
import { copyFileSync, existsSync, mkdirSync, readdirSync } from "node:fs";
import { basename, dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const args = process.argv.slice(2);
const release = args.includes("--release");
const forceCpu = args.includes("--cpu") || process.env.EARSHOT_GPU === "cpu";
const targetIdx = args.indexOf("--target");
const explicitTarget = targetIdx >= 0 ? args[targetIdx + 1] : process.env.TAURI_ENV_TARGET_TRIPLE;
const profile = release ? "release" : "debug";

const hostTriple = execFileSync("rustc", ["-vV"], { encoding: "utf8" })
  .split("\n")
  .find((l) => l.startsWith("host:"))
  .slice("host:".length)
  .trim();
const triple = explicitTarget || hostTriple;
const ext = triple.includes("windows") ? ".exe" : "";
const outDir = join(root, "src-tauri", "binaries");
const out = join(outDir, `earshot-worker-${triple}${ext}`);
mkdirSync(outDir, { recursive: true });

/** Finds an installed CUDA Toolkit directory, or null. */
function cudaToolkit() {
  const candidates = [process.env.CUDA_PATH];
  if (process.platform === "win32") {
    try {
      // nvcc on PATH -> its parent's parent is the toolkit.
      const nvcc = execFileSync("where", ["nvcc"], { encoding: "utf8" }).split(/\r?\n/)[0].trim();
      if (nvcc) candidates.push(dirname(dirname(nvcc)));
    } catch {}
    // Standard install location; newest version first.
    const base = "C:\\Program Files\\NVIDIA GPU Computing Toolkit\\CUDA";
    if (existsSync(base)) {
      const versions = readdirSync(base)
        .filter((d) => /^v\d+\.\d+$/.test(d))
        .sort((a, b) => b.localeCompare(a, undefined, { numeric: true }));
      candidates.push(...versions.map((v) => join(base, v)));
    }
  } else {
    candidates.push("/usr/local/cuda");
  }
  return candidates.find((d) => d && existsSync(join(d, "bin", `nvcc${ext}`))) ?? null;
}

let features = [];
const env = { ...process.env };
let targetDir = join(root, "target");
if (!forceCpu && triple.includes("apple")) {
  features = ["metal"];
} else if (!forceCpu) {
  const cuda = cudaToolkit();
  if (cuda) {
    features = ["cuda"];
    targetDir = join(root, "target", "worker-cuda");
    env.CUDA_PATH = cuda;
    // Visual Studio's CUDA build rules look for CUDA_PATH_V<major>_<minor>.
    const m = /^v(\d+)\.(\d+)$/.exec(basename(cuda));
    if (m) env[`CUDA_PATH_V${m[1]}_${m[2]}`] = cuda;
    env.CMAKE_CUDA_ARCHITECTURES ??= "native";
    env.PATH = `${join(cuda, "bin")}${process.platform === "win32" ? ";" : ":"}${env.PATH}`;
  }
}
console.log(`earshot-worker: building with ${features.length ? features.join(", ") : "CPU only"}`);

/** Builds for one target (or the host when `target` is undefined) and returns the binary path. */
function build(target) {
  const cargoArgs = ["build", "-p", "earshot-worker", "--target-dir", targetDir];
  if (release) cargoArgs.push("--release");
  if (target) cargoArgs.push("--target", target);
  if (features.length) cargoArgs.push("--features", features.join(","));
  execFileSync("cargo", cargoArgs, { cwd: root, stdio: "inherit", env });
  return target
    ? join(targetDir, target, profile, `earshot-worker${ext}`)
    : join(targetDir, profile, `earshot-worker${ext}`);
}

if (triple === "universal-apple-darwin") {
  const parts = ["aarch64-apple-darwin", "x86_64-apple-darwin"].map(build);
  execFileSync("lipo", ["-create", "-output", out, ...parts], { stdio: "inherit" });
} else {
  copyFileSync(build(explicitTarget), out);
}
console.log(`earshot-worker -> ${out}`);
