//! Delta sync of a local folder into a project on the VPS: hash locally (with a cache), ask the agent what differs,
//! pack and upload only that, and delete what was removed locally.

use std::{
    collections::{HashMap, HashSet},
    io::Read,
    path::{Path, PathBuf},
    sync::Arc,
    time::UNIX_EPOCH,
};

use anyhow::{bail, Context, Result};
use hfr_proto::FileEntry;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{Cancelled, Client, Phase, Transfer};

const DEFAULT_SKIP: &[&str] = &["node_modules", ".git"];

#[derive(Debug, Clone)]
pub struct LocalFile {
    /// Path inside the folder, with `/`.
    pub rel: String,
    pub abs: PathBuf,
    pub size: u64,
    pub mtime_ms: u64,
}

fn walk(dir: &Path) -> ignore::Walk {
    ignore::WalkBuilder::new(dir)
        .standard_filters(false)
        .add_custom_ignore_filename(".hfrignore")
        .filter_entry(|e| !DEFAULT_SKIP.iter().any(|s| e.file_name() == *s))
        .build()
}

/// All files of `dir` that would be synced (honouring `.hfrignore`, skipping `node_modules` and `.git`, no links).
pub fn list_local(dir: &Path, t: &Transfer) -> Result<Vec<LocalFile>> {
    let mut out = Vec::new();
    for e in walk(dir) {
        let e = e?;
        if t.is_cancelled() {
            bail!(Cancelled);
        }
        if !e.file_type().is_some_and(|ft| ft.is_file()) {
            continue;
        }
        let rel =
            e.path().strip_prefix(dir)?.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect::<Vec<_>>().join("/");
        let md = e.metadata()?;
        let mtime_ms = md.modified().ok().and_then(|m| m.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_millis() as u64).unwrap_or(0);
        out.push(LocalFile { rel, abs: e.path().to_path_buf(), size: md.len(), mtime_ms });
    }
    out.sort_by(|a, b| a.rel.cmp(&b.rel));
    Ok(out)
}

#[derive(Serialize, Deserialize)]
struct CacheEntry {
    size: u64,
    mtime_ms: u64,
    sha256: String,
}

/// Remembers hashes by (path, size, modified time) so unchanged multi-GB files are not read again on every sync.
pub struct HashCache {
    file: PathBuf,
    map: HashMap<String, CacheEntry>,
}

impl HashCache {
    pub fn load(data_dir: &Path) -> HashCache {
        let file = data_dir.join("hashcache.json");
        let map = std::fs::read(&file).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        HashCache { file, map }
    }

    pub fn save(&self) {
        if let Some(d) = self.file.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        if let Ok(b) = serde_json::to_vec(&self.map) {
            let _ = std::fs::write(&self.file, b);
        }
    }

    fn get(&self, f: &LocalFile) -> Option<String> {
        self.map
            .get(&f.abs.to_string_lossy().to_string())
            .filter(|c| c.size == f.size && c.mtime_ms == f.mtime_ms)
            .map(|c| c.sha256.clone())
    }

    fn put(&mut self, f: &LocalFile, sha: &str) {
        self.map.insert(f.abs.to_string_lossy().to_string(), CacheEntry { size: f.size, mtime_ms: f.mtime_ms, sha256: sha.to_string() });
    }

    /// Forget cached files below `prefix` that are no longer present.
    fn prune(&mut self, prefix: &Path, present: &[LocalFile]) {
        let keep: HashSet<String> = present.iter().map(|f| f.abs.to_string_lossy().to_string()).collect();
        let p = prefix.to_string_lossy().to_string();
        self.map.retain(|k, _| !k.starts_with(&p) || keep.contains(k));
    }
}

fn hash_counted(path: &Path, t: &Transfer) -> Result<String> {
    let mut f = std::fs::File::open(path).with_context(|| format!("cannot read {}", path.display()))?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        if t.is_cancelled() {
            bail!(Cancelled);
        }
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
        t.add_done(n as u64);
    }
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// Hashes the files, reading only those the cache does not know. Progress counts the bytes actually read.
pub fn hash_local(files: &[LocalFile], cache: &mut HashCache, t: &Transfer) -> Result<Vec<FileEntry>> {
    let to_read: u64 = files.iter().filter(|f| cache.get(f).is_none()).map(|f| f.size).sum();
    t.begin(Phase::Hashing, to_read);
    let mut out = Vec::with_capacity(files.len());
    for f in files {
        let sha256 = match cache.get(f) {
            Some(s) => s,
            None => {
                let s = hash_counted(&f.abs, t)?;
                cache.put(f, &s);
                s
            }
        };
        out.push(FileEntry { path: f.rel.clone(), size: f.size, sha256 });
    }
    Ok(out)
}

/// Reads a file for the archive, counting bytes and aborting promptly when cancelled.
struct CountingReader<'a> {
    inner: std::io::BufReader<std::fs::File>,
    t: &'a Transfer,
}

impl Read for CountingReader<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.t.is_cancelled() {
            return Err(std::io::Error::new(std::io::ErrorKind::Interrupted, "cancelled"));
        }
        let n = self.inner.read(buf)?;
        self.t.add_done(n as u64);
        Ok(n)
    }
}

/// Builds a tar holding exactly `files`.
pub fn pack_files(files: &[LocalFile], out: &Path, t: &Transfer) -> Result<()> {
    t.begin(Phase::Packing, files.iter().map(|f| f.size).sum());
    let mut b = tar::Builder::new(std::io::BufWriter::with_capacity(1 << 20, std::fs::File::create(out)?));
    b.follow_symlinks(false);
    for f in files {
        let file = std::fs::File::open(&f.abs)?;
        let mut header = tar::Header::new_gnu();
        header.set_metadata(&file.metadata()?);
        let reader = CountingReader { inner: std::io::BufReader::with_capacity(1 << 20, file), t };
        b.append_data(&mut header, &f.rel, reader)?;
    }
    b.into_inner()?.into_inner().map_err(|e| e.into_error())?;
    Ok(())
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SyncReport {
    pub total_files: usize,
    pub uploaded_files: usize,
    pub uploaded_bytes: u64,
    pub deleted_files: usize,
}

/// Makes the project on the VPS match `dir`, sending only what differs. Progress and cancellation go through `t`.
pub async fn sync_project(client: &Client, pid: &str, dir: &Path, t: Arc<Transfer>) -> Result<SyncReport> {
    let res = sync_inner(client, pid, dir, &t).await;
    match res {
        Ok(r) => {
            t.set_phase(Phase::Done);
            Ok(r)
        }
        Err(_) if t.is_cancelled() => Err(Cancelled.into()),
        Err(e) => Err(e),
    }
}

async fn sync_inner(client: &Client, pid: &str, dir: &Path, t: &Arc<Transfer>) -> Result<SyncReport> {
    let (d, tt) = (dir.to_path_buf(), t.clone());
    let (local, entries) = tokio::task::spawn_blocking(move || -> Result<(Vec<LocalFile>, Vec<FileEntry>)> {
        let mut cache = HashCache::load(&crate::config::data_dir());
        tt.begin(Phase::Scanning, 0);
        let files = list_local(&d, &tt)?;
        let hashed = hash_local(&files, &mut cache, &tt);
        cache.prune(&d, &files);
        cache.save(); // keep what was hashed even if this sync is cancelled
        Ok((files, hashed?))
    })
    .await??;
    if local.is_empty() {
        bail!("{} has no files to upload", dir.display());
    }

    let plan = client.sync_plan(pid, &entries).await?;
    let wanted: HashSet<&str> = plan.upload.iter().map(String::as_str).collect();
    let upload: Vec<LocalFile> = local.iter().filter(|f| wanted.contains(f.rel.as_str())).cloned().collect();
    let uploaded_bytes: u64 = upload.iter().map(|f| f.size).sum();

    if !upload.is_empty() {
        let tmp = std::env::temp_dir().join(format!(
            "hfr-sync-{}-{}.tar",
            std::process::id(),
            std::time::SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos()
        ));
        let (tmp2, tt) = (tmp.clone(), t.clone());
        let files = upload.clone();
        let sent = async {
            tokio::task::spawn_blocking(move || pack_files(&files, &tmp2, &tt)).await??;
            client.sync_upload(pid, &tmp, t).await
        }
        .await;
        let _ = tokio::fs::remove_file(&tmp).await;
        sent?;
    }
    if !plan.delete.is_empty() {
        client.sync_delete(pid, &plan.delete).await?;
    }
    Ok(SyncReport { total_files: local.len(), uploaded_files: upload.len(), uploaded_bytes, deleted_files: plan.delete.len() })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "hfr-sync-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn lists_files_honouring_ignore_rules_and_hashes_with_a_cache() {
        let d = tmp();
        std::fs::create_dir_all(d.join("assets")).unwrap();
        std::fs::create_dir_all(d.join("node_modules/x")).unwrap();
        std::fs::create_dir_all(d.join("renders")).unwrap();
        std::fs::write(d.join("index.html"), "hello").unwrap();
        std::fs::write(d.join("assets/a.txt"), "alpha").unwrap();
        std::fs::write(d.join("node_modules/x/n.js"), "n").unwrap();
        std::fs::write(d.join("renders/old.mp4"), "video").unwrap();
        std::fs::write(d.join(".hfrignore"), "renders/\n").unwrap();
        let t = Transfer::new();
        let files = list_local(&d, &t).unwrap();
        let names: Vec<&str> = files.iter().map(|f| f.rel.as_str()).collect();
        assert_eq!(names, [".hfrignore", "assets/a.txt", "index.html"]);

        let data = tmp();
        let mut cache = HashCache::load(&data);
        let first = hash_local(&files, &mut cache, &t).unwrap();
        assert_eq!(first.len(), 3);
        assert_eq!(first[2].sha256.len(), 64);
        cache.save();
        // Second run: everything comes from the cache, so no bytes are read.
        let t2 = Transfer::new();
        let mut cache2 = HashCache::load(&data);
        let second = hash_local(&files, &mut cache2, &t2).unwrap();
        assert_eq!(first, second);
        assert_eq!(t2.snapshot().total, 0, "nothing needed hashing");
        // A changed file is hashed again.
        std::fs::write(d.join("index.html"), "hello world").unwrap();
        let files3 = list_local(&d, &t).unwrap();
        let t3 = Transfer::new();
        let third = hash_local(&files3, &mut cache2, &t3).unwrap();
        assert_ne!(third[2].sha256, first[2].sha256);
        assert_eq!(t3.snapshot().total, "hello world".len() as u64);
        let _ = std::fs::remove_dir_all(&d);
        let _ = std::fs::remove_dir_all(&data);
    }

    #[test]
    fn pack_contains_only_the_requested_files() {
        let d = tmp();
        std::fs::write(d.join("a.txt"), "aaa").unwrap();
        std::fs::write(d.join("b.txt"), "bbb").unwrap();
        let t = Transfer::new();
        let all = list_local(&d, &t).unwrap();
        let only_b: Vec<LocalFile> = all.into_iter().filter(|f| f.rel == "b.txt").collect();
        let out = d.join("out.tar");
        pack_files(&only_b, &out, &t).unwrap();
        let mut ar = tar::Archive::new(std::fs::File::open(&out).unwrap());
        let names: Vec<String> = ar.entries().unwrap().map(|e| e.unwrap().path().unwrap().to_string_lossy().into_owned()).collect();
        assert_eq!(names, ["b.txt"]);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn cancelling_stops_hashing() {
        let d = tmp();
        std::fs::write(d.join("a.txt"), vec![1u8; 4 << 20]).unwrap();
        let t = Transfer::new();
        t.cancel();
        let files = vec![LocalFile { rel: "a.txt".into(), abs: d.join("a.txt"), size: 4 << 20, mtime_ms: 0 }];
        let err = hash_local(&files, &mut HashCache::load(&d), &t).unwrap_err();
        assert!(err.is::<Cancelled>());
        let _ = std::fs::remove_dir_all(&d);
    }
}
