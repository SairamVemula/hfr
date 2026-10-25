//! The allowlist of commands the agent will run. Shared so clients can check their own output against it.

const VALUE_FLAGS: &[&str] = &[
    "-c",
    "--composition",
    "-o",
    "--output",
    "-f",
    "--fps",
    "-q",
    "--quality",
    "--format",
    "-w",
    "--workers",
    "--crf",
    "--video-bitrate",
    "--vp9-cpu-used",
    "--variables",
    "--variables-file",
    "--video-frame-format",
    "--gif-loop",
    "--hls-segment-seconds",
];
const BOOL_FLAGS: &[&str] = &[
    "--hdr",
    "--sdr",
    "--quiet",
    "--strict",
    "--strict-all",
    "--gpu",
    "--browser-gpu",
    "--no-browser-gpu",
    "--best-effort",
    "--no-best-effort",
    "--strict-variables",
    "--lint-verbose",
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
}
