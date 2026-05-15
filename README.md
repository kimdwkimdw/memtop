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

`--scan-threads` controls PSS/USS collection cost. The default `4` keeps CPU usage bounded. Use `--scan-threads 0` to use all available parallelism for faster exact snapshots at higher CPU and memory cost.

Examples:

```bash
memtop --scan-threads 2      # lower CPU background PSS refresh
memtop --scan-threads 0      # fastest PSS refresh, higher CPU and memory pressure
memtop --metric rss          # fastest mode, but shared memory is double-counted
```

## Limitations

The first TUI frame can show inflated memory. This is intentional: when the requested metric is `pss` or `uss`, memtop opens with a fast RSS preview, then replaces it with the slower `smaps_rollup` result in the background.

Example from a real host:

```text
RSS preview: process RSS sum 979.6 GiB, system used 88.1 GiB
PSS refresh: process PSS sum  77.7 GiB, system used 88.7 GiB
```

The `979.6 GiB` preview is not host memory usage. It is the sum of per-process RSS, so shared pages are counted once for every process that maps them. In the same snapshot:

```text
training group: 963.9 GiB RSS -> 63.1 GiB PSS
training worker:  15.0 GiB RSS ->  1.5 GiB PSS
serving group:     2.9 GiB RSS ->  2.0 GiB PSS
serving worker:  692.6 MiB RSS -> 480.1 MiB PSS
```

The header has two totals with different meanings:

- `system used` comes from `/proc/meminfo` as `MemTotal - MemAvailable`. This is the kernel's host-wide memory pressure signal.
- `process <metric> sum` is the sum of sampled processes above `--min-memory-kib`. It drives treemap tile sizes. It is an attribution view, so it will not exactly match `system used`.

Metric examples:

- `rss`: reads `VmRSS` from `/proc/<pid>/status`. It is fast. If four workers map the same 4 GiB model, RSS reports about `4 GiB x 4 = 16 GiB`.
- `pss`: reads `Pss` from `/proc/<pid>/smaps_rollup`. It splits shared pages. The same 4 GiB model across four workers contributes about `1 GiB` to each worker, `4 GiB` total.
- `uss`: sums `Private_Clean + Private_Dirty + Private_Hugetlb` from `smaps_rollup`. The shared 4 GiB model contributes `0 GiB`; only private pages remain.

Reading `smaps_rollup` is slower because the kernel must summarize memory mappings for each process. `--scan-threads 4` keeps CPU use bounded; `--scan-threads 0` uses all available parallelism. On the example host, `--scan-threads 0` used `64` scanner threads. If `smaps_rollup` cannot be read, that process falls back to RSS; the example PSS snapshot reported `119` RSS fallbacks.
