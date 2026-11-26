// Generates the third-party license notices from the dependencies that actually ship:
//   THIRD_PARTY_LICENSES.md   every package with its license text (bundled with the app)
//   src/lib/licenses.json     name / version / license / link per package (About → licenses)
//
// Usage: node scripts/licenses.mjs [--check]
// Sources: `cargo metadata` (normal dependencies of the workspace members, for the Windows and
// macOS targets), package-lock.json (packages not marked dev, i.e. in the frontend bundle), and
// scripts/third-party-extra.json for native libraries and runtimes that come without a package
// manifest (whisper.cpp, llama.cpp, sherpa-onnx's libraries, the VC++ and CUDA runtimes).
// --check exits non-zero when the committed files are out of date (CI).

import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { existsSync, readdirSync, readFileSync, statSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const check = process.argv.includes("--check");
const PLATFORMS = ["x86_64-pc-windows-msvc", "aarch64-apple-darwin", "x86_64-apple-darwin"];
const LICENSE_FILE = /^(licen[cs]e|copying|notice|unlicense|copyright)([-._].*)?$/i;

/** License and notice files in a package directory, in a stable order. */
function licenseTexts(dir) {
  if (!dir || !existsSync(dir)) return [];
  return readdirSync(dir)
    .filter((f) => LICENSE_FILE.test(f) && statSync(join(dir, f)).isFile())
    .sort()
    .map((f) => readFileSync(join(dir, f), "utf8").replace(/\r\n/g, "\n").trim())
    .filter(Boolean);
}

function link(repo, fallback) {
  const url = (repo || fallback || "").replace(/^git\+/, "").replace(/\.git$/, "").replace(/^git:\/\//, "https://");
  return url.startsWith("https://") ? url : "";
}

// ---- Rust crates ----
const rust = new Map();
for (const platform of PLATFORMS) {
  const meta = JSON.parse(
    execFileSync("cargo", ["metadata", "--format-version", "1", "--locked", "--filter-platform", platform], {
      cwd: root,
      encoding: "utf8",
      maxBuffer: 256 * 1024 * 1024,
    }),
  );
  const members = new Set(meta.workspace_members);
  const nodes = new Map(meta.resolve.nodes.map((n) => [n.id, n]));
  const reached = new Set();
  const stack = [...members];
  while (stack.length) {
    const id = stack.pop();
    if (reached.has(id)) continue;
    reached.add(id);
    // Only normal dependencies end up in the binaries (not build scripts or dev/test tools).
    for (const dep of nodes.get(id).deps) if (dep.dep_kinds.some((k) => k.kind === null)) stack.push(dep.pkg);
  }
  for (const p of meta.packages) {
    if (!reached.has(p.id) || members.has(p.id)) continue;
    const key = `${p.name}@${p.version}`;
    if (rust.has(key)) continue;
    rust.set(key, {
      name: p.name,
      version: p.version,
      license: p.license ?? "see license text",
      url: link(p.repository, p.homepage) || `https://crates.io/crates/${p.name}`,
      source: "Rust",
      texts: licenseTexts(dirname(p.manifest_path)),
    });
  }
}

// ---- npm packages in the frontend bundle ----
const npm = [];
const lock = JSON.parse(readFileSync(join(root, "package-lock.json"), "utf8"));
for (const [path, p] of Object.entries(lock.packages)) {
  if (!path || p.dev || !path.startsWith("node_modules/")) continue;
  const dir = join(root, path);
  const pkg = existsSync(join(dir, "package.json")) ? JSON.parse(readFileSync(join(dir, "package.json"), "utf8")) : {};
  const name = pkg.name ?? path.slice(path.lastIndexOf("node_modules/") + "node_modules/".length);
  const repo = typeof pkg.repository === "string" ? pkg.repository : pkg.repository?.url;
  npm.push({
    name,
    version: p.version,
    license: p.license ?? pkg.license ?? "see license text",
    url: link(repo?.replace(/^github:/, "https://github.com/"), pkg.homepage) || `https://www.npmjs.com/package/${name}`,
    source: "JavaScript",
    texts: licenseTexts(dir),
  });
}

// ---- Native libraries and runtimes ----
const extraSpec = JSON.parse(readFileSync(join(root, "scripts", "third-party-extra.json"), "utf8"));
const registry = join(process.env.CARGO_HOME ?? join(process.env.USERPROFILE ?? process.env.HOME, ".cargo"), "registry", "src");
const registryDirs = existsSync(registry) ? readdirSync(registry).map((d) => join(registry, d)) : [];
const crateDir = (name, version) => registryDirs.map((d) => join(d, `${name}-${version}`)).find(existsSync);
const native = extraSpec.map((e) => {
  let texts = [];
  if (e.textFromCrate) {
    const [name, version, sub] = e.textFromCrate;
    texts = licenseTexts(join(crateDir(name, version) ?? "", sub ?? ""));
    if (!texts.length) throw new Error(`no license text for ${e.name} in ${name}-${version}/${sub ?? ""}`);
  }
  if (e.text) texts.push(e.text.join("\n"));
  return { name: e.name, version: e.version ?? "", license: e.license, url: e.url, source: e.source, texts };
});

const byName = (a, b) => a.name.localeCompare(b.name) || a.version.localeCompare(b.version, undefined, { numeric: true });
const all = [...native, ...[...rust.values()].sort(byName), ...npm.sort(byName)];

// ---- THIRD_PARTY_LICENSES.md: each distinct license text once, with the packages using it ----
// Texts are compared with whitespace collapsed (copies of the Apache license differ only in
// layout); a package with several files (MIT + Apache) appears under each.
const groups = new Map();
const noText = [];
for (const p of all) {
  if (!p.texts.length) {
    noText.push(p);
    continue;
  }
  for (const text of p.texts) {
    const hash = createHash("sha256").update(text.replace(/\s+/g, " ")).digest("hex");
    if (!groups.has(hash)) groups.set(hash, { text, packages: [] });
    const g = groups.get(hash);
    if (!g.packages.includes(p)) g.packages.push(p);
  }
}
const label = (p) => `${p.name}${p.version ? ` ${p.version}` : ""}`;
let md = `# Third-party licenses

Earshot includes the software listed below. Each entry gives the package, its license and the
license text the package ships with. This file is generated by \`scripts/licenses.mjs\` from the
dependencies that are part of the app; models are downloaded separately and listed in
Settings → About.

${all.length} packages: ${native.length} native libraries and runtimes, ${rust.size} Rust crates, ${npm.length} JavaScript packages.
`;
const title = (packages) =>
  packages.length > 3 ? `${packages.slice(0, 3).map(label).join(", ")} and ${packages.length - 3} more` : packages.map(label).join(", ");
for (const { text, packages } of groups.values()) {
  md += `\n---\n\n## ${title(packages)}\n\n`;
  md += packages.map((p) => `- ${label(p)} (${p.source}) — ${p.license}${p.url ? ` — ${p.url}` : ""}`).join("\n");
  md += `\n\n\`\`\`text\n${text.replace(/```/g, "'''")}\n\`\`\`\n`;
}
if (noText.length) {
  md += `\n---\n\n## Packages that ship without a license file\n\nThese are distributed under the license named, whose standard text applies.\n\n`;
  md += noText.map((p) => `- ${label(p)} (${p.source}) — ${p.license}${p.url ? ` — ${p.url}` : ""}`).join("\n") + "\n";
}

const json =
  JSON.stringify(
    all.map(({ name, version, license, url, source }) => ({ name, version, license, url, source })),
    null,
    1,
  ) + "\n";

const outputs = [
  [join(root, "THIRD_PARTY_LICENSES.md"), md],
  [join(root, "src", "lib", "licenses.json"), json],
];
if (check) {
  // Line endings may be converted on checkout (Windows), so they are ignored.
  const stale = outputs.filter(
    ([file, content]) => !existsSync(file) || readFileSync(file, "utf8").split("\r\n").join("\n") !== content,
  );
  if (stale.length) {
    console.error(`Out of date: ${stale.map(([f]) => f).join(", ")}. Run: node scripts/licenses.mjs`);
    process.exit(1);
  }
  console.log("third-party licenses are up to date");
} else {
  for (const [file, content] of outputs) writeFileSync(file, content);
  console.log(`${all.length} packages (${groups.size} distinct license texts, ${noText.length} without a file)`);
}
