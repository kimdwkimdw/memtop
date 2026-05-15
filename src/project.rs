use std::{
    collections::HashMap,
    env,
    ffi::OsStr,
    path::{Component, Path, PathBuf},
};

use crate::procfs::ProcessSample;

#[derive(Debug, Clone)]
pub struct ProjectNode {
    pub name: String,
    pub path: String,
    pub total_memory_kib: u64,
    pub processes: Vec<ProcessNode>,
}

#[derive(Debug, Clone)]
pub struct ProcessNode {
    pub pid: u32,
    pub ppid: u32,
    pub name: String,
    pub command: String,
    pub memory_kib: u64,
}

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
struct ProjectKey {
    name: String,
    path: String,
}

pub fn build_projects(processes: Vec<ProcessSample>, min_memory_kib: u64) -> Vec<ProjectNode> {
    let home = home_dir();
    let mut projects: HashMap<ProjectKey, ProjectNode> = HashMap::new();

    for process in processes {
        if process.memory_kib < min_memory_kib {
            continue;
        }

        let key = infer_project(&process, home.as_deref());
        let process_node = ProcessNode {
            pid: process.pid,
            ppid: process.ppid,
            name: process.name.clone(),
            command: command_line(&process),
            memory_kib: process.memory_kib,
        };

        let project = projects.entry(key.clone()).or_insert_with(|| ProjectNode {
            name: key.name,
            path: key.path,
            total_memory_kib: 0,
            processes: Vec::new(),
        });
        project.total_memory_kib += process.memory_kib;
        project.processes.push(process_node);
    }

    let mut projects: Vec<ProjectNode> = projects.into_values().collect();
    for project in &mut projects {
        project.processes.sort_by(|left, right| {
            right
                .memory_kib
                .cmp(&left.memory_kib)
                .then(left.pid.cmp(&right.pid))
        });
    }
    projects.sort_by(|left, right| {
        right
            .total_memory_kib
            .cmp(&left.total_memory_kib)
            .then(left.name.cmp(&right.name))
    });
    projects
}

fn infer_project(process: &ProcessSample, home: Option<&Path>) -> ProjectKey {
    if let Some(container_id) = process.container_id.as_deref() {
        return container_key(container_id);
    }

    if let Some(cwd) = process.cwd.as_deref()
        && let Some(key) = infer_from_path(cwd, home)
    {
        return key;
    }

    for path in command_paths(process) {
        if let Some(key) = infer_from_path(&path, home) {
            return key;
        }
    }

    if let Some(exe) = process.exe.as_deref()
        && let Some(key) = infer_from_path(exe, home)
    {
        return key;
    }

    if let Some(exe) = process.exe.as_deref()
        && let Some(parent) = exe.parent()
        && is_system_path(exe)
    {
        return ProjectKey {
            name: "system".to_string(),
            path: display_path(parent, home),
        };
    }

    if process.name.starts_with('[') && process.name.ends_with(']') {
        return ProjectKey {
            name: "kernel".to_string(),
            path: "kernel threads".to_string(),
        };
    }

    unknown_process_key(process)
}

fn infer_from_path(path: &Path, home: Option<&Path>) -> Option<ProjectKey> {
    if let Some(root) = find_project_root(path) {
        return Some(key_from_root(&root, home));
    }
    if let Some(parent) = path.parent()
        && let Some(root) = find_project_root(parent)
    {
        return Some(key_from_root(&root, home));
    }
    if let Some(root) = find_named_workspace_root(path) {
        return Some(key_from_root(&root, home));
    }
    if let Some(parent) = path.parent()
        && let Some(root) = find_named_workspace_root(parent)
    {
        return Some(key_from_root(&root, home));
    }
    None
}

fn command_paths(process: &ProcessSample) -> impl Iterator<Item = PathBuf> + '_ {
    process.cmdline.iter().filter_map(|argument| {
        let value = argument
            .split_once('=')
            .map(|(_, value)| value)
            .unwrap_or(argument);
        value.starts_with('/').then(|| PathBuf::from(value))
    })
}

fn find_project_root(path: &Path) -> Option<PathBuf> {
    let mut cursor = Some(path);
    while let Some(current) = cursor {
        if PROJECT_MARKERS
            .iter()
            .any(|marker| current.join(marker).exists())
        {
            return Some(current.to_path_buf());
        }
        cursor = current.parent();
    }
    None
}

fn find_named_workspace_root(path: &Path) -> Option<PathBuf> {
    let components: Vec<Component<'_>> = path.components().collect();

    for (index, component) in components.iter().enumerate() {
        if !is_workspace_component(component.as_os_str()) || index + 1 >= components.len() {
            continue;
        }

        let mut root = PathBuf::new();
        for component in &components[..=index + 1] {
            root.push(component.as_os_str());
        }
        return Some(root);
    }

    None
}

fn is_workspace_component(component: &OsStr) -> bool {
    matches!(
        component.to_str(),
        Some(
            "prj" | "project" | "projects" | "repo" | "repos" | "src" | "workspace" | "workspaces"
        )
    )
}

fn key_from_root(root: &Path, home: Option<&Path>) -> ProjectKey {
    let name = root
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("/")
        .to_string();

    ProjectKey {
        name,
        path: display_path(root, home),
    }
}

fn container_key(container_id: &str) -> ProjectKey {
    ProjectKey {
        name: format!("container {}", short_container_id(container_id)),
        path: format!("container {container_id}"),
    }
}

fn short_container_id(container_id: &str) -> &str {
    container_id.get(..12).unwrap_or(container_id)
}

fn unknown_process_key(process: &ProcessSample) -> ProjectKey {
    let name = if process.name.is_empty() {
        format!("process {}", process.pid)
    } else {
        format!("{} ({})", process.name, process.pid)
    };

    ProjectKey {
        name,
        path: "no accessible cwd/exe".to_string(),
    }
}

fn command_line(process: &ProcessSample) -> String {
    if process.cmdline.is_empty() {
        process.name.clone()
    } else {
        process.cmdline.join(" ")
    }
}

fn display_path(path: &Path, home: Option<&Path>) -> String {
    if let Some(home) = home
        && let Ok(relative) = path.strip_prefix(home)
    {
        if relative.as_os_str().is_empty() {
            return "~".to_string();
        }
        return format!("~/{}", relative.display());
    }

    if let (Ok(user), true) = (env::var("USER"), path.is_absolute()) {
        let nfs_home = PathBuf::from(format!("/nfs/home/{user}"));
        if let Ok(relative) = path.strip_prefix(&nfs_home) {
            if relative.as_os_str().is_empty() {
                return "~".to_string();
            }
            return format!("~/{}", relative.display());
        }
    }

    path.display().to_string()
}

fn home_dir() -> Option<PathBuf> {
    env::var_os("HOME").map(PathBuf::from)
}

fn is_system_path(path: &Path) -> bool {
    [
        "/bin",
        "/sbin",
        "/usr",
        "/lib",
        "/lib64",
        "/snap",
        "/nix/store",
    ]
    .iter()
    .any(|prefix| path.starts_with(prefix))
}

const PROJECT_MARKERS: &[&str] = &[
    ".git",
    "Cargo.toml",
    "package.json",
    "pyproject.toml",
    "go.mod",
    "pom.xml",
    "build.gradle",
    "Makefile",
    "mise.toml",
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_named_workspace_root() {
        let root = find_named_workspace_root(Path::new("/nfs/home/arthur/prj/memtop/src"));
        assert_eq!(root.unwrap(), PathBuf::from("/nfs/home/arthur/prj/memtop"));
    }

    #[test]
    fn groups_processes_by_project() {
        let processes = vec![
            ProcessSample {
                pid: 10,
                ppid: 1,
                name: "alpha".to_string(),
                cmdline: vec!["alpha".to_string()],
                cwd: Some(PathBuf::from("/nfs/home/arthur/prj/example")),
                exe: None,
                container_id: None,
                memory_kib: 2048,
                memory_source: crate::procfs::MemoryMetric::Pss,
            },
            ProcessSample {
                pid: 11,
                ppid: 1,
                name: "beta".to_string(),
                cmdline: vec!["beta".to_string()],
                cwd: Some(PathBuf::from("/nfs/home/arthur/prj/example/src")),
                exe: None,
                container_id: None,
                memory_kib: 1024,
                memory_source: crate::procfs::MemoryMetric::Pss,
            },
        ];

        let projects = build_projects(processes, 1);
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].name, "example");
        assert_eq!(projects[0].total_memory_kib, 3072);
        assert_eq!(projects[0].processes.len(), 2);
    }

    #[test]
    fn does_not_group_processes_without_project_evidence() {
        let processes = vec![
            ProcessSample {
                pid: 10,
                ppid: 1,
                name: "claude".to_string(),
                cmdline: vec!["claude".to_string()],
                cwd: None,
                exe: None,
                container_id: None,
                memory_kib: 2048,
                memory_source: crate::procfs::MemoryMetric::Pss,
            },
            ProcessSample {
                pid: 11,
                ppid: 1,
                name: "claude".to_string(),
                cmdline: vec!["claude".to_string()],
                cwd: None,
                exe: None,
                container_id: None,
                memory_kib: 1024,
                memory_source: crate::procfs::MemoryMetric::Pss,
            },
        ];

        let projects = build_projects(processes, 1);

        assert_eq!(projects.len(), 2);
        assert_eq!(projects[0].name, "claude (10)");
        assert_eq!(projects[0].processes.len(), 1);
        assert_eq!(projects[0].processes[0].pid, 10);
        assert_eq!(projects[1].name, "claude (11)");
        assert_eq!(projects[1].processes.len(), 1);
        assert_eq!(projects[1].processes[0].pid, 11);
    }

    #[test]
    fn groups_processes_by_docker_container_when_project_is_unknown() {
        let container_id =
            "c6e70a0867a1b7957698586b67fb03e411bd70d8e5c2b737c9cb08b951c31c6f".to_string();
        let processes = vec![
            ProcessSample {
                pid: 10,
                ppid: 1,
                name: "tritonserver".to_string(),
                cmdline: vec!["tritonserver".to_string()],
                cwd: None,
                exe: None,
                container_id: Some(container_id.clone()),
                memory_kib: 2048,
                memory_source: crate::procfs::MemoryMetric::Pss,
            },
            ProcessSample {
                pid: 11,
                ppid: 10,
                name: "triton_python_b".to_string(),
                cmdline: vec![
                    "/opt/tritonserver/backends/python/triton_python_backend_stub".to_string(),
                    "/models2/melo-preprocessor/1/model.py".to_string(),
                ],
                cwd: None,
                exe: None,
                container_id: Some(container_id),
                memory_kib: 1024,
                memory_source: crate::procfs::MemoryMetric::Pss,
            },
        ];

        let projects = build_projects(processes, 1);

        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].name, "container c6e70a0867a1");
        assert_eq!(
            projects[0].path,
            "container c6e70a0867a1b7957698586b67fb03e411bd70d8e5c2b737c9cb08b951c31c6f"
        );
        assert_eq!(projects[0].total_memory_kib, 3072);
        assert_eq!(projects[0].processes.len(), 2);
    }

    #[test]
    fn docker_container_takes_precedence_over_project_paths() {
        let container_id =
            "c6e70a0867a1b7957698586b67fb03e411bd70d8e5c2b737c9cb08b951c31c6f".to_string();
        let processes = vec![ProcessSample {
            pid: 10,
            ppid: 1,
            name: "python".to_string(),
            cmdline: vec!["python".to_string()],
            cwd: Some(PathBuf::from("/nfs/home/arthur/prj/example")),
            exe: None,
            container_id: Some(container_id),
            memory_kib: 2048,
            memory_source: crate::procfs::MemoryMetric::Pss,
        }];

        let projects = build_projects(processes, 1);

        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].name, "container c6e70a0867a1");
    }
}
