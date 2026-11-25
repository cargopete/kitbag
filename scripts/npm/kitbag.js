#!/usr/bin/env node
// Runs the kitbag binary from the platform package npm installed alongside this one.
"use strict";
const { spawnSync } = require("child_process");
const path = require("path");

const scope = require("../package.json").name.replace(/\/kitbag$/, "");
const key = `${process.platform}-${process.arch}`;
const exe = process.platform === "win32" ? "kitbag.exe" : "kitbag";

function binaryPath() {
  if (process.env.KITBAG_BINARY) return process.env.KITBAG_BINARY;
  try {
    return require.resolve(`${scope}/kitbag-${key}/bin/${exe}`);
  } catch {
    console.error(
      `kitbag: no prebuilt binary for ${key}.\n` +
        `The optional dependency ${scope}/kitbag-${key} was not installed (unsupported platform, or ` +
        `installed with --no-optional / --omit=optional).\n` +
        `Other ways to install: https://github.com/cargopete/kitbag#install`,
    );
    process.exit(1);
  }
}

const result = spawnSync(binaryPath(), process.argv.slice(2), { stdio: "inherit", windowsHide: false });
if (result.error) {
  console.error(`kitbag: failed to run ${path.basename(exe)}: ${result.error.message}`);
  process.exit(1);
}
if (result.signal) process.kill(process.pid, result.signal);
process.exit(result.status ?? 1);
