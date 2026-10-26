//! Render output files: finding them, hashing them, serving them (with `Range` so downloads can resume) and
//! deleting them.

use std::{
    collections::HashSet,
    io::Read,
    path::{Path as FsPath, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use axum::{
    body::Body,
    extract::{Path, Query, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use hfr_proto::{OutputFile, OUTPUT_EXTS};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio_util::io::ReaderStream;

use crate::{ise, not_found, App, Res};

/// A project-relative path with `/` separators that cannot leave the project folder.
pub fn rel_ok(rel: &str) -> bool {
    !rel.is_empty()
        && !rel.contains(['\0', '\\', ':'])
        && !rel.starts_with('/')
        && !rel.split('/').any(|seg| seg == ".." || seg.is_empty() || seg == ".")
}

pub fn safe_join(base: &FsPath, rel: &str) -> Option<PathBuf> {
    rel_ok(rel).then(|| base.join(rel))
}

pub fn unix(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Streaming SHA-256 of a file as lowercase hex.
pub fn hash_file(path: &FsPath) -> std::io::Result<String> {
    let mut f = std::fs::File::open(path)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// Total size of all files below `dir`.
pub fn dir_size(dir: &FsPath) -> u64 {
    let Ok(rd) = std::fs::read_dir(dir) else { return 0 };
    rd.flatten()
        .map(|e| match e.metadata() {
            Ok(m) if m.is_dir() => dir_size(&e.path()),
            Ok(m) => m.len(),
            Err(_) => 0,
        })
        .sum()
}

fn walk(dir: &FsPath, rel: &str, f: &mut dyn FnMut(&FsPath, &str, &std::fs::Metadata)) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let Ok(md) = e.metadata() else { continue };
        let rel_path = if rel.is_empty() { name.clone() } else { format!("{rel}/{name}") };
        if md.is_dir() {
            if name != "node_modules" {
                walk(&e.path(), &rel_path, f);
            }
        } else {
            f(&e.path(), &rel_path, &md);
        }
    }
}

/// Video files below `dir` written between `since` and `until`, other than the ones listed in `exclude`
/// (files that came from a sync are sources, not results).
pub fn scan_outputs(dir: &FsPath, exclude: &HashSet<String>, since: SystemTime, until: Option<SystemTime>) -> Vec<OutputFile> {
    let mut found = Vec::new();
    walk(dir, "", &mut |_, rel, md| {
        let is_video = rel.rsplit('.').next().map(|x| OUTPUT_EXTS.contains(&x.to_ascii_lowercase().as_str())).unwrap_or(false);
        if !is_video || exclude.contains(rel) {
            return;
        }
        let Ok(m) = md.modified() else { return };
        if m < since || until.is_some_and(|u| m > u) {
            return;
        }
        found.push(OutputFile {
            path: rel.to_string(),
            name: rel.rsplit('/').next().unwrap_or(rel).to_string(),
            size: md.len(),
            created_unix: unix(m),
            sha256: None,
        });
    });
    found.sort_by(|a, b| (a.created_unix, &a.path).cmp(&(b.created_unix, &b.path)));
    found
}

/// Every non-`node_modules` file below `dir`, relative, hashed. Used to build the manifest of a migrated project.
pub fn hash_tree(dir: &FsPath, exclude: &HashSet<String>) -> Vec<hfr_proto::FileEntry> {
    let mut list = Vec::new();
    walk(dir, "", &mut |p, rel, md| {
        if !exclude.contains(rel) {
            list.push((p.to_path_buf(), rel.to_string(), md.len()));
        }
    });
    list.into_iter()
        .filter_map(|(p, rel, size)| hash_file(&p).ok().map(|sha256| hfr_proto::FileEntry { path: rel, size, sha256 }))
        .collect()
}

// ---- Range ------------------------------------------------------------------------------------------------

#[derive(Debug, PartialEq, Eq)]
pub enum RangeSpec {
    /// No (usable) Range header: send the whole file.
    Full,
    /// Inclusive byte range.
    Part(u64, u64),
    /// The range starts beyond the end of the file.
    Unsatisfiable,
}

/// Parses a single `bytes=start-end` / `bytes=start-` / `bytes=-suffix` range for a file of `len` bytes.
/// Multi-range requests are answered with the whole file, which the spec allows.
pub fn parse_range(header: Option<&str>, len: u64) -> RangeSpec {
    let Some(h) = header.and_then(|h| h.strip_prefix("bytes=")) else { return RangeSpec::Full };
    if h.contains(',') {
        return RangeSpec::Full;
    }
    let Some((a, b)) = h.split_once('-') else { return RangeSpec::Full };
    let (start, end) = match (a.trim(), b.trim()) {
        ("", suffix) => match suffix.parse::<u64>() {
            Ok(n) if n > 0 => (len.saturating_sub(n), len.saturating_sub(1)),
            _ => return RangeSpec::Full,
        },
        (s, "") => match s.parse::<u64>() {
            Ok(s) => (s, len.saturating_sub(1)),
            Err(_) => return RangeSpec::Full,
        },
        (s, e) => match (s.parse::<u64>(), e.parse::<u64>()) {
            (Ok(s), Ok(e)) if s <= e => (s, e.min(len.saturating_sub(1))),
            _ => return RangeSpec::Full,
        },
    };
    if len == 0 || start >= len {
        RangeSpec::Unsatisfiable
    } else {
        RangeSpec::Part(start, end)
    }
}

// ---- routes -----------------------------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct PathQuery {
    path: String,
}

pub async fn list(State(app): State<App>, Path((p, j)): Path<(String, String)>) -> Res<Json<Vec<OutputFile>>> {
    Ok(Json(crate::get_job(&app, &p, &j).await?.files))
}

async fn locate(app: &App, p: &str, j: &str, rel: &str) -> Res<(OutputFile, PathBuf)> {
    let job = crate::get_job(app, p, j).await?;
    let file = job.files.into_iter().find(|f| f.path == rel).ok_or_else(not_found)?;
    let full = safe_join(&app.files_dir(p), rel).ok_or_else(not_found)?;
    Ok((file, full))
}

/// Download a render output. Supports `Range` so an interrupted download can continue where it stopped.
pub async fn download(
    State(app): State<App>,
    Path((p, j)): Path<(String, String)>,
    Query(q): Query<PathQuery>,
    headers: HeaderMap,
) -> Res<Response> {
    let (file, full) = locate(&app, &p, &j, &q.path).await?;
    let mut f = tokio::fs::File::open(&full).await.map_err(|_| not_found())?;
    let len = f.metadata().await.map_err(ise)?.len();
    let range = parse_range(headers.get(header::RANGE).and_then(|v| v.to_str().ok()), len);
    let mut resp = Response::builder().header(header::ACCEPT_RANGES, "bytes").header(header::CONTENT_TYPE, "application/octet-stream");
    if let Some(sha) = &file.sha256 {
        resp = resp.header("x-hfr-sha256", sha.as_str());
    }
    let body = match range {
        RangeSpec::Full => {
            resp = resp.status(StatusCode::OK).header(header::CONTENT_LENGTH, len);
            Body::from_stream(ReaderStream::with_capacity(f, 256 * 1024))
        }
        RangeSpec::Part(start, end) => {
            f.seek(std::io::SeekFrom::Start(start)).await.map_err(ise)?;
            let n = end - start + 1;
            resp = resp
                .status(StatusCode::PARTIAL_CONTENT)
                .header(header::CONTENT_RANGE, format!("bytes {start}-{end}/{len}"))
                .header(header::CONTENT_LENGTH, n);
            Body::from_stream(ReaderStream::with_capacity(f.take(n), 256 * 1024))
        }
        RangeSpec::Unsatisfiable => {
            let mut r = (StatusCode::RANGE_NOT_SATISFIABLE, "range not satisfiable").into_response();
            r.headers_mut().insert(header::CONTENT_RANGE, HeaderValue::from_str(&format!("bytes */{len}")).map_err(ise)?);
            return Ok(r);
        }
    };
    resp.body(body).map_err(ise)
}

/// Delete one render output from the VPS.
pub async fn delete(State(app): State<App>, Path((p, j)): Path<(String, String)>, Query(q): Query<PathQuery>) -> Res<StatusCode> {
    let (_, full) = locate(&app, &p, &j, &q.path).await?;
    match tokio::fs::remove_file(&full).await {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(ise(e)),
    }
    if let Some(job) = app.inner.lock().await.jobs.get_mut(&j) {
        job.files.retain(|f| f.path != q.path);
    }
    crate::store::persist_job(&app, &j).await;
    crate::refresh_project_size(&app, &p).await;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_paths_are_checked() {
        for ok in ["a.mp4", "renders/a.mp4", "a b/c.mp4"] {
            assert!(rel_ok(ok), "{ok}");
        }
        for bad in ["", "/etc/passwd", "../x", "a/../b", "a//b", "./a", "C:\\x", "a\\b", "x:y", "a\0b"] {
            assert!(!rel_ok(bad), "{bad:?}");
        }
    }

    #[test]
    fn ranges() {
        assert_eq!(parse_range(None, 100), RangeSpec::Full);
        assert_eq!(parse_range(Some("bytes=0-"), 100), RangeSpec::Part(0, 99));
        assert_eq!(parse_range(Some("bytes=40-"), 100), RangeSpec::Part(40, 99));
        assert_eq!(parse_range(Some("bytes=10-19"), 100), RangeSpec::Part(10, 19));
        assert_eq!(parse_range(Some("bytes=90-500"), 100), RangeSpec::Part(90, 99), "end is clamped");
        assert_eq!(parse_range(Some("bytes=-10"), 100), RangeSpec::Part(90, 99));
        assert_eq!(parse_range(Some("bytes=100-"), 100), RangeSpec::Unsatisfiable);
        assert_eq!(parse_range(Some("bytes=0-1,5-6"), 100), RangeSpec::Full, "multi-range falls back to the whole file");
        assert_eq!(parse_range(Some("bytes=20-10"), 100), RangeSpec::Full);
        assert_eq!(parse_range(Some("items=0-5"), 100), RangeSpec::Full);
        assert_eq!(parse_range(Some("bytes=0-"), 0), RangeSpec::Unsatisfiable);
    }

    #[test]
    fn outputs_are_found_by_time_and_extension() {
        let dir = std::env::temp_dir().join(format!("hfr-files-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("renders")).unwrap();
        std::fs::create_dir_all(dir.join("node_modules/x")).unwrap();
        let before = SystemTime::now() - std::time::Duration::from_secs(5);
        std::fs::write(dir.join("renders/a.mp4"), "12345").unwrap();
        std::fs::write(dir.join("notes.txt"), "x").unwrap();
        std::fs::write(dir.join("node_modules/x/v.mp4"), "x").unwrap();
        std::fs::write(dir.join("source.mp4"), "x").unwrap();
        let exclude: HashSet<String> = ["source.mp4".to_string()].into();
        let found = scan_outputs(&dir, &exclude, before, None);
        assert_eq!(found.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(), ["renders/a.mp4"]);
        assert_eq!(found[0].size, 5);
        // Nothing written after "now + 1h" is found.
        assert!(scan_outputs(&dir, &exclude, SystemTime::now() + std::time::Duration::from_secs(3600), None).is_empty());
        assert_eq!(hash_file(&dir.join("renders/a.mp4")).unwrap().len(), 64);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
