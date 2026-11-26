// Builds the sidecars -- earshot-worker (transcription, speakers) and earshot-llm (summaries) --
// and copies them to src-tauri/binaries/ with the target-triple suffix that Tauri's `externalBin`
// expects. They are separate binaries because whisper.cpp and llama.cpp each bundle their own
// copy of ggml, which can't be linked into one executable.
//
// Usage: node scripts/build-worker.mjs [--release] [--target <triple>] [--gpu cuda|vulkan|cpu] [--cpu]
// The Tauri CLI sets TAURI_ENV_TARGET_TRIPLE for before*Command hooks; it is used when present.
// `universal-apple-darwin` builds both macOS architectures and merges them with `lipo`.
//
// GPU backend for whisper.cpp and llama.cpp:
//   macOS                -> Metal
//   Windows / Linux      -> `--gpu <backend>` or EARSHOT_GPU=<backend> when given; otherwise CUDA
//                           when the CUDA Toolkit is installed (CUDA_PATH or nvcc on PATH), else
//                           Vulkan when the Vulkan SDK is (VULKAN_SDK), else CPU. `--cpu` = `--gpu cpu`.
//   CMAKE_CUDA_ARCHITECTURES defaults to "native" (the GPU in this machine); set it to a list
//   (e.g. "75;80;86;89;120") for builds meant for other machines.
// GPU builds use their own target directory so switching backends doesn't rebuild whisper.cpp
// in the main one.
//
// Windows: the sidecars link the Visual C++ runtime dynamically, and CUDA builds load cuBLAS.
// Those DLLs are copied to src-tauri/runtime/, which tauri.windows.conf.json bundles next to the
// executables, so the installer works on a PC without the VC++ Redistributable or CUDA Toolkit.
// cuBLAS (~530 MB) is only staged for --release builds; dev runs find it on PATH.

import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { copyFileSync, existsSync, mkdirSync, readdirSync, readFileSync, renameSync, rmSync, writeFileSync } from "node:fs";
import { basename, dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const args = process.argv.slice(2);
const release = args.includes("--release");
const gpuIdx = args.indexOf("--gpu");
const requestedGpu = args.includes("--cpu") ? "cpu" : gpuIdx >= 0 ? args[gpuIdx + 1] : process.env.EARSHOT_GPU || null;
if (requestedGpu && !["cuda", "vulkan", "cpu"].includes(requestedGpu)) {
  throw new Error(`Unknown GPU backend "${requestedGpu}" (expected cuda, vulkan or cpu)`);
}
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
const BINS = ["earshot-worker", "earshot-llm"];
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
let cuda = null;
if (triple.includes("apple")) {
  if (requestedGpu !== "cpu") features = ["metal"];
} else {
  let gpu = requestedGpu;
  if (!gpu) gpu = cudaToolkit() ? "cuda" : process.env.VULKAN_SDK ? "vulkan" : "cpu";
  if (gpu === "cuda") {
    cuda = cudaToolkit();
    if (!cuda) throw new Error("--gpu cuda: no CUDA Toolkit found (set CUDA_PATH)");
  }
  if (gpu === "vulkan") {
    if (!process.env.VULKAN_SDK) throw new Error("--gpu vulkan: the Vulkan SDK is not installed (VULKAN_SDK is unset)");
    features = ["vulkan"];
    targetDir = join(root, "target", "worker-vulkan");
  }
  if (cuda) {
    features = ["cuda"];
    targetDir = join(root, "target", "worker-cuda");
    env.CUDA_PATH = cuda;
    // Visual Studio's CUDA build rules look for CUDA_PATH_V<major>_<minor>.
    const m = /^v(\d+)\.(\d+)$/.exec(basename(cuda));
    if (m) env[`CUDA_PATH_V${m[1]}_${m[2]}`] = cuda;
    env.CMAKE_CUDA_ARCHITECTURES ??= "native";
    // llama.cpp's build reads CMake's own variable for this.
    env.CUDAARCHS ??= env.CMAKE_CUDA_ARCHITECTURES;
    // On Windows the variable is usually spelled "Path"; a copied env object is case-sensitive.
    const pathKey = Object.keys(env).find((k) => k.toUpperCase() === "PATH") ?? "PATH";
    env[pathKey] = `${join(cuda, "bin")}${process.platform === "win32" ? ";" : ":"}${env[pathKey] ?? ""}`;
  }
}
// EARSHOT_WORKER_TARGET_DIR overrides it: the Windows Vulkan build nests deeper than the 260-character
// path limit under a long checkout path (CI uses C:/b).
if (process.env.EARSHOT_WORKER_TARGET_DIR) targetDir = process.env.EARSHOT_WORKER_TARGET_DIR;
console.log(`sidecars: building with ${features.length ? features.join(", ") : "CPU only"}`);

// sherpa-onnx (speaker diarization) links a prebuilt static library. sherpa-onnx-sys would download
// it at build time without checking it, and on Windows it picks the build that uses the static C
// runtime (/MT), which can't be linked with whisper.cpp (/MD; its CUDA code ignores /MT). So the
// library is fetched here, verified against the checksum GitHub publishes for the release, extracted,
// and handed over through SHERPA_ONNX_LIB_DIR. The host platform's copy lives in a fixed "host"
// folder, which .cargo/config.toml points plain `cargo` commands (clippy, tests) at.
// Keep the version in step with the worker's Cargo.toml.
const SHERPA_VERSION = "2.0.3";
const SHERPA_ARCHIVES = {
  "x86_64-pc-windows-msvc": ["win-x64-static-MD-Release", "4187678699067d486950717c1e667b2e9a59aef83106a0e8a12328ffb750adf2"],
  "aarch64-pc-windows-msvc": ["win-arm64-static-MD-Release", "c9f57aa3a03e43c9efd45f46ccf14bc47eeed1d87f65948993d52ce205b6b0c5"],
  "aarch64-apple-darwin": ["osx-arm64-static", "c86468ad8f79b679550f0865cc1a67747f58b8cc8780623ed7d8367fd4b541a3"],
  "x86_64-apple-darwin": ["osx-x64-static", "aa2669f175bc3b496788308e211e1bbb792ca7ba654f79db69bf20bef1d597a0"],
  "x86_64-unknown-linux-gnu": ["linux-x64-static", "f927e695d29b8c7ea3fda97eed215204b210fd57f9faf990a93377bac098ad57"],
  "aarch64-unknown-linux-gnu": ["linux-aarch64-static", "11f9860fd4dc18af397a493ee9bcb88d18c6ba1b235fb3041ec1a3ef6522364b"],
};
const archiveDir = join(root, "target", "sherpa-onnx-archives");

/** Downloads (if needed), verifies and extracts the sherpa-onnx library; returns its lib directory. */
async function sherpaLibDir(target) {
  const entry = SHERPA_ARCHIVES[target];
  if (!entry) throw new Error(`No pinned sherpa-onnx library for ${target}`);
  const [kind, sha256] = entry;
  const stem = `sherpa-onnx-v${SHERPA_VERSION}-${kind}-lib`;
  const file = join(archiveDir, `${stem}.tar.bz2`);
  const dir = join(archiveDir, target === hostTriple ? "host" : stem);
  const libDir = join(dir, "lib");
  const marker = join(dir, ".verified");
  if (existsSync(marker) && readFileSync(marker, "utf8") === sha256) return libDir;

  const digest = (buf) => createHash("sha256").update(buf).digest("hex");
  mkdirSync(archiveDir, { recursive: true });
  if (!existsSync(file) || digest(readFileSync(file)) !== sha256) {
    const url = `https://github.com/k2-fsa/sherpa-onnx/releases/download/v${SHERPA_VERSION}/${stem}.tar.bz2`;
    console.log(`earshot-worker: downloading ${stem}.tar.bz2`);
    // A few tries: CI runners occasionally drop the connection mid-download.
    let buf;
    for (let attempt = 1; ; attempt++) {
      try {
        const res = await fetch(url);
        if (!res.ok) throw new Error(`Download failed (${res.status}): ${url}`);
        buf = Buffer.from(await res.arrayBuffer());
        break;
      } catch (e) {
        if (attempt >= 4) throw e;
        console.log(`earshot-worker: download failed (${e.message}), retrying`);
        await new Promise((r) => setTimeout(r, attempt * 5000));
      }
    }
    const got = digest(buf);
    if (got !== sha256) throw new Error(`Checksum mismatch for ${stem}: expected ${sha256}, got ${got}`);
    writeFileSync(`${file}.part`, buf);
    renameSync(`${file}.part`, file);
  }
  rmSync(join(archiveDir, stem), { recursive: true, force: true });
  rmSync(dir, { recursive: true, force: true });
  // bsdtar ships with Windows 10+ and macOS; both read bzip2.
  const tar = process.platform === "win32" ? join(process.env.SystemRoot ?? "C:\\Windows", "System32", "tar.exe") : "tar";
  execFileSync(tar, ["-xjf", file, "-C", archiveDir], { stdio: "inherit" });
  if (dir !== join(archiveDir, stem)) renameSync(join(archiveDir, stem), dir);
  if (!existsSync(libDir)) throw new Error(`${stem}.tar.bz2 has no lib/ directory`);
  writeFileSync(marker, sha256);
  return libDir;
}

/** Builds both sidecars for one target (or the host when `target` is undefined); returns the
 * directory holding the binaries. */
async function build(target) {
  const libDir = await sherpaLibDir(target ?? triple);
  const cargoArgs = ["build", ...BINS.flatMap((b) => ["-p", b]), "--target-dir", targetDir];
  if (release) cargoArgs.push("--release");
  if (target) cargoArgs.push("--target", target);
  if (features.length) cargoArgs.push("--features", BINS.flatMap((b) => features.map((f) => `${b}/${f}`)).join(","));
  execFileSync("cargo", cargoArgs, { cwd: root, stdio: "inherit", env: { ...env, SHERPA_ONNX_LIB_DIR: libDir } });
  return target ? join(targetDir, target, profile) : join(targetDir, profile);
}

const outPath = (bin) => join(outDir, `${bin}-${triple}${ext}`);
if (triple === "universal-apple-darwin") {
  const arches = ["aarch64-apple-darwin", "x86_64-apple-darwin"];
  const dirs = [await build(arches[0]), await build(arches[1])];
  for (const bin of BINS) {
    execFileSync("lipo", ["-create", "-output", outPath(bin), ...dirs.map((d) => join(d, bin))], { stdio: "inherit" });
    // Tauri compiles the app once per architecture and checks each one's sidecars by name too.
    arches.forEach((arch, i) => copyFileSync(join(dirs[i], bin), join(outDir, `${bin}-${arch}`)));
  }
} else {
  const dir = await build(explicitTarget);
  for (const bin of BINS) copyFileSync(join(dir, `${bin}${ext}`), outPath(bin));
}
for (const bin of BINS) console.log(`${bin} -> ${outPath(bin)}`);

/** The newest Visual C++ runtime redistributable folder for an architecture ("x64", "arm64"). */
function vcRuntimeDir(arch) {
  const vswhere = join(process.env["ProgramFiles(x86)"] ?? "C:\\Program Files (x86)", "Microsoft Visual Studio", "Installer", "vswhere.exe");
  const install = execFileSync(vswhere, ["-latest", "-products", "*", "-property", "installationPath"], { encoding: "utf8" }).trim();
  const redist = join(install, "VC", "Redist", "MSVC");
  const versions = readdirSync(redist)
    .filter((d) => /^\d+\.\d+\.\d+$/.test(d))
    .sort((a, b) => b.localeCompare(a, undefined, { numeric: true }));
  for (const v of versions) {
    const archDir = join(redist, v, arch);
    if (!existsSync(archDir)) continue;
    const crt = readdirSync(archDir).find((d) => /^Microsoft\.VC\d+\.CRT$/.test(d));
    if (crt) return join(archDir, crt);
  }
  throw new Error(`No Visual C++ ${arch} runtime found under ${redist}`);
}

// Runtime DLLs bundled next to the sidecars (see the header). The folder is rebuilt every time so
// switching backends never leaves another build's DLLs behind.
if (triple.includes("windows")) {
  const runtimeDir = join(root, "src-tauri", "runtime");
  rmSync(runtimeDir, { recursive: true, force: true });
  mkdirSync(runtimeDir, { recursive: true });
  const crt = vcRuntimeDir(triple.startsWith("aarch64") ? "arm64" : "x64");
  const dlls = ["msvcp140.dll", "msvcp140_1.dll", "vcruntime140.dll"];
  if (!triple.startsWith("aarch64")) dlls.push("vcruntime140_1.dll");
  for (const dll of dlls) copyFileSync(join(crt, dll), join(runtimeDir, dll));
  if (features.includes("vulkan") && release) {
    // The sidecars import vulkan-1.dll, which normally comes with the GPU driver. Shipping the
    // Khronos loader keeps them starting (and falling back to the CPU) on a PC without one.
    const sdk = process.env.VULKAN_SDK;
    const loader = [
      process.env.EARSHOT_VULKAN_LOADER,
      join(sdk, "Bin", "vulkan-1.dll"),
      join(sdk, "..", "runtime", "x64", "vulkan-1.dll"),
      join(sdk, "runtime", "x64", "vulkan-1.dll"),
      join(process.env.SystemRoot ?? "C:\\Windows", "System32", "vulkan-1.dll"),
    ].find((p) => p && existsSync(p));
    if (!loader) throw new Error("vulkan-1.dll not found (set EARSHOT_VULKAN_LOADER)");
    copyFileSync(loader, join(runtimeDir, "vulkan-1.dll"));
  }
  if (cuda && release) {
    // CUDA 13 keeps its DLLs in bin\x64, older versions in bin.
    const bin = [join(cuda, "bin", "x64"), join(cuda, "bin")].find(
      (d) => existsSync(d) && readdirSync(d).some((f) => /^cublas64_\d+\.dll$/.test(f)),
    );
    if (!bin) throw new Error(`No cuBLAS DLL found in ${cuda}`);
    for (const dll of readdirSync(bin).filter((f) => /^cublas(Lt)?64_\d+\.dll$/.test(f))) {
      copyFileSync(join(bin, dll), join(runtimeDir, dll));
    }
  }
  console.log(`runtime DLLs -> ${runtimeDir}: ${readdirSync(runtimeDir).join(", ")}`);
}
