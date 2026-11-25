// Bumps the app version everywhere it is declared, commits, and tags.
//
// Usage: npm run release -- <version>      e.g. npm run release -- 0.2.0
//
// Pushing the tag (`git push --follow-tags`) triggers .github/workflows/release.yml, which builds
// signed installers and publishes them, plus the updater manifest, to the public releases repo.

import { execFileSync } from "node:child_process";
import { readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const version = process.argv[2];
if (!version || !/^\d+\.\d+\.\d+(-[0-9A-Za-z.-]+)?$/.test(version)) {
  console.error("usage: npm run release -- <semver>, e.g. 0.2.0");
  process.exit(1);
}
const git = (...a) => execFileSync("git", a, { cwd: root, encoding: "utf8" }).trim();

if (git("status", "--porcelain")) {
  console.error("working tree is not clean; commit or stash first");
  process.exit(1);
}
if (git("tag", "--list", `v${version}`)) {
  console.error(`tag v${version} already exists`);
  process.exit(1);
}

function editJson(rel, fn) {
  const path = join(root, rel);
  const json = JSON.parse(readFileSync(path, "utf8"));
  fn(json);
  writeFileSync(path, JSON.stringify(json, null, 2) + "\n");
}
editJson("package.json", (j) => (j.version = version));
editJson("package-lock.json", (j) => {
  j.version = version;
  if (j.packages?.[""]) j.packages[""].version = version;
});
editJson("src-tauri/tauri.conf.json", (j) => (j.version = version));

const cargoPath = join(root, "Cargo.toml");
const cargo = readFileSync(cargoPath, "utf8");
const bumped = cargo.replace(
  /(\[workspace\.package\][^[]*?\nversion = )"[^"]*"/,
  `$1"${version}"`,
);
if (bumped === cargo) {
  console.error("could not find [workspace.package] version in Cargo.toml");
  process.exit(1);
}
writeFileSync(cargoPath, bumped);
execFileSync("cargo", ["update", "--workspace"], { cwd: root, stdio: "inherit" });

git("add", "package.json", "package-lock.json", "src-tauri/tauri.conf.json", "Cargo.toml", "Cargo.lock");
git("commit", "-m", `chore(release): v${version}`);
git("tag", "-a", `v${version}`, "-m", `v${version}`);
console.log(`\nTagged v${version}. Publish with:  git push --follow-tags`);
