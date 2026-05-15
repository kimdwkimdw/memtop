use std::{
    fmt, fs, io,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use clap::ValueEnum;
use rayon::{ThreadPoolBuilder, prelude::*};

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum MemoryMetric {
    /// Proportional Set Size. Shared pages are divided across sharers.
    Pss,
    /// Unique Set Size. Only private resident pages.
    Uss,
    /// Resident Set Size. Fast, but shared pages are counted for every process.
    Rss,
}

impl MemoryMetric {
    pub fn label(self) -> &'static str {
        match self {
            Self::Pss => "PSS",
            Self::Uss => "USS",
            Self::Rss => "RSS",
        }
    }
}

impl fmt::Display for MemoryMetric {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let value = match self {
            Self::Pss => "pss",
            Self::Uss => "uss",
            Self::Rss => "rss",
        };
        formatter.write_str(value)
    }
}

#[derive(Debug, Clone, Default)]
pub struct MemInfo {
    pub total_kib: u64,
    pub available_kib: u64,
}

#[derive(Debug, Clone)]
pub struct ProcessSample {
    pub pid: u32,
    pub ppid: u32,
    pub name: String,
    pub cmdline: Vec<String>,
    pub cwd: Option<PathBuf>,
    pub exe: Option<PathBuf>,
    pub memory_kib: u64,
    pub memory_source: MemoryMetric,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct ProcessMemory {
    rss_kib: u64,
    pss_kib: Option<u64>,
    uss_kib: Option<u64>,
}

impl ProcessMemory {
    fn select(&self, metric: MemoryMetric) -> (u64, MemoryMetric) {
        match metric {
            MemoryMetric::Pss => self
                .pss_kib
                .map(|value| (value, MemoryMetric::Pss))
                .unwrap_or((self.rss_kib, MemoryMetric::Rss)),
            MemoryMetric::Uss => self
                .uss_kib
                .map(|value| (value, MemoryMetric::Uss))
                .unwrap_or((self.rss_kib, MemoryMetric::Rss)),
            MemoryMetric::Rss => (self.rss_kib, MemoryMetric::Rss),
        }
    }
}

pub fn read_meminfo() -> Result<MemInfo> {
    let contents = fs::read_to_string("/proc/meminfo").context("failed to read /proc/meminfo")?;
    let mut info = MemInfo::default();

    for line in contents.lines() {
        if let Some(value) = parse_meminfo_value(line, "MemTotal:") {
            info.total_kib = value;
        } else if let Some(value) = parse_meminfo_value(line, "MemAvailable:") {
            info.available_kib = value;
        }
    }

    Ok(info)
}

pub fn collect_processes(
    metric: MemoryMetric,
    min_memory_kib: u64,
    scan_threads: usize,
) -> Result<Vec<ProcessSample>> {
    let mut pids = Vec::new();

    for entry in fs::read_dir("/proc").context("failed to read /proc")? {
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => continue,
        };
        let file_name = entry.file_name();
        let Some(pid) = file_name.to_str().and_then(|name| name.parse::<u32>().ok()) else {
            continue;
        };

        pids.push(pid);
    }

    let scan_threads = normalize_scan_threads(scan_threads);
    let mut processes: Vec<ProcessSample> = if metric == MemoryMetric::Rss || scan_threads <= 1 {
        pids.into_iter()
            .filter_map(|pid| read_process(pid, metric, min_memory_kib))
            .collect()
    } else {
        let pool = ThreadPoolBuilder::new()
            .num_threads(scan_threads)
            .build()
            .context("failed to build smaps scanner thread pool")?;
        pool.install(|| {
            pids.par_iter()
                .filter_map(|pid| read_process(*pid, metric, min_memory_kib))
                .collect()
        })
    };
    processes.sort_by_key(|process| process.pid);

    Ok(processes)
}

pub fn normalize_scan_threads(scan_threads: usize) -> usize {
    if scan_threads == 0 {
        std::thread::available_parallelism()
            .map(usize::from)
            .unwrap_or(1)
    } else {
        scan_threads
    }
}

fn read_process(pid: u32, metric: MemoryMetric, min_memory_kib: u64) -> Option<ProcessSample> {
    let proc_dir = Path::new("/proc").join(pid.to_string());
    let status = fs::read_to_string(proc_dir.join("status")).ok()?;
    let (name, ppid, rss_kib) = parse_status(&status)?;
    if rss_kib < min_memory_kib {
        return None;
    }

    let mut memory = if metric == MemoryMetric::Rss {
        ProcessMemory::default()
    } else {
        read_smaps_rollup(&proc_dir.join("smaps_rollup")).unwrap_or_default()
    };
    memory.rss_kib = rss_kib;
    let (memory_kib, memory_source) = memory.select(metric);

    Some(ProcessSample {
        pid,
        ppid,
        name,
        cmdline: read_cmdline(&proc_dir.join("cmdline")),
        cwd: read_link(proc_dir.join("cwd")),
        exe: read_link(proc_dir.join("exe")),
        memory_kib,
        memory_source,
    })
}

fn parse_status(status: &str) -> Option<(String, u32, u64)> {
    let mut name = None;
    let mut ppid = 0;
    let mut rss_kib = 0;

    for line in status.lines() {
        if let Some(value) = line.strip_prefix("Name:") {
            name = Some(value.trim().to_string());
        } else if let Some(value) = line.strip_prefix("PPid:") {
            ppid = value.trim().parse().unwrap_or(0);
        } else if let Some(value) = line.strip_prefix("VmRSS:") {
            rss_kib = parse_kib_value(value);
        }
    }

    name.map(|name| (name, ppid, rss_kib))
}

fn read_cmdline(path: &Path) -> Vec<String> {
    let Ok(bytes) = fs::read(path) else {
        return Vec::new();
    };

    bytes
        .split(|byte| *byte == 0)
        .filter(|part| !part.is_empty())
        .map(|part| String::from_utf8_lossy(part).into_owned())
        .collect()
}

fn read_link(path: PathBuf) -> Option<PathBuf> {
    match fs::read_link(path) {
        Ok(path) => Some(path),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(_) => None,
    }
}

fn read_smaps_rollup(path: &Path) -> Option<ProcessMemory> {
    let contents = fs::read_to_string(path).ok()?;
    Some(parse_smaps_rollup(&contents))
}

fn parse_smaps_rollup(contents: &str) -> ProcessMemory {
    let mut memory = ProcessMemory::default();
    let mut private_kib = 0;

    for line in contents.lines() {
        if let Some(value) = line.strip_prefix("Rss:") {
            memory.rss_kib = parse_kib_value(value);
        } else if let Some(value) = line.strip_prefix("Pss:") {
            memory.pss_kib = Some(parse_kib_value(value));
        } else if let Some(value) = line.strip_prefix("Private_Clean:") {
            private_kib += parse_kib_value(value);
        } else if let Some(value) = line.strip_prefix("Private_Dirty:") {
            private_kib += parse_kib_value(value);
        } else if let Some(value) = line.strip_prefix("Private_Hugetlb:") {
            private_kib += parse_kib_value(value);
        }
    }

    memory.uss_kib = Some(private_kib);
    memory
}

fn parse_meminfo_value(line: &str, key: &str) -> Option<u64> {
    line.strip_prefix(key).map(parse_kib_value)
}

fn parse_kib_value(value: &str) -> u64 {
    value
        .split_whitespace()
        .next()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_status_fields() {
        let status = "Name:\ttest-proc\nPPid:\t42\nVmRSS:\t2048 kB\n";
        let (name, ppid, rss_kib) = parse_status(status).unwrap();
        assert_eq!(name, "test-proc");
        assert_eq!(ppid, 42);
        assert_eq!(rss_kib, 2048);
    }

    #[test]
    fn parses_smaps_rollup_pss_and_uss() {
        let rollup = "\
Rss:                8192 kB
Pss:                4096 kB
Private_Clean:      1024 kB
Private_Dirty:      2048 kB
Private_Hugetlb:     512 kB
";
        let memory = parse_smaps_rollup(rollup);
        assert_eq!(memory.rss_kib, 8192);
        assert_eq!(memory.pss_kib, Some(4096));
        assert_eq!(memory.uss_kib, Some(3584));
    }

    #[test]
    fn pss_selection_falls_back_to_rss_when_rollup_is_missing() {
        let memory = ProcessMemory {
            rss_kib: 8192,
            pss_kib: None,
            uss_kib: None,
        };
        assert_eq!(memory.select(MemoryMetric::Pss), (8192, MemoryMetric::Rss));
    }
}
