mod human;
mod procfs;
mod project;
mod treemap;
mod ui;

use anyhow::Result;
use clap::Parser;

use crate::{
    human::{format_kib, percent, truncate},
    procfs::{
        MemoryMetric, collect_processes, effective_memory_metric, normalize_scan_threads,
        read_meminfo,
    },
    project::{GroupMode, ProjectNode, build_projects},
};

#[derive(Debug, Clone, Parser)]
#[command(
    name = "memtop",
    version,
    about = "Live project/process memory treemap for Linux and macOS"
)]
pub struct Args {
    #[arg(
        long,
        default_value_t = 2000,
        help = "Refresh interval in milliseconds"
    )]
    pub interval_ms: u64,

    #[arg(
        long,
        default_value_t = 1024,
        alias = "min-rss-kib",
        help = "Ignore processes below this selected memory metric in KiB"
    )]
    pub min_memory_kib: u64,

    #[arg(
        long,
        value_enum,
        default_value_t = MemoryMetric::Pss,
        help = "Memory metric to size tiles: pss avoids shared-page double counting, uss shows private pages, rss is fastest but double-counts shared pages"
    )]
    pub metric: MemoryMetric,

    #[arg(
        long,
        value_enum,
        default_value_t = GroupMode::Project,
        help = "Group processes by project context or Linux uid"
    )]
    pub group_by: GroupMode,

    #[arg(
        long,
        default_value_t = 4,
        help = "Max concurrent smaps_rollup reads for PSS/USS; 0 uses all available parallelism"
    )]
    pub scan_threads: usize,

    #[arg(long, default_value_t = 24, help = "Maximum group tiles to render")]
    pub top_projects: usize,

    #[arg(long, default_value_t = 8, help = "Maximum process tiles per group")]
    pub top_processes: usize,

    #[arg(
        long,
        help = "Print a single textual snapshot instead of opening the TUI"
    )]
    pub once: bool,
}

#[derive(Debug, Clone)]
pub struct Snapshot {
    pub metric: MemoryMetric,
    pub requested_metric: MemoryMetric,
    pub group_by: GroupMode,
    pub mem_total_kib: u64,
    pub mem_available_kib: u64,
    pub observed_memory_kib: u64,
    pub filtered_process_count: usize,
    pub fallback_process_count: usize,
    pub projects: Vec<ProjectNode>,
}

fn main() -> Result<()> {
    let args = Args::parse();

    if args.once {
        print_once(&args)?;
        return Ok(());
    }

    ui::run(args)
}

pub fn collect_snapshot(
    min_memory_kib: u64,
    metric: MemoryMetric,
    requested_metric: MemoryMetric,
    group_by: GroupMode,
    scan_threads: usize,
) -> Result<Snapshot> {
    let metric = effective_memory_metric(metric);
    let group_by = group_by.effective();
    let meminfo = read_meminfo()?;
    let processes = collect_processes(metric, min_memory_kib, scan_threads)?;
    let fallback_process_count = processes
        .iter()
        .filter(|process| process.memory_kib >= min_memory_kib && process.memory_source != metric)
        .count();
    let projects = build_projects(processes, min_memory_kib, group_by);
    let observed_memory_kib = projects
        .iter()
        .map(|project| project.total_memory_kib)
        .sum();
    let filtered_process_count = projects
        .iter()
        .map(|project| project.processes.len())
        .sum::<usize>();

    Ok(Snapshot {
        metric,
        requested_metric,
        group_by,
        mem_total_kib: meminfo.total_kib,
        mem_available_kib: meminfo.available_kib,
        observed_memory_kib,
        filtered_process_count,
        fallback_process_count,
        projects,
    })
}

fn print_once(args: &Args) -> Result<()> {
    let snapshot = collect_snapshot(
        args.min_memory_kib,
        args.metric,
        args.metric,
        args.group_by,
        args.scan_threads,
    )?;
    let used_kib = snapshot
        .mem_total_kib
        .saturating_sub(snapshot.mem_available_kib);

    println!("memtop snapshot");
    println!(
        "process {} sum: {}, system used: {} ({:.1}% of total)",
        snapshot.metric.label(),
        format_kib(snapshot.observed_memory_kib),
        format_kib(used_kib),
        percent(used_kib, snapshot.mem_total_kib)
    );
    if snapshot.metric != snapshot.requested_metric {
        println!(
            "{} is unavailable on this platform; using {} instead",
            snapshot.requested_metric.label(),
            snapshot.metric.label()
        );
    }
    println!(
        "{} {}, {} processes >= {} {}",
        snapshot.projects.len(),
        snapshot.group_by.plural_label(),
        snapshot.filtered_process_count,
        format_kib(args.min_memory_kib),
        snapshot.metric.label()
    );
    println!("grouping: {}", snapshot.group_by.label());
    if snapshot.fallback_process_count > 0 {
        println!(
            "{} processes fell back to RSS because smaps_rollup was unavailable",
            snapshot.fallback_process_count
        );
    }
    if snapshot.metric != MemoryMetric::Rss {
        println!(
            "smaps scan threads: {}",
            normalize_scan_threads(args.scan_threads)
        );
    }

    for project in snapshot.projects.iter().take(args.top_projects) {
        println!(
            "\n{:>10} {:>5.1}%  {}  ({})",
            format_kib(project.total_memory_kib),
            percent(project.total_memory_kib, snapshot.observed_memory_kib),
            project.name,
            project.path
        );

        for process in project.processes.iter().take(args.top_processes) {
            println!(
                "  {:>10}  pid {:<7} ppid {:<7} {}",
                format_kib(process.memory_kib),
                process.pid,
                process.ppid,
                truncate(&process.command, 180)
            );
        }
    }

    Ok(())
}
