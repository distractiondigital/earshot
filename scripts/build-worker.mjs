// Builds the earshot-worker sidecar and copies it to src-tauri/binaries/ with the
// target-triple suffix that Tauri's `externalBin` expects.
//
// Usage: node scripts/build-worker.mjs [--release] [--target <triple>]
// The Tauri CLI sets TAURI_ENV_TARGET_TRIPLE for before*Command hooks; it is used when present.

import { execFileSync } from "node:child_process";
import { copyFileSync, mkdirSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const args = process.argv.slice(2);
const release = args.includes("--release");
const targetIdx = args.indexOf("--target");
const explicitTarget = targetIdx >= 0 ? args[targetIdx + 1] : process.env.TAURI_ENV_TARGET_TRIPLE;

const hostTriple = execFileSync("rustc", ["-vV"], { encoding: "utf8" })
  .split("\n")
  .find((l) => l.startsWith("host:"))
  .slice("host:".length)
  .trim();
const triple = explicitTarget || hostTriple;

const cargoArgs = ["build", "-p", "earshot-worker"];
if (release) cargoArgs.push("--release");
if (explicitTarget) cargoArgs.push("--target", explicitTarget);
execFileSync("cargo", cargoArgs, { cwd: root, stdio: "inherit" });

const profile = release ? "release" : "debug";
const ext = triple.includes("windows") ? ".exe" : "";
const built = explicitTarget
  ? join(root, "target", explicitTarget, profile, `earshot-worker${ext}`)
  : join(root, "target", profile, `earshot-worker${ext}`);
const outDir = join(root, "src-tauri", "binaries");
mkdirSync(outDir, { recursive: true });
copyFileSync(built, join(outDir, `earshot-worker-${triple}${ext}`));
console.log(`earshot-worker -> src-tauri/binaries/earshot-worker-${triple}${ext}`);
