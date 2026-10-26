//! Input checks for everything a client can send the agent: job commands, uploaded archives, tokens.

use std::path::{Component, Path};

use sha2::{Digest, Sha256};

pub use hfr_proto::validate_command;

/// Extracts a tar into `dest`, refusing links, devices and any path that is absolute or climbs out.
/// Returns the project-relative paths (with `/`) of the regular files it wrote.
pub fn extract_safe(tar_path: &Path, dest: &Path) -> Result<Vec<String>, String> {
    let f = std::fs::File::open(tar_path).map_err(|e| e.to_string())?;
    let mut ar = tar::Archive::new(std::io::BufReader::new(f));
    ar.set_preserve_permissions(false);
    ar.set_unpack_xattrs(false);
    let mut written = Vec::new();
    for entry in ar.entries().map_err(|e| e.to_string())? {
        let mut e = entry.map_err(|e| e.to_string())?;
        let t = e.header().entry_type();
        if !matches!(t, tar::EntryType::Regular | tar::EntryType::Directory | tar::EntryType::Continuous) {
            return Err(format!("archive contains a disallowed entry type ({t:?})"));
        }
        let path = e.path().map_err(|e| e.to_string())?.into_owned();
        if !path.components().all(|c| matches!(c, Component::Normal(_) | Component::CurDir)) {
            return Err(format!("unsafe path in archive: {}", path.display()));
        }
        if !e.unpack_in(dest).map_err(|e| e.to_string())? {
            return Err(format!("unsafe path in archive: {}", path.display()));
        }
        if t != tar::EntryType::Directory {
            let rel: Vec<String> = path
                .components()
                .filter_map(|c| match c {
                    Component::Normal(s) => Some(s.to_string_lossy().into_owned()),
                    _ => None,
                })
                .collect();
            written.push(rel.join("/"));
        }
    }
    Ok(written)
}

/// Constant-time token check: compares fixed-length digests so neither length nor content leaks via timing.
pub fn token_matches(header: &str, token: &str) -> bool {
    let expected = Sha256::digest(format!("Bearer {token}").as_bytes());
    let got = Sha256::digest(header.as_bytes());
    expected.iter().zip(got.iter()).fold(0u8, |acc, (a, b)| acc | (a ^ b)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_check() {
        assert!(token_matches("Bearer abc", "abc"));
        assert!(!token_matches("Bearer abd", "abc"));
        assert!(!token_matches("", "abc"));
    }

    fn write_tar(path: &Path, f: impl FnOnce(&mut tar::Builder<std::fs::File>)) {
        let mut b = tar::Builder::new(std::fs::File::create(path).unwrap());
        f(&mut b);
        b.finish().unwrap();
    }

    #[test]
    fn extract_ok_and_blocks_symlink() {
        let base = std::env::temp_dir().join(format!("hfr-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(base.join("out")).unwrap();
        let good = base.join("good.tar");
        write_tar(&good, |b| {
            let mut h = tar::Header::new_gnu();
            h.set_size(2);
            h.set_mode(0o644);
            b.append_data(&mut h, "dir/a.txt", &b"hi"[..]).unwrap();
        });
        assert_eq!(extract_safe(&good, &base.join("out")).unwrap(), ["dir/a.txt"]);
        assert!(base.join("out/dir/a.txt").is_file());

        let bad = base.join("bad.tar");
        write_tar(&bad, |b| {
            let mut h = tar::Header::new_gnu();
            h.set_entry_type(tar::EntryType::Symlink);
            h.set_size(0);
            h.set_mode(0o777);
            b.append_link(&mut h, "link", "/etc/passwd").unwrap();
        });
        assert!(extract_safe(&bad, &base.join("out")).is_err());
        let _ = std::fs::remove_dir_all(&base);
    }
}
