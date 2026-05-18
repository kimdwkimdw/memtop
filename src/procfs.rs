use std::{
    fmt,
    path::{Path, PathBuf},
};

use anyhow::Result;
use clap::ValueEnum;
#[cfg(target_os = "linux")]
use {
    anyhow::Context,
    rayon::{ThreadPoolBuilder, prelude::*},
    std::{fs, io},
};

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

pub fn effective_memory_metric(metric: MemoryMetric) -> MemoryMetric {
    if cfg!(target_os = "linux") && Path::new("/proc/meminfo").exists() {
        metric
    } else {
        MemoryMetric::Rss
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
    pub uid: Option<u32>,
    pub name: String,
    pub cmdline: Vec<String>,
    pub cwd: Option<PathBuf>,
    pub exe: Option<PathBuf>,
    pub container_id: Option<String>,
    pub memory_kib: u64,
    pub memory_source: MemoryMetric,
}

#[cfg(any(target_os = "linux", test))]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct ProcessMemory {
    rss_kib: u64,
    pss_kib: Option<u64>,
    uss_kib: Option<u64>,
}

#[cfg(any(target_os = "linux", test))]
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

#[cfg(target_os = "linux")]
pub fn read_meminfo() -> Result<MemInfo> {
    let Ok(contents) = fs::read_to_string("/proc/meminfo") else {
        return Ok(read_meminfo_sysinfo());
    };
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

#[cfg(not(target_os = "linux"))]
pub fn read_meminfo() -> Result<MemInfo> {
    Ok(read_meminfo_sysinfo())
}

fn read_meminfo_sysinfo() -> MemInfo {
    let mut system = sysinfo::System::new();
    system.refresh_memory();

    MemInfo {
        total_kib: bytes_to_kib(system.total_memory()),
        available_kib: bytes_to_kib(system.available_memory()),
    }
}

#[cfg(target_os = "linux")]
pub fn collect_processes(
    metric: MemoryMetric,
    min_memory_kib: u64,
    scan_threads: usize,
) -> Result<Vec<ProcessSample>> {
    let mut pids = Vec::new();

    let entries = match fs::read_dir("/proc") {
        Ok(entries) => entries,
        Err(_) => return Ok(collect_processes_sysinfo(min_memory_kib)),
    };

    for entry in entries {
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

#[cfg(not(target_os = "linux"))]
pub fn collect_processes(
    _metric: MemoryMetric,
    min_memory_kib: u64,
    _scan_threads: usize,
) -> Result<Vec<ProcessSample>> {
    Ok(collect_processes_sysinfo(min_memory_kib))
}

fn collect_processes_sysinfo(min_memory_kib: u64) -> Vec<ProcessSample> {
    let mut system = sysinfo::System::new_all();
    system.refresh_processes(sysinfo::ProcessesToUpdate::All, true);

    let mut processes: Vec<ProcessSample> = system
        .processes()
        .iter()
        .filter_map(|(pid, process)| {
            let memory_kib = bytes_to_kib(process.memory());
            if memory_kib < min_memory_kib {
                return None;
            }

            Some(ProcessSample {
                pid: pid.as_u32(),
                ppid: process.parent().map(|pid| pid.as_u32()).unwrap_or(0),
                uid: None,
                name: process.name().to_string_lossy().into_owned(),
                cmdline: process
                    .cmd()
                    .iter()
                    .map(|argument| argument.to_string_lossy().into_owned())
                    .collect(),
                cwd: process.cwd().map(Path::to_path_buf),
                exe: process.exe().map(Path::to_path_buf),
                container_id: None,
                memory_kib,
                memory_source: MemoryMetric::Rss,
            })
        })
        .collect();
    processes.sort_by_key(|process| process.pid);

    processes
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

#[cfg(target_os = "linux")]
fn read_process(pid: u32, metric: MemoryMetric, min_memory_kib: u64) -> Option<ProcessSample> {
    let proc_dir = Path::new("/proc").join(pid.to_string());
    let status = fs::read_to_string(proc_dir.join("status")).ok()?;
    let (name, ppid, uid, rss_kib) = parse_status(&status)?;
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
        uid,
        name,
        cmdline: read_cmdline(&proc_dir.join("cmdline")),
        cwd: read_link(proc_dir.join("cwd")),
        exe: read_link(proc_dir.join("exe")),
        container_id: read_container_id(&proc_dir.join("cgroup")),
        memory_kib,
        memory_source,
    })
}

#[cfg(any(target_os = "linux", test))]
fn parse_status(status: &str) -> Option<(String, u32, Option<u32>, u64)> {
    let mut name = None;
    let mut ppid = 0;
    let mut uid = None;
    let mut rss_kib = 0;

    for line in status.lines() {
        if let Some(value) = line.strip_prefix("Name:") {
            name = Some(value.trim().to_string());
        } else if let Some(value) = line.strip_prefix("PPid:") {
            ppid = value.trim().parse().unwrap_or(0);
        } else if let Some(value) = line.strip_prefix("Uid:") {
            uid = value
                .split_whitespace()
                .next()
                .and_then(|value| value.parse().ok());
        } else if let Some(value) = line.strip_prefix("VmRSS:") {
            rss_kib = parse_kib_value(value);
        }
    }

    name.map(|name| (name, ppid, uid, rss_kib))
}

#[cfg(target_os = "linux")]
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

#[cfg(target_os = "linux")]
fn read_link(path: PathBuf) -> Option<PathBuf> {
    match fs::read_link(path) {
        Ok(path) => Some(path),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(_) => None,
    }
}

#[cfg(target_os = "linux")]
fn read_container_id(path: &Path) -> Option<String> {
    let contents = fs::read_to_string(path).ok()?;
    parse_container_id(&contents)
}

#[cfg(any(target_os = "linux", test))]
fn parse_container_id(contents: &str) -> Option<String> {
    for line in contents.lines() {
        let Some(path) = line.rsplit(':').next() else {
            continue;
        };
        let components: Vec<&str> = path.split('/').filter(|part| !part.is_empty()).collect();

        if let Some(id) = docker_path_container_id(&components) {
            return Some(id.to_string());
        }

        if let Some(id) = scoped_container_id(&components) {
            return Some(id.to_string());
        }
    }

    None
}

#[cfg(any(target_os = "linux", test))]
fn docker_path_container_id<'a>(components: &'a [&str]) -> Option<&'a str> {
    components.windows(2).find_map(|window| {
        (window[0] == "docker" && is_container_id(window[1])).then_some(window[1])
    })
}

#[cfg(any(target_os = "linux", test))]
fn scoped_container_id<'a>(components: &'a [&str]) -> Option<&'a str> {
    components.iter().find_map(|component| {
        extract_scoped_container_id(component, "docker-")
            .or_else(|| extract_scoped_container_id(component, "cri-containerd-"))
            .or_else(|| extract_scoped_container_id(component, "crio-"))
            .or_else(|| extract_scoped_container_id(component, "libpod-"))
    })
}

#[cfg(any(target_os = "linux", test))]
fn extract_scoped_container_id<'a>(component: &'a str, prefix: &str) -> Option<&'a str> {
    let id = component
        .strip_prefix(prefix)
        .and_then(|value| value.strip_suffix(".scope"))?;
    is_container_id(id).then_some(id)
}

#[cfg(any(target_os = "linux", test))]
fn is_container_id(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[cfg(target_os = "linux")]
fn read_smaps_rollup(path: &Path) -> Option<ProcessMemory> {
    let contents = fs::read_to_string(path).ok()?;
    Some(parse_smaps_rollup(&contents))
}

#[cfg(any(target_os = "linux", test))]
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

#[cfg(target_os = "linux")]
fn parse_meminfo_value(line: &str, key: &str) -> Option<u64> {
    line.strip_prefix(key).map(parse_kib_value)
}

#[cfg(any(target_os = "linux", test))]
fn parse_kib_value(value: &str) -> u64 {
    value
        .split_whitespace()
        .next()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(0)
}

fn bytes_to_kib(bytes: u64) -> u64 {
    bytes / 1024
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_status_fields() {
        let status = "Name:\ttest-proc\nPPid:\t42\nUid:\t1000\t1000\t1000\t1000\nVmRSS:\t2048 kB\n";
        let (name, ppid, uid, rss_kib) = parse_status(status).unwrap();
        assert_eq!(name, "test-proc");
        assert_eq!(ppid, 42);
        assert_eq!(uid, Some(1000));
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

    #[test]
    fn parses_docker_container_id_from_cgroup_path() {
        let cgroup = "0::/../26f7c48a1dd0c9265b6b8929ba2f0237311d88bee6771f3c7799d77f7c3b45d3/docker/c6e70a0867a1b7957698586b67fb03e411bd70d8e5c2b737c9cb08b951c31c6f\n";
        assert_eq!(
            parse_container_id(cgroup).as_deref(),
            Some("c6e70a0867a1b7957698586b67fb03e411bd70d8e5c2b737c9cb08b951c31c6f")
        );
    }

    #[test]
    fn parses_systemd_scoped_container_id_from_cgroup_path() {
        let cgroup = "0::/system.slice/docker-c6e70a0867a1b7957698586b67fb03e411bd70d8e5c2b737c9cb08b951c31c6f.scope\n";
        assert_eq!(
            parse_container_id(cgroup).as_deref(),
            Some("c6e70a0867a1b7957698586b67fb03e411bd70d8e5c2b737c9cb08b951c31c6f")
        );
    }

    #[test]
    fn ignores_unmarked_cgroup_hex_components() {
        let cgroup = "0::/../f78869aa8bb617a13393a9ac532c68e4f1ed3a25f262bd8983bcaf1347654101\n";
        assert_eq!(parse_container_id(cgroup), None);
    }
}
