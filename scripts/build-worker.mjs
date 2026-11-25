// Builds the earshot-worker sidecar and copies it to src-tauri/binaries/ with the
// target-triple suffix that Tauri's `externalBin` expects.
//
// Usage: node scripts/build-worker.mjs [--release] [--target <triple>]
// The Tauri CLI sets TAURI_ENV_TARGET_TRIPLE for before*Command hooks; it is used when present.
// `universal-apple-darwin` builds both macOS architectures and merges them with `lipo`.

import { execFileSync } from "node:child_process";
import { copyFileSync, mkdirSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const args = process.argv.slice(2);
const release = args.includes("--release");
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

/** Builds for one target (or the host when `target` is undefined) and returns the binary path. */
function build(target) {
  const cargoArgs = ["build", "-p", "earshot-worker"];
  if (release) cargoArgs.push("--release");
  if (target) cargoArgs.push("--target", target);
  execFileSync("cargo", cargoArgs, { cwd: root, stdio: "inherit" });
  return target
    ? join(root, "target", target, profile, `earshot-worker${ext}`)
    : join(root, "target", profile, `earshot-worker${ext}`);
}

if (triple === "universal-apple-darwin") {
  const parts = ["aarch64-apple-darwin", "x86_64-apple-darwin"].map(build);
  execFileSync("lipo", ["-create", "-output", out, ...parts], { stdio: "inherit" });
} else {
  copyFileSync(build(explicitTarget), out);
}
console.log(`earshot-worker -> ${out}`);
