# CLI reference: `hfr`

All commands except `init` and `setup` open an SSH tunnel to your VPS using the saved connection (see [configuration.md](configuration.md)), run, and close it again.

```
hfr <command> [options]
```

## Commands

### `hfr setup`
One-time (and upgrade) installation on the VPS. See [getting-started.md](getting-started.md#4-install-the-agent-on-the-vps).

| Option | Default | Meaning |
| --- | --- | --- |
| `--host <host>` | required | VPS hostname or IP |
| `--user <user>` | `root` | SSH user (root or passwordless sudo) |
| `--key <path>` | ssh-agent | Private key file to pass as `ssh -i` |
| `--ssh-port <n>` | `22` | SSH port |
| `--agent <path>` | auto | Linux `hfr-agent` binary to upload. Auto-detected for the VPS CPU from the installed app's bundled copy or `target/<triple>/release/` |

### `hfr init`
Saves connection settings without installing anything (for a VPS you set up by hand, or for local testing).

| Option | Meaning |
| --- | --- |
| `--host`, `--user`, `--key`, `--ssh-port` | as above |
| `--token <token>` | required; the agent's `HFR_TOKEN` |
| `--direct-url <url>` | skip the SSH tunnel and call the agent at this URL (local testing only) |

### `hfr render [dir] [-- options]`
Pack `dir` (default `.`), upload, start the render and follow it until it ends. Exits non-zero if the render failed.

| Option | Meaning |
| --- | --- |
| `--detach` | return right after starting; follow later with `hfr logs -f` |
| `-- <args>` | extra `npx hyperframes render` options, e.g. `-- npx hyperframes render -q delivery --fps 60` |

Only `npx hyperframes render` with a fixed allowlist of options is accepted by the agent: `-c/--composition`, `-o/--output`, `-f/--fps`, `-q/--quality`, `--format`, `-w/--workers`, `--crf`, `--video-bitrate`, `--vp9-cpu-used`, `--variables`, `--variables-file`, `--video-frame-format`, `--gif-loop`, `--hls-segment-seconds`, `--hdr`, `--sdr`, `--quiet`, `--strict`, `--strict-all`, `--gpu`, `--browser-gpu`, `--no-browser-gpu`, `--best-effort`, `--no-best-effort`, `--strict-variables`, `--lint-verbose`. Path values (`-o`, `-c`, `--variables-file`) must be relative and inside the project.

### `hfr push [dir] [-- options]`
Pack and upload only; prints the job id. Start it later with `hfr start`.

### `hfr start <job-id>`
Start an uploaded job.

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
