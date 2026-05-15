#!/usr/bin/env node
import { existsSync, readdirSync, statSync } from "node:fs";
import { spawnSync, type SpawnSyncReturns } from "node:child_process";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..", "..");
const binaryName = process.platform === "win32" ? "memtop.exe" : "memtop";
const releaseBinary = resolve(root, "target", "release", binaryName);
const bundledBinary = bundledBinaryPath();
const args = process.argv.slice(2);
if (args[0] === "--") {
  args.shift();
}

let result: SpawnSyncReturns<Buffer>;
if (bundledBinary && existsSync(bundledBinary)) {
  result = run(bundledBinary, args);
} else if (isFreshRelease(releaseBinary)) {
  result = run(releaseBinary, args);
} else {
  result = run(process.env.CARGO ?? "cargo", [
    "run",
    "--quiet",
    "--",
    ...args,
  ]);
  if (isEnoent(result.error)) {
    result = run("mise", [
      "exec",
      "--",
      "cargo",
      "run",
      "--quiet",
      "--",
      ...args,
    ]);
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

function run(command: string, commandArgs: string[]): SpawnSyncReturns<Buffer> {
  return spawnSync(command, commandArgs, {
    cwd: root,
    env: process.env,
    stdio: "inherit",
  });
}

function isEnoent(error: Error | undefined): boolean {
  return error instanceof Error && "code" in error && error.code === "ENOENT";
}

function isFreshRelease(binaryPath: string): boolean {
  if (!existsSync(binaryPath)) {
    return false;
  }

  const binaryMtime = statSync(binaryPath).mtimeMs;
  return rustInputPaths().every(
    (inputPath) => statSync(inputPath).mtimeMs <= binaryMtime,
  );
}

function rustInputPaths(): string[] {
  const srcDir = resolve(root, "src");
  const srcFiles = readdirSync(srcDir, { withFileTypes: true })
    .filter((entry) => entry.isFile() && entry.name.endsWith(".rs"))
    .map((entry) => resolve(srcDir, entry.name));

  return [resolve(root, "Cargo.toml"), resolve(root, "Cargo.lock"), ...srcFiles];
}

function bundledBinaryPath(): string | null {
  const target = targetTriple();
  if (!target) {
    return null;
  }

  return resolve(root, "vendor", target, binaryName);
}

function targetTriple(): string | null {
  if (process.platform === "darwin") {
    if (process.arch === "arm64") {
      return "aarch64-apple-darwin";
    }
    if (process.arch === "x64") {
      return "x86_64-apple-darwin";
    }
  }

  if (process.platform === "linux") {
    if (process.arch === "arm64") {
      return "aarch64-unknown-linux-musl";
    }
    if (process.arch === "x64") {
      return "x86_64-unknown-linux-musl";
    }
  }

  return null;
}
