// CI only: merges the updater-<build>.json parts written by collect-release.mjs into latest.json
// (the Tauri updater's static manifest) and writes the release notes. The parts are removed so
// only installers, signatures and latest.json are published.
//
// Usage: node scripts/ci/write-latest-json.mjs <dir> <tag> <downloadBaseUrl>

import { readdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { join } from "node:path";

const [dir, tag, base] = process.argv.slice(2);
if (!dir || !tag || !base) throw new Error("usage: write-latest-json.mjs <dir> <tag> <downloadBaseUrl>");

const platforms = {};
for (const part of readdirSync(dir).filter((f) => /^updater-.*\.json$/.test(f))) {
  for (const [key, { file, signature }] of Object.entries(JSON.parse(readFileSync(join(dir, part), "utf8")))) {
    if (platforms[key]) throw new Error(`two builds claim updater target ${key}`);
    platforms[key] = { signature, url: `${base}/${encodeURIComponent(file)}` };
  }
  rmSync(join(dir, part));
}
for (const key of ["windows-x86_64", "windows-x86_64-cuda", "darwin-aarch64", "darwin-x86_64"]) {
  if (!platforms[key]) throw new Error(`no build for updater target ${key}`);
}

const version = tag.replace(/^v/, "");
const { version: built } = JSON.parse(readFileSync(join(import.meta.dirname, "..", "..", "src-tauri", "tauri.conf.json"), "utf8"));
if (built !== version) throw new Error(`tag ${tag} does not match the app version ${built}`);
const notes = `Earshot ${version}.`;
writeFileSync(
  join(dir, "latest.json"),
  JSON.stringify({ version, notes, pub_date: new Date().toISOString(), platforms }, null, 2) + "\n",
);

const files = readdirSync(dir).filter((f) => !f.endsWith(".sig") && f !== "latest.json").sort();
const describe = (f) =>
  f.endsWith("_x64-cuda-setup.exe")
    ? "Windows, NVIDIA GPU (CUDA). Larger download; fastest on GeForce RTX 20-series and newer."
    : f.endsWith("_x64-setup.exe")
      ? "Windows. Uses the GPU through Vulkan (NVIDIA, AMD, Intel), otherwise the CPU."
      : f.endsWith(".dmg")
        ? "macOS (Apple Silicon and Intel)."
        : f.endsWith(".app.tar.gz")
          ? "macOS update bundle (used by the updater)."
          : "";
writeFileSync(
  "release-notes.md",
  `Download the installer for your computer:\n\n${files
    .filter((f) => describe(f))
    .map((f) => `- **${f}**: ${describe(f)}`)
    .join("\n")}\n`,
);
console.log(JSON.stringify(platforms, null, 2));
