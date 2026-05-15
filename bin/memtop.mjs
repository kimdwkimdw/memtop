#!/usr/bin/env node
import { existsSync } from "node:fs";
import { spawnSync } from "node:child_process";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const binaryName = process.platform === "win32" ? "memtop.exe" : "memtop";
const releaseBinary = resolve(root, "target", "release", binaryName);
const args = process.argv.slice(2);
if (args[0] === "--") {
  args.shift();
}

let result;
if (existsSync(releaseBinary)) {
  result = run(releaseBinary, args);
} else {
  result = run(process.env.CARGO || "cargo", ["run", "--quiet", "--", ...args]);
  if (result.error?.code === "ENOENT") {
    result = run("mise", ["exec", "--", "cargo", "run", "--quiet", "--", ...args]);
  }
}

if (result.error) {
  console.error(`failed to run memtop: ${result.error.message}`);
  process.exit(127);
}

if (result.signal) {
  console.error(`memtop exited from signal ${result.signal}`);
  process.exit(1);
}

process.exit(result.status ?? 1);

function run(command, commandArgs) {
  return spawnSync(command, commandArgs, {
    cwd: root,
    env: process.env,
    stdio: "inherit",
  });
}
