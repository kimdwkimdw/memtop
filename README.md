# memtop — a treemap-based memory monitor for your terminal

Live TUI treemap for Linux process memory, grouped by inferred project and then by process.

## Run

```bash
pnpm start
bun run start
```

For a non-interactive snapshot:

```bash
pnpm once
bun run once
```

Build the release binary:

```bash
pnpm build
```

After a release build, the package CLI wrapper runs `target/release/memtop` directly. Before that, it falls back to `cargo run`.

## Controls

- `q` or `Esc`: quit
- `r`: refresh now
- Mouse left-click: select the project or process tile under the cursor
- `Up`/`Down` or `k`/`j`: move selection
- `Enter` or `z`: zoom into the selected project and show its process treemap
- `Backspace` or `x`: zoom back out to the project treemap

## Options

```bash
memtop --help
memtop --interval-ms 1000 --metric pss --min-memory-kib 4096 --scan-threads 4 --top-projects 30 --top-processes 10
```

Project detection uses common repository markers such as `.git`, `Cargo.toml`, `package.json`, `pyproject.toml`, `go.mod`, and `mise.toml`. If no marker is visible, paths under folders such as `prj`, `projects`, `repo`, or `workspaces` are grouped by the next path component.

The TUI starts with a fast RSS preview so the first screen appears quickly, then refreshes to the requested metric in the background. The default metric is PSS from `/proc/<pid>/smaps_rollup`. PSS divides shared pages across the processes that map them, which avoids the large double counting you see when Python multiprocessing or model workers share memory. Use `--metric rss` only when you want the fastest RSS view and accept duplicate shared-page accounting. Use `--metric uss` to see private memory only. If `smaps_rollup` is unavailable for a process, that process falls back to RSS.

`--scan-threads` controls PSS/USS collection cost. The default `4` keeps CPU usage bounded. Use `--scan-threads 0` to use all available parallelism for faster exact snapshots at higher CPU and memory cost.

Examples:

```bash
memtop --scan-threads 2      # lower CPU background PSS refresh
memtop --scan-threads 0      # fastest PSS refresh, higher CPU and memory pressure
memtop --metric rss          # fastest mode, but shared memory is double-counted
```
