#!/usr/bin/env node
// Builds the npm packages for a kitbag release from its GitHub release assets.
//
//   node scripts/npm/build.mjs --version 0.1.0 --assets <dir> [--out <dir>] [--scope @axiomatic-aardvark]
//
// <dir> holds the release archives and their .sha256 files (`gh release download vX.Y.Z`).
// Output: one package per platform (holding only that platform's binary, selected by npm via
// `os`/`cpu`) plus the main package, which lists them as optionalDependencies and ships a tiny
// JS shim that execs the right binary. No install scripts: this is the esbuild/biome layout.
import { createHash } from "node:crypto";
import { execFileSync } from "node:child_process";
import { chmodSync, copyFileSync, existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const args = Object.fromEntries(
  process.argv.slice(2).reduce((acc, a, i, all) => (a.startsWith("--") ? [...acc, [a.slice(2), all[i + 1]]] : acc), []),
);
const version = (args.version ?? "").replace(/^v/, "");
const scope = args.scope ?? "@axiomatic-aardvark";
const assets = resolve(args.assets ?? "");
const out = resolve(args.out ?? "npm-dist");
if (!/^\d+\.\d+\.\d+(-[0-9A-Za-z.-]+)?$/.test(version) || !args.assets) {
  console.error("usage: build.mjs --version X.Y.Z --assets <dir> [--out <dir>] [--scope @scope]");
  process.exit(2);
}

// npm platform/arch -> Rust target built by .github/workflows/release.yml
export const PLATFORMS = {
  "darwin-arm64": "aarch64-apple-darwin",
  "darwin-x64": "x86_64-apple-darwin",
  "linux-arm64": "aarch64-unknown-linux-musl",
  "linux-x64": "x86_64-unknown-linux-musl",
  "win32-arm64": "aarch64-pc-windows-msvc",
  "win32-x64": "x86_64-pc-windows-msvc",
};

const repository = { type: "git", url: "git+https://github.com/cargopete/kitbag.git" };
const common = {
  version,
  license: "MIT",
  homepage: "https://github.com/cargopete/kitbag",
  bugs: "https://github.com/cargopete/kitbag/issues",
  repository,
};

const sha256 = (file) => createHash("sha256").update(readFileSync(file)).digest("hex");

rmSync(out, { recursive: true, force: true });
mkdirSync(out, { recursive: true });

const optionalDependencies = {};
for (const [key, target] of Object.entries(PLATFORMS)) {
  const [os, cpu] = key.split("-");
  const windows = os === "win32";
  const archive = `kitbag-${target}.${windows ? "zip" : "tar.gz"}`;
  const archivePath = join(assets, archive);
  const sumPath = join(assets, `kitbag-${target}.sha256`);
  if (!existsSync(archivePath) || !existsSync(sumPath)) throw new Error(`missing ${archive} or its .sha256 in ${assets}`);
  const expected = readFileSync(sumPath, "utf8").trim().split(/\s+/)[0].toLowerCase();
  const actual = sha256(archivePath);
  if (expected !== actual) throw new Error(`checksum mismatch for ${archive}: ${actual} != ${expected}`);

  const tmp = mkdtempSync(join(tmpdir(), "kitbag-npm-"));
  if (windows) execFileSync("unzip", ["-q", "-o", archivePath, "-d", tmp]);
  else execFileSync("tar", ["-xzf", archivePath, "-C", tmp]);
  const exe = windows ? "kitbag.exe" : "kitbag";
  const name = `${scope}/kitbag-${key}`;
  const dir = join(out, `kitbag-${key}`);
  mkdirSync(join(dir, "bin"), { recursive: true });
  copyFileSync(join(tmp, exe), join(dir, "bin", exe));
  if (!windows) chmodSync(join(dir, "bin", exe), 0o755);
  rmSync(tmp, { recursive: true, force: true });
  writeFileSync(
    join(dir, "package.json"),
    JSON.stringify(
      { name, ...common, description: `The ${key} binary for kitbag (installed automatically by ${scope}/kitbag).`, os: [os], cpu: [cpu], files: ["bin/"], preferUnplugged: true },
      null,
      2,
    ) + "\n",
  );
  writeFileSync(join(dir, "README.md"), `# ${name}\n\nThe \`${target}\` binary of [kitbag](https://github.com/cargopete/kitbag). Install \`${scope}/kitbag\` instead; npm picks this package for you.\n`);
  copyFileSync(join(here, "..", "..", "LICENSE"), join(dir, "LICENSE"));
  optionalDependencies[name] = version;
}

const main = join(out, "kitbag");
mkdirSync(join(main, "bin"), { recursive: true });
copyFileSync(join(here, "kitbag.js"), join(main, "bin", "kitbag.js"));
chmodSync(join(main, "bin", "kitbag.js"), 0o755);
writeFileSync(
  join(main, "package.json"),
  JSON.stringify(
    {
      name: `${scope}/kitbag`,
      ...common,
      description: "Pack your AI agent setup once, unpack it into any coding tool (Claude Code, Cursor, Codex CLI, Gemini CLI, Claude Desktop).",
      keywords: ["mcp", "claude-code", "cursor", "codex", "gemini-cli", "ai-agents", "dotfiles", "cli"],
      bin: { kitbag: "bin/kitbag.js" },
      files: ["bin/"],
      engines: { node: ">=16" },
      optionalDependencies,
    },
    null,
    2,
  ) + "\n",
);
copyFileSync(join(here, "..", "..", "README.md"), join(main, "README.md"));
copyFileSync(join(here, "..", "..", "LICENSE"), join(main, "LICENSE"));
console.log(`built ${Object.keys(PLATFORMS).length + 1} packages for ${scope}/kitbag@${version} in ${out}`);
