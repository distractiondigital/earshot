// CI only: copies one build's installers and updater bundles into <outDir> under their release
// names, and writes updater-<build>.json, this build's part of latest.json (write-latest-json.mjs).
//
// Usage: node scripts/ci/collect-release.mjs <build> <outDir>
//   build: windows-x64 | windows-x64-cuda | macos-universal (the release.yml matrix names)

import { copyFileSync, existsSync, mkdirSync, readdirSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";

const [build, outDir] = process.argv.slice(2);
if (!build || !outDir) throw new Error("usage: collect-release.mjs <build> <outDir>");
const root = join(import.meta.dirname, "..", "..");
const { version } = JSON.parse(readFileSync(join(root, "src-tauri", "tauri.conf.json"), "utf8"));
mkdirSync(outDir, { recursive: true });

/** The single file in `dir` whose name matches `re`. */
function one(dir, re) {
  const found = existsSync(dir) ? readdirSync(dir).filter((f) => re.test(f)) : [];
  if (found.length !== 1) throw new Error(`expected one ${re} in ${dir}, found: ${found.join(", ") || "none"}`);
  return join(dir, found[0]);
}

const platforms = {};
/** Copies an installer (and its updater signature when `updaterKeys` are given). */
function add(src, name, updaterKeys = []) {
  copyFileSync(src, join(outDir, name));
  console.log(`${src} -> ${name}`);
  // No signature: a test build made without the updater key.
  if (!updaterKeys.length || !existsSync(`${src}.sig`)) return;
  const signature = readFileSync(`${src}.sig`, "utf8").trim();
  copyFileSync(`${src}.sig`, join(outDir, `${name}.sig`));
  for (const key of updaterKeys) platforms[key] = { file: name, signature };
}

if (build.startsWith("windows-")) {
  const nsis = join(root, "target", "release", "bundle", "nsis");
  const setup = one(nsis, /_x64-setup\.exe$/);
  if (build === "windows-x64-cuda") add(setup, `Earshot_${version}_x64-cuda-setup.exe`, ["windows-x86_64-cuda"]);
  else add(setup, `Earshot_${version}_x64-setup.exe`, ["windows-x86_64"]);
} else if (build === "macos-universal") {
  const bundle = join(root, "target", "universal-apple-darwin", "release", "bundle");
  add(one(join(bundle, "dmg"), /\.dmg$/), `Earshot_${version}_universal.dmg`);
  // The updater bundle only exists when the updater key was available.
  if (readdirSync(join(bundle, "macos")).some((f) => f.endsWith(".app.tar.gz"))) {
    add(one(join(bundle, "macos"), /\.app\.tar\.gz$/), `Earshot_${version}_universal.app.tar.gz`, ["darwin-aarch64", "darwin-x86_64"]);
  }
} else {
  throw new Error(`unknown build ${build}`);
}
writeFileSync(join(outDir, `updater-${build}.json`), JSON.stringify(platforms, null, 2));
