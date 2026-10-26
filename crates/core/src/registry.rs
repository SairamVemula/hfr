//! Remembers which local folder a project on the VPS came from, so picking the same folder again finds its project
//! instead of uploading everything a second time. Stored as `projects.json` next to the config file.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    /// The folder as it was picked, in a canonical form.
    pub folder: String,
    pub project_id: String,
    pub name: String,
    pub last_sync_unix: u64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Registry {
    #[serde(skip)]
    file: PathBuf,
    entries: Vec<Entry>,
}

/// Canonical text form of a folder: absolute, symlinks resolved, no Windows `\\?\` prefix.
pub fn normalize(p: &Path) -> String {
    let c = std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let s = c.to_string_lossy().into_owned();
    s.strip_prefix(r"\\?\").map(str::to_string).unwrap_or(s)
}

/// Comparison key: case-insensitive where the file system is.
fn key(folder: &str) -> String {
    if cfg!(any(windows, target_os = "macos")) {
        folder.to_lowercase()
    } else {
        folder.to_string()
    }
}

impl Registry {
    pub fn load() -> Registry {
        Self::load_from(&crate::config::data_dir().join("projects.json"))
    }

    pub fn load_from(file: &Path) -> Registry {
        let mut r: Registry = std::fs::read(file).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        r.file = file.to_path_buf();
        r
    }

    pub fn save(&self) -> std::io::Result<()> {
        if let Some(dir) = self.file.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&self.file, serde_json::to_vec_pretty(self).map_err(std::io::Error::other)?)
    }

    /// The project this folder was uploaded as, if any.
    pub fn find_folder(&self, folder: &Path) -> Option<&Entry> {
        let k = key(&normalize(folder));
        self.entries.iter().find(|e| key(&e.folder) == k)
    }

    /// The local folder a project came from (shown under its name).
    pub fn folder_of(&self, project_id: &str) -> Option<&str> {
        self.entries.iter().find(|e| e.project_id == project_id).map(|e| e.folder.as_str())
    }

    /// Records (or updates) the project a folder belongs to.
    pub fn upsert(&mut self, folder: &Path, project_id: &str, name: &str, last_sync_unix: u64) {
        let folder = normalize(folder);
        let k = key(&folder);
        self.entries.retain(|e| key(&e.folder) != k && e.project_id != project_id);
        self.entries.push(Entry { folder, project_id: project_id.into(), name: name.into(), last_sync_unix });
    }

    pub fn forget_project(&mut self, project_id: &str) {
        self.entries.retain(|e| e.project_id != project_id);
    }

    /// Drops entries whose project no longer exists on the VPS.
    pub fn retain_projects(&mut self, alive: &dyn Fn(&str) -> bool) {
        self.entries.retain(|e| alive(&e.project_id));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_folder_finds_its_project_and_different_folders_do_not() {
        let base = std::env::temp_dir().join(format!("hfr-reg-{}", std::process::id()));
        let (a, b) = (base.join("one/demo"), base.join("two/demo"));
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        let file = base.join("projects.json");
        let mut r = Registry::load_from(&file);
        r.upsert(&a, "p1", "demo", 10);
        r.upsert(&b, "p2", "demo", 20);
        assert_eq!(r.find_folder(&a).unwrap().project_id, "p1");
        assert_eq!(r.find_folder(&b).unwrap().project_id, "p2", "same name, other folder: a separate project");
        r.save().unwrap();
        let again = Registry::load_from(&file);
        assert_eq!(again.find_folder(&a).unwrap().project_id, "p1");
        assert!(again.folder_of("p2").is_some());
        let mut r2 = again;
        r2.upsert(&a, "p3", "demo", 30); // re-uploaded as a new project: the folder now points to it
        assert_eq!(r2.find_folder(&a).unwrap().project_id, "p3");
        r2.forget_project("p2");
        assert!(r2.find_folder(&b).is_none());
        let _ = std::fs::remove_dir_all(&base);
    }
}
