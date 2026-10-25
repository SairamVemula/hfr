# CLI reference: `hfr`

All commands except `init` and `setup` open an SSH tunnel to your VPS using the saved connection (see [configuration.md](configuration.md)), run, and close it again.

```
hfr <command> [options]
```

## Commands

### `hfr setup`
Installs everything on the VPS, and upgrades it later. **Safe to re-run:** each step first checks what is already there and only installs what is missing or outdated (system packages, Node 22, hyperframes, Chrome, the agent). It never interrupts a running render: while a render is running, or while jobs from an older agent version are still on the VPS, the hyperframes and agent upgrades are postponed with a note, and the next run picks them up. See [getting-started.md](getting-started.md#4-install-the-agent-on-the-vps).

| Option | Default | Meaning |
| --- | --- | --- |
| `--host <host>` | required | VPS hostname or IP |
| `--user <user>` | `root` | SSH user (root or passwordless sudo) |
| `--key <path>` | ssh-agent | Private key file to pass as `ssh -i` |
| `--ssh-port <n>` | `22` | SSH port |
| `--agent <path>` | auto | Linux `hfr-agent` binary to upload. Auto-detected for the VPS CPU from the installed app's bundled copy or `target/<triple>/release/` |
| `--force` | off | Reinstall everything even if it is already installed and up to date |

### `hfr init`
Saves connection settings without installing anything (for a VPS you set up by hand, or for local testing).

| Option | Meaning |
| --- | --- |
| `--host`, `--user`, `--key`, `--ssh-port` | as above |
| `--token <token>` | required; the agent's `HFR_TOKEN` |
| `--direct-url <url>` | skip the SSH tunnel and call the agent at this URL (local testing only) |

### `hfr render [dir] [options]`
Pack `dir` (default `.`), upload it, start the render and follow it until it ends. Exits non-zero if the render failed.

While uploading it prints one progress line with speed and time left (`Uploading  42%  251.3 MB of 600.0 MB  18.4 MB/s  ETA 1m 05s`). **Ctrl+C during the upload cancels it**; the half-created job is deleted from the VPS, so nothing is left behind.

| Option | Default | Meaning |
| --- | --- | --- |
| `--fps <1-240>` | composition's own (30 if unset) | Frame rate |
| `--quality <draft\|looks\|delivery>` | `looks` (CRF 16) | Quality preset |
| `--format <mp4\|webm\|mov\|gif>` | `mp4` | Output format (`webm` and `mov` keep transparency) |
| `--workers <n>` | auto | Parallel Chrome workers; each needs about 256 MB RAM |
| `--hdr` / `--sdr` | auto-detect | Force HDR or SDR |
| `--composition <file.html>` | `index.html` | Render another HTML file of the project |
| `--detach` | off | Return right after starting; follow later with `hfr logs -f` |
| `-- <command...>` | | Advanced: replace all of the above with a full command, e.g. `-- npx hyperframes render -q delivery --crf 14` |

These are exactly the choices of the desktop app's **Render options** dialog. The agent only accepts `npx hyperframes render` with a fixed allowlist of options (defined in `crates/proto/src/command.rs`): `-c/--composition`, `-o/--output`, `-f/--fps`, `-q/--quality`, `--format`, `-w/--workers`, `--crf`, `--video-bitrate`, `--vp9-cpu-used`, `--variables`, `--variables-file`, `--video-frame-format`, `--gif-loop`, `--hls-segment-seconds`, `--hdr`, `--sdr`, `--quiet`, `--strict`, `--strict-all`, `--gpu`, `--browser-gpu`, `--no-browser-gpu`, `--best-effort`, `--no-best-effort`, `--strict-variables`, `--lint-verbose`. Path values (`-o`, `-c`, `--variables-file`) must be relative and inside the project. PNG-sequence and HLS output (directories) are not supported yet.

### `hfr push [dir] [options]`
Pack and upload only, with the same options and progress line; prints the job id. Start it later with `hfr start`.

### `hfr start <job-id>`
Start an uploaded job.

### `hfr cancel <job-id>`
Stop a running render. The agent stops the whole process tree (npx, node, Chrome workers, ffmpeg), the job becomes **cancelled**, and the project stays on the VPS so you can render it again without uploading. Cancelling a job that is not running returns an error.

### `hfr rerun <job-id> [options]`
Run an existing job again with different options, **without uploading the project again**. A running render is cancelled first. Accepts the same options as `hfr render` (`--fps`, `--quality`, `--format`, `--workers`, `--hdr`/`--sdr`, `--composition`, `--detach`, `-- <command...>`); options you leave out keep the value the job already has, including hand-written extras such as `--crf 14`. It follows the new run's logs unless `--detach` is given.

```bash
hfr rerun 14511aaa-... --quality delivery            # same job, better quality
hfr rerun 14511aaa-... --fps 60 --workers 2 --detach
```

### `hfr jobs [--json]`
List jobs: id, state, size, name. `--json` prints the raw job objects (fields in [agent-api.md](agent-api.md#job)).

### `hfr logs <job-id> [-f]`
Print the job's log. With `-f` and a running job, follow it live.

### `hfr status [--json]`
VPS snapshot: CPU (cores, load), memory, swap, disk. `--json` prints the metrics object.

### `hfr pull <job-id> [-o <dir>] [--delete]`
Download the render output (default directory: current). The file is written as `*.part`, its size and SHA-256 are checked against the agent's, and only then renamed. With `--delete` the job is removed from the VPS after a verified download.

### `hfr rm <job-id>`
Delete a job and all its files from the VPS (refused while it is running).

## Ignoring files when uploading

Put a `.hfrignore` file in the project root, in gitignore syntax:

```
renders/
*.psd
old-footage/
```

`node_modules` and `.git` are always skipped. The VPS installs Hyperframes globally, so a local `node_modules` is not needed.

## Exit codes and output

`0` success; non-zero with an `Error: ...` line on stderr otherwise. Only `jobs --json` and `status --json` are meant for parsing; other output is for humans and may change.

## Typical session

```bash
hfr status                       # is the VPS idle?
hfr render ./bodycam-project     # follow it, or --detach and come back
hfr jobs
hfr pull 14511aaa-... --delete   # output file in the current folder, VPS space freed
```
