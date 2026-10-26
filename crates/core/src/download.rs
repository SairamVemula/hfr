//! Downloading a render output: resumable, never overwrites, verified.
//!
//! While a download runs it is written to `<name>.hfrpart` next to a small `<name>.hfrpart.json` that says which
//! remote file it belongs to. Pausing (or a dropped connection) leaves both in place; starting the same download
//! again continues from the bytes already there. A finished file is renamed to its final name and checked against
//! the SHA-256 the agent recorded.

use std::{
    io::Read,
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{bail, Context, Result};
use futures_util::StreamExt;
use hfr_proto::OutputFile;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;

use crate::{Cancelled, Client, Phase, Transfer};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct PartMeta {
    project_id: String,
    job_id: String,
    path: String,
    size: u64,
    final_name: String,
}

/// Where a download writes, and how much of it already exists.
#[derive(Debug, Clone)]
pub struct DownloadPlan {
    pub final_path: PathBuf,
    pub part: PathBuf,
    pub meta: PathBuf,
    pub resume_from: u64,
}

const PART_EXT: &str = "hfrpart";

fn part_paths(dir: &Path, final_name: &str) -> (PathBuf, PathBuf) {
    (dir.join(format!("{final_name}.{PART_EXT}")), dir.join(format!("{final_name}.{PART_EXT}.json")))
}

/// `name.mp4`, or `name (1).mp4`, `name (2).mp4`... when something of that name (or an unfinished download of
/// it) already exists. An existing file is never overwritten.
pub fn unique_name(dir: &Path, name: &str) -> String {
    let taken = |n: &str| dir.join(n).exists() || part_paths(dir, n).0.exists();
    if !taken(name) {
        return name.to_string();
    }
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    };
    (1..).map(|i| format!("{stem} ({i}){ext}")).find(|n| !taken(n)).unwrap()
}

/// Decides the file names for a download and how far a previous attempt got.
pub fn plan(dir: &Path, project_id: &str, job_id: &str, file: &OutputFile) -> Result<DownloadPlan> {
    std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    let me = |final_name: &str| PartMeta {
        project_id: project_id.into(),
        job_id: job_id.into(),
        path: file.path.clone(),
        size: file.size,
        final_name: final_name.into(),
    };
    // An unfinished download of exactly this file?
    for e in std::fs::read_dir(dir)?.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if !name.ends_with(&format!(".{PART_EXT}.json")) {
            continue;
        }
        let Some(existing) = std::fs::read(e.path()).ok().and_then(|b| serde_json::from_slice::<PartMeta>(&b).ok()) else { continue };
        if existing == me(&existing.final_name) {
            let (part, meta) = part_paths(dir, &existing.final_name);
            let resume_from = std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
            return Ok(DownloadPlan { final_path: dir.join(&existing.final_name), part, meta, resume_from });
        }
    }
    let final_name = unique_name(dir, &file.name);
    let (part, meta) = part_paths(dir, &final_name);
    std::fs::write(&meta, serde_json::to_vec(&me(&final_name))?)?;
    Ok(DownloadPlan { final_path: dir.join(&final_name), part, meta, resume_from: 0 })
}

/// Throws an unfinished download away (the user chose Cancel instead of Pause).
pub fn discard(p: &DownloadPlan) {
    let _ = std::fs::remove_file(&p.part);
    let _ = std::fs::remove_file(&p.meta);
}

#[derive(Debug, Clone)]
pub struct Downloaded {
    pub path: PathBuf,
    pub bytes: u64,
    /// False when the agent had no checksum to compare with (only the size was checked).
    pub verified: bool,
}

fn sha256_counted(path: &Path, t: &Transfer) -> Result<String> {
    let mut f = std::fs::File::open(path)?;
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

/// Downloads one output file into `dir`. Cancelling `t` stops it and keeps the partial file (that is "pause");
/// call [`discard`] with the same [`plan`] to throw it away instead.
pub async fn download_file(
    client: &Client,
    project_id: &str,
    job_id: &str,
    file: &OutputFile,
    dir: &Path,
    t: Arc<Transfer>,
) -> Result<Downloaded> {
    let p = plan(dir, project_id, job_id, file)?;
    let res = run(client, project_id, job_id, file, &p, &t).await;
    match res {
        Ok(d) => {
            t.set_phase(Phase::Done);
            Ok(d)
        }
        Err(_) if t.is_cancelled() => Err(Cancelled.into()),
        Err(e) => Err(e),
    }
}

async fn run(client: &Client, pid: &str, jid: &str, file: &OutputFile, p: &DownloadPlan, t: &Arc<Transfer>) -> Result<Downloaded> {
    let mut have = p.resume_from;
    if have > file.size {
        // Something is off with the leftovers: start over.
        tokio::fs::remove_file(&p.part).await.ok();
        have = 0;
    }
    t.begin_at(Phase::Downloading, file.size, have);

    if have < file.size {
        let r = client.file_request(pid, jid, &file.path, have).send().await?;
        let status = r.status();
        let r = Client::ok(r).await?;
        // 206 continues where we stopped; a plain 200 means the whole file is coming again.
        let append = have > 0 && status == reqwest::StatusCode::PARTIAL_CONTENT;
        if !append {
            t.begin_at(Phase::Downloading, file.size, 0);
        }
        let mut f = tokio::fs::OpenOptions::new().create(true).write(true).append(append).truncate(!append).open(&p.part).await?;
        let mut stream = r.bytes_stream();
        loop {
            let next = tokio::select! {
                n = stream.next() => n,
                _ = t.cancelled() => { f.flush().await.ok(); bail!(Cancelled) }
            };
            let Some(chunk) = next else { break };
            let chunk = chunk?;
            f.write_all(&chunk).await?;
            t.add_done(chunk.len() as u64);
        }
        f.flush().await?;
        drop(f);
    }

    let size_now = tokio::fs::metadata(&p.part).await?.len();
    if size_now != file.size {
        // Keep the partial file so Resume can continue.
        bail!("the connection ended early ({size_now} of {} bytes); resume the download to continue", file.size);
    }

    // Verify. The hash is computed on the VPS after a render ends, so it may not have been known when the list
    // was fetched: ask again once.
    let expected = match &file.sha256 {
        Some(s) => Some(s.clone()),
        None => client.job_files(pid, jid).await.ok().and_then(|v| v.into_iter().find(|f| f.path == file.path)).and_then(|f| f.sha256),
    };
    t.begin(Phase::Verifying, file.size);
    let (part, tt) = (p.part.clone(), t.clone());
    let actual = tokio::task::spawn_blocking(move || sha256_counted(&part, &tt)).await??;
    let verified = match &expected {
        Some(e) if *e != actual => {
            discard(p);
            bail!("download verification failed: the file on this computer does not match the one on the VPS (sha256 {actual})");
        }
        Some(_) => true,
        None => false,
    };

    // Final name: the planned one, unless something took it meanwhile.
    let mut dest = p.final_path.clone();
    if dest.exists() {
        let dir = dest.parent().unwrap_or(Path::new("."));
        dest = dir.join(unique_name(dir, &file.name));
    }
    tokio::fs::rename(&p.part, &dest).await?;
    let _ = tokio::fs::remove_file(&p.meta).await;
    Ok(Downloaded { path: dest, bytes: file.size, verified })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "hfr-dl-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn file(path: &str, size: u64) -> OutputFile {
        OutputFile { path: path.into(), name: path.rsplit('/').next().unwrap().into(), size, created_unix: 0, sha256: None }
    }

    #[test]
    fn existing_files_are_never_overwritten() {
        let d = tmp();
        assert_eq!(unique_name(&d, "a.mp4"), "a.mp4");
        std::fs::write(d.join("a.mp4"), "x").unwrap();
        assert_eq!(unique_name(&d, "a.mp4"), "a (1).mp4");
        std::fs::write(d.join("a (1).mp4"), "x").unwrap();
        assert_eq!(unique_name(&d, "a.mp4"), "a (2).mp4");
        std::fs::write(d.join("noext"), "x").unwrap();
        assert_eq!(unique_name(&d, "noext"), "noext (1)");
        // An unfinished download also reserves its name.
        std::fs::write(d.join("b.mp4.hfrpart"), "x").unwrap();
        assert_eq!(unique_name(&d, "b.mp4"), "b (1).mp4");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn same_file_resumes_and_different_file_with_same_name_gets_its_own_part() {
        let d = tmp();
        let f1 = file("renders/out.mp4", 100);
        let p1 = plan(&d, "p", "j1", &f1).unwrap();
        assert_eq!(p1.resume_from, 0);
        std::fs::write(&p1.part, vec![0u8; 40]).unwrap();
        // Asking again for the same remote file continues the same part.
        let again = plan(&d, "p", "j1", &f1).unwrap();
        assert_eq!(again.resume_from, 40);
        assert_eq!(again.part, p1.part);
        // Another job's file with the same name must not continue (or clobber) it.
        let other = plan(&d, "p", "j2", &f1).unwrap();
        assert_eq!(other.resume_from, 0);
        assert_ne!(other.part, p1.part);
        assert_eq!(other.final_path.file_name().unwrap().to_string_lossy(), "out (1).mp4");
        discard(&other);
        assert!(!other.part.exists() && !other.meta.exists());
        assert!(p1.part.exists(), "discarding one download leaves the other alone");
        let _ = std::fs::remove_dir_all(&d);
    }
}
