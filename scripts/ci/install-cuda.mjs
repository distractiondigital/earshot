// CI only: installs the parts of the CUDA Toolkit that the Windows CUDA build needs, from NVIDIA's
// redistributable archives, each checked against the SHA-256 in NVIDIA's release manifest.
// Then hooks CUDA into Visual Studio's MSBuild (whisper.cpp and llama.cpp are built with the
// Visual Studio CMake generator) and exports CUDA_PATH / PATH for the following steps.
//
// Usage: node scripts/ci/install-cuda.mjs <version>     e.g. 13.5.0

import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { appendFileSync, cpSync, existsSync, mkdirSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import { join } from "node:path";

const version = process.argv[2];
if (!/^\d+\.\d+\.\d+$/.test(version ?? "")) throw new Error("usage: install-cuda.mjs <major.minor.patch>");
const [major, minor] = version.split(".");
const BASE = "https://developer.download.nvidia.com/compute/cuda/redist/";
// What ggml's CUDA backend needs to compile and link: the compiler, runtime, cuBLAS and headers.
const COMPONENTS = [
  "cuda_nvcc",
  "cuda_crt",
  "libnvvm",
  "cuda_cudart",
  "libcublas",
  "cccl",
  "cuda_nvtx",
  "cuda_profiler_api",
  "cuda_nvml_dev",
  "visual_studio_integration",
];
const PLATFORM = "windows-x86_64";

const cudaDir = join(process.env.ProgramFiles ?? "C:\\Program Files", "NVIDIA GPU Computing Toolkit", "CUDA", `v${major}.${minor}`);
const work = join(process.env.RUNNER_TEMP ?? process.env.TEMP, "cuda-redist");
mkdirSync(work, { recursive: true });
mkdirSync(cudaDir, { recursive: true });

/** Fetches a URL into a buffer, retrying dropped connections. */
async function download(url) {
  for (let attempt = 1; ; attempt++) {
    try {
      const res = await fetch(url);
      if (!res.ok) throw new Error(`download failed (${res.status}): ${url}`);
      return Buffer.from(await res.arrayBuffer());
    } catch (e) {
      if (attempt >= 4) throw e;
      console.log(`${e.message}; retrying`);
      await new Promise((r) => setTimeout(r, attempt * 5000));
    }
  }
}

const manifest = JSON.parse((await download(`${BASE}redistrib_${version}.json`)).toString("utf8"));
for (const name of COMPONENTS) {
  const entry = manifest[name]?.[PLATFORM];
  if (!entry) throw new Error(`${name} has no ${PLATFORM} archive in CUDA ${version}`);
  const buf = await download(BASE + entry.relative_path);
  const sha = createHash("sha256").update(buf).digest("hex");
  if (sha !== entry.sha256) throw new Error(`checksum mismatch for ${name}: ${sha} != ${entry.sha256}`);
  const zip = join(work, `${name}.zip`);
  writeFileSync(zip, buf);
  const out = join(work, name);
  rmSync(out, { recursive: true, force: true });
  mkdirSync(out);
  execFileSync(join(process.env.SystemRoot ?? "C:\\Windows", "System32", "tar.exe"), ["-xf", zip, "-C", out], { stdio: "inherit" });
  // Each archive has one top-level folder holding bin/, include/, lib/ ...
  const [top] = readdirSync(out);
  cpSync(join(out, top), cudaDir, { recursive: true, force: true });
  console.log(`${name} ${manifest[name].version} ok`);
}

// MSBuild integration (what the full installer's "Visual Studio Integration" option does).
const vswhere = join(process.env["ProgramFiles(x86)"], "Microsoft Visual Studio", "Installer", "vswhere.exe");
const vs = execFileSync(vswhere, ["-latest", "-products", "*", "-property", "installationPath"], { encoding: "utf8" }).trim();
// The redistributable archive keeps them in visual_studio_integration/ (the full installer in extras/).
const ext = [
  join(cudaDir, "visual_studio_integration", "MSBuildExtensions"),
  join(cudaDir, "extras", "visual_studio_integration", "MSBuildExtensions"),
].find(existsSync);
if (!ext) throw new Error(`no MSBuildExtensions in ${cudaDir}`);
for (const toolset of readdirSync(join(vs, "MSBuild", "Microsoft", "VC"))) {
  const dest = join(vs, "MSBuild", "Microsoft", "VC", toolset, "BuildCustomizations");
  if (existsSync(dest)) cpSync(ext, dest, { recursive: true, force: true });
}

if (process.env.GITHUB_ENV) {
  appendFileSync(process.env.GITHUB_ENV, `CUDA_PATH=${cudaDir}\nCUDA_PATH_V${major}_${minor}=${cudaDir}\n`);
  appendFileSync(process.env.GITHUB_PATH, `${join(cudaDir, "bin")}\n${join(cudaDir, "bin", "x64")}\n`);
}
console.log(`CUDA ${version} -> ${cudaDir}`);
