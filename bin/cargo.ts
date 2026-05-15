#!/usr/bin/env node
import { spawnSync, type SpawnSyncReturns } from "node:child_process";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..", "..");
const args = process.argv.slice(2);
if (args[0] === "--") {
  args.shift();
}

let result = run(process.env.CARGO ?? "cargo", args);
if (isEnoent(result.error)) {
  result = run("mise", ["exec", "--", "cargo", ...args]);
}

if (result.error) {
  console.error(`failed to run cargo: ${result.error.message}`);
  process.exit(127);
}

if (result.signal) {
  console.error(`cargo exited from signal ${result.signal}`);
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
