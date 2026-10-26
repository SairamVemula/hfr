use serde::{Deserialize, Serialize};

/// Version of the HTTP API. Clients check it and refuse to talk to an older agent.
pub const API_VERSION: u32 = 2;

/// Oldest agent API this client still works with. Raise it only when support for an older protocol is dropped; an
/// app update then does not force an upgrade of the VPS agent.
pub const MIN_API: u32 = 2;

/// Text compiled into every agent binary (`hfr-agent-api=<version>`) so a client can tell which API a Linux binary
/// speaks without running it. Keep in step with `API_VERSION` (a test checks it).
pub const AGENT_MARKER: &str = "hfr-agent-api=2";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectState {
    /// Created, no files yet.
    Empty,
    /// Files are being synced; no render can start meanwhile.
    Syncing,
    Ready,
}

/// A project is a folder of files on the VPS. It has any number of render jobs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub id: String,
    pub name: String,
    pub created_unix: u64,
    /// Size of the project folder, render outputs included.
    pub size_bytes: u64,
    pub state: ProjectState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Running,
    Done,
    Failed,
    /// Stopped by the user while it was running.
    Cancelled,
}

impl JobState {
    pub fn is_terminal(self) -> bool {
        !matches!(self, JobState::Running)
    }
}

/// A video (or GIF) a render produced.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OutputFile {
    /// Path inside the project, always with `/`, e.g. `renders/intro_2026-10-25.mp4`.
    pub path: String,
    pub name: String,
    pub size: u64,
    pub created_unix: u64,
    /// Filled in a moment after the render ends (hashing a big file takes a while).
    pub sha256: Option<String>,
}

/// One run of `npx hyperframes render ...` inside a project.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Job {
    pub id: String,
    pub project_id: String,
    pub state: JobState,
    /// Command run inside the project folder, e.g. `["npx","hyperframes","render","--fps","60"]`.
    pub command: Vec<String>,
    pub created_unix: u64,
    pub started_unix: Option<u64>,
    pub finished_unix: Option<u64>,
    pub files: Vec<OutputFile>,
}

/// A project with all its jobs: what the app shows, fetched in one call.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectView {
    pub project: Project,
    /// Newest first.
    pub jobs: Vec<Job>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateProject {
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateJob {
    pub command: Vec<String>,
}

/// A file of a project, as the sync protocol sees it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileEntry {
    pub path: String,
    pub size: u64,
    pub sha256: String,
}

/// What the client has to do to make the VPS copy match its local folder.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncPlan {
    pub upload: Vec<String>,
    pub delete: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeletePaths {
    pub paths: Vec<String>,
}

/// Decides which files must be uploaded or deleted. Used by the agent; also unit-tested here.
/// Only files that came from an earlier sync (`remote`) can be deleted, so render outputs are never touched.
pub fn plan_sync(local: &[FileEntry], remote: &[FileEntry]) -> SyncPlan {
    use std::collections::HashMap;
    let remote_by_path: HashMap<&str, &FileEntry> = remote.iter().map(|f| (f.path.as_str(), f)).collect();
    let local_paths: std::collections::HashSet<&str> = local.iter().map(|f| f.path.as_str()).collect();
    let upload = local
        .iter()
        .filter(|f| remote_by_path.get(f.path.as_str()).map(|r| r.size != f.size || r.sha256 != f.sha256).unwrap_or(true))
        .map(|f| f.path.clone())
        .collect();
    let delete = remote.iter().filter(|f| !local_paths.contains(f.path.as_str())).map(|f| f.path.clone()).collect();
    SyncPlan { upload, delete }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VersionInfo {
    pub api: u32,
    pub agent: String,
}

/// Cheap summary used by the installer to see whether the VPS is busy.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Status {
    pub running_jobs: usize,
    pub projects: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Metrics {
    pub cpu_total: f32,
    pub cpu_cores: Vec<f32>,
    pub mem_used: u64,
    pub mem_total: u64,
    pub swap_used: u64,
    pub swap_total: u64,
    pub load1: f64,
    pub disk_used: u64,
    pub disk_total: u64,
}

mod command;
pub use command::validate_command;

/// File extensions the agent treats as render output.
pub const OUTPUT_EXTS: &[&str] = &["mp4", "mov", "webm", "mkv", "gif"];

#[cfg(test)]
mod tests {
    use super::*;

    const _: () = assert!(MIN_API >= 1 && MIN_API <= API_VERSION);

    #[test]
    fn agent_marker_matches_api_version() {
        assert_eq!(AGENT_MARKER, format!("hfr-agent-api={API_VERSION}"));
    }

    fn f(path: &str, size: u64, sha: &str) -> FileEntry {
        FileEntry { path: path.into(), size, sha256: sha.into() }
    }

    #[test]
    fn sync_plan_uploads_new_and_changed_and_deletes_removed() {
        let local = [f("a.html", 1, "x"), f("b.js", 2, "new"), f("c.png", 3, "z")];
        let remote = [f("a.html", 1, "x"), f("b.js", 2, "old"), f("gone.txt", 9, "g")];
        let plan = plan_sync(&local, &remote);
        assert_eq!(plan.upload, ["b.js", "c.png"]);
        assert_eq!(plan.delete, ["gone.txt"]);
    }

    #[test]
    fn identical_folders_need_nothing() {
        let a = [f("a", 1, "x")];
        assert_eq!(plan_sync(&a, &a), SyncPlan::default());
    }

    #[test]
    fn empty_remote_uploads_everything() {
        let local = [f("a", 1, "x"), f("b", 2, "y")];
        assert_eq!(plan_sync(&local, &[]).upload, ["a", "b"]);
    }
}
