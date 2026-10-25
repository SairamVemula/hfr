//! Input checks for everything a client can send the agent: job commands, uploaded archives, tokens.

use std::path::{Component, Path};

use sha2::{Digest, Sha256};

const VALUE_FLAGS: &[&str] = &[
    "-c", "--composition", "-o", "--output", "-f", "--fps", "-q", "--quality", "--format", "-w", "--workers", "--crf",
    "--video-bitrate", "--vp9-cpu-used", "--variables", "--variables-file", "--video-frame-format", "--gif-loop",
    "--hls-segment-seconds",
];
const BOOL_FLAGS: &[&str] = &[
    "--hdr", "--sdr", "--quiet", "--strict", "--strict-all", "--gpu", "--browser-gpu", "--no-browser-gpu",
    "--best-effort", "--no-best-effort", "--strict-variables", "--lint-verbose",
];
/// Flags whose value is a path inside the project; it must stay relative and inside the project.
const PATH_FLAGS: &[&str] = &["-c", "--composition", "-o", "--output", "--variables-file"];

fn safe_relative(p: &str) -> bool {
    if p.is_empty() || p.contains('\0') || p.contains(':') || p.starts_with('/') || p.starts_with('\\') {
        return false;
    }
    !p.split(['/', '\\']).any(|seg| seg == "..")
}

/// Only `npx hyperframes render [known flags] [.]` is allowed; no shell is involved, so values cannot inject commands.
pub fn validate_command(cmd: &[String]) -> Result<(), String> {
    if cmd.len() < 3 || cmd[0] != "npx" || cmd[1] != "hyperframes" || cmd[2] != "render" {
        return Err("only `npx hyperframes render [options]` is allowed".into());
    }
    let mut i = 3;
    while i < cmd.len() {
        let t = &cmd[i];
        i += 1;
        if !t.starts_with('-') {
            if t == "." {
                continue;
            }
            return Err(format!("unexpected argument `{t}`"));
        }
        let (name, inline) = match t.split_once('=') {
            Some((n, v)) => (n, Some(v.to_string())),
            None => (t.as_str(), None),
        };
        if BOOL_FLAGS.contains(&name) {
            if inline.is_some() {
                return Err(format!("flag `{name}` takes no value"));
            }
        } else if VALUE_FLAGS.contains(&name) {
            let value = match inline {
                Some(v) => v,
                None => {
                    let v = cmd.get(i).ok_or_else(|| format!("flag `{name}` needs a value"))?.clone();
                    i += 1;
                    v
                }
            };
            if value.contains('\0') || (PATH_FLAGS.contains(&name) && !safe_relative(&value)) {
                return Err(format!("unsafe value for `{name}`"));
            }
        } else {
            return Err(format!("flag `{name}` is not allowed"));
        }
    }
    Ok(())
}

/// Extracts a tar into `dest`, refusing links, devices and any path that is absolute or climbs out.
pub fn extract_safe(tar_path: &Path, dest: &Path) -> Result<(), String> {
    let f = std::fs::File::open(tar_path).map_err(|e| e.to_string())?;
    let mut ar = tar::Archive::new(std::io::BufReader::new(f));
    ar.set_preserve_permissions(false);
    ar.set_unpack_xattrs(false);
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
    }
    Ok(())
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

    fn v(s: &[&str]) -> Vec<String> {
        s.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn allows_plain_and_flagged_render() {
        assert!(validate_command(&v(&["npx", "hyperframes", "render"])).is_ok());
        assert!(validate_command(&v(&["npx", "hyperframes", "render", "-q", "delivery", "--fps=60", "--quiet", "."])).is_ok());
        assert!(validate_command(&v(&["npx", "hyperframes", "render", "-o", "renders/out.mp4"])).is_ok());
    }

    #[test]
    fn rejects_other_commands_and_unknown_flags() {
        assert!(validate_command(&v(&["bash", "-c", "id"])).is_err());
        assert!(validate_command(&v(&["npx", "hyperframes", "init"])).is_err());
        assert!(validate_command(&v(&["npx", "hyperframes", "render", "--docker"])).is_err());
        assert!(validate_command(&v(&["npx", "hyperframes", "render", "/etc"])).is_err());
        assert!(validate_command(&v(&["npx", "hyperframes", "render", "--quiet=1"])).is_err());
        assert!(validate_command(&v(&["npx", "hyperframes", "render", "-q"])).is_err());
    }

    #[test]
    fn rejects_escaping_output_paths() {
        assert!(validate_command(&v(&["npx", "hyperframes", "render", "-o", "/etc/cron.d/x"])).is_err());
        assert!(validate_command(&v(&["npx", "hyperframes", "render", "--output=../x.mp4"])).is_err());
        assert!(validate_command(&v(&["npx", "hyperframes", "render", "-o", "C:\\x.mp4"])).is_err());
    }

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
        extract_safe(&good, &base.join("out")).unwrap();
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
