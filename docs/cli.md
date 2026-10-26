# CLI reference: `hfr`

All commands except `init` and `setup` open an SSH tunnel to your VPS using the saved connection (see [configuration.md](configuration.md)), run, and close it again.

```
hfr <command> [options]
```

## Commands

### `hfr update [--check] [--json]`
Looks for a newer release of hfr on GitHub. `--check` only reports (`--json` prints `{current, latest, available, notes}`). Without `--check` it downloads the installer, verifies the signed checksums and the installer's SHA-256 (see [security.md](security.md#updates)), then installs it (Windows: silently, the program ends and starts again; macOS/Linux: opens the verified file). `hfr status` also prints one line when the last check found a newer version. The VPS agent is updated separately with `hfr setup`.

### `hfr setup`
Installs everything on the VPS, and upgrades it later. **Safe to re-run:** each step first checks what is already there and only installs what is missing or outdated (system packages, Node 22, hyperframes, Chrome, the agent). It never interrupts a running render: while a render is running, or while render folders of an older agent version that cannot be migrated are still on the VPS, the hyperframes and agent upgrades are postponed with a note, and the next run picks them up. See [getting-started.md](getting-started.md#4-install-the-agent-on-the-vps).

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

### Projects and renders: how the pieces fit

A **project** is a folder of files on the VPS. Each **render** (job) runs inside a project with its own options and log and lists the **files** it produced. Rendering again does not upload the project again: only files that changed are sent. A folder is remembered by its path (see [configuration.md](configuration.md#remembered-folders)), so `hfr render ./intro` twice goes to the same project; two folders called `intro` in different places are two projects. Anywhere an id is expected, an unambiguous prefix (`03d91877`) or, for projects, the name works.

### `hfr render [dir] [options]`
Sync `dir` (default `.`) into its project (only changes are uploaded), start a render and follow it until it ends. Exits non-zero if the render failed.

While uploading it prints one progress line with speed and time left (`Uploading  42%  251.3 MB of 600.0 MB  18.4 MB/s  ETA 1m 05s`). **Ctrl+C during the upload cancels it**; files that already arrived stay on the VPS and are not sent again next time.

| Option | Default | Meaning |
| --- | --- | --- |
| `--fps <1-240>` | composition's own (30 if unset) | Frame rate |
| `--quality <draft\|looks\|delivery>` | `looks` (CRF 16) | Quality preset |
| `--format <mp4\|webm\|mov\|gif>` | `mp4` | Output format (`webm` and `mov` keep transparency) |
| `--workers <n>` | auto | Parallel Chrome workers; each needs about 256 MB RAM |
| `--hdr` / `--sdr` | auto-detect | Force HDR or SDR |
| `--resolution <preset>` | composition's own | Output size: `landscape` (1920x1080), `landscape-4k`, `portrait` (1080x1920), `portrait-4k`, `square` (1080x1080), `square-4k`. The shape must match the composition; not available with `--hdr` |
| `--composition <file.html>` | `index.html` | Render another HTML file of the project |
| `--project <id\|name>` | the project remembered for `dir` | Render in this project |
| `--new` | off | Always create a new project for this folder |
| `--no-sync` | off | Do not sync: render with the files already on the VPS |
| `--like <job>` | | Start from the options of an earlier render; options given here override them |
| `--detach` | off | Return right after starting; follow later with `hfr logs -f` |
| `-- <command...>` | | Advanced: replace all of the above with a full command, e.g. `-- npx hyperframes render -q delivery --crf 14` |

These are exactly the choices of the desktop app's **Render options** dialog. The agent only accepts `npx hyperframes render` with a fixed allowlist of options (defined in `crates/proto/src/command.rs`): `-c/--composition`, `-o/--output`, `-f/--fps`, `-q/--quality`, `--format`, `-w/--workers`, `--crf`, `--video-bitrate`, `--vp9-cpu-used`, `--variables`, `--variables-file`, `--video-frame-format`, `--gif-loop`, `--hls-segment-seconds`, `--hdr`, `--sdr`, `--quiet`, `--strict`, `--strict-all`, `--gpu`, `--browser-gpu`, `--no-browser-gpu`, `--best-effort`, `--no-best-effort`, `--strict-variables`, `--lint-verbose`. Path values (`-o`, `-c`, `--variables-file`) must be relative and inside the project. PNG-sequence and HLS output (directories) are not supported yet.

```bash
hfr render ./intro --quality delivery --fps 60      # first time: creates the project, uploads everything
hfr render ./intro --quality draft --detach         # again: uploads nothing (or only what you edited)
hfr render ./intro --like 03d91877 --format webm    # same options as that render, but WebM
```

### `hfr push [dir] [options]`
Create the project for `dir`, or sync only the changes into its existing project, without rendering. Options: `--project <id or name>`, `--new`. Prints how many files were uploaded and deleted.

### `hfr projects [--json]`
List projects: id, name, size, number of renders and files, local folder. `--json` prints `[{ "project": ..., "jobs": [...] }]` (fields in [agent-api.md](agent-api.md#types)).

### `hfr jobs [project] [--json]`
List renders of all projects, or of one (id prefix or name): id, state, project, options, number of files, age.

### `hfr files <job> [--json]`
List the output files of a render: path, size, checksum.

### `hfr logs <job> [-f]`
Print the render's log. With `-f` and a running render, follow it live.

### `hfr cancel <job>`
Stop a running render. The agent stops the whole process tree (npx, node, Chrome workers, ffmpeg), the render becomes **cancelled**, and the project stays on the VPS so you can render it again without uploading. Cancelling a render that is not running returns an error.

### `hfr pull <job> [file] [-o <dir>] [--delete]`
Download the files of a render (all of them, or one by name or path) into `-o` (default: the current folder). Shows `Downloading  62%  1.2 GB of 2.0 GB  38.0 MB/s  ETA 21s`. Each file is written as `*.hfrpart` with a small sidecar; if the connection drops or you press Ctrl+C, run the same command again and it **resumes** where it stopped. The finished file's SHA-256 is compared with the agent's before it is renamed into place. An existing file is never overwritten: the new one becomes `name (1).mp4`. With `--delete` each file is removed from the VPS only after it was verified.

### `hfr rm project|job|file`
Delete from the VPS, after asking (use `--yes` to skip the question):

| Command | Removes |
| --- | --- |
| `hfr rm project <project>` | The project with all its files, renders and outputs. Refused while a render is running. Your local folder is untouched |
| `hfr rm job <job>` | A render, its log and its output files. The project's own files stay |
| `hfr rm file <job> <file>` | One output file of a render |

## Ignoring files when uploading

Put a `.hfrignore` file in the project root, in gitignore syntax:

```
renders/
*.psd
old-footage/
```

`node_modules` and `.git` are always skipped. The VPS installs Hyperframes globally, so a local `node_modules` is not needed.

## Exit codes and output

`0` success; non-zero with an `Error: ...` line on stderr otherwise. Only the `--json` outputs (`projects`, `jobs`, `files`, `status`) are meant for parsing; other output is for humans and may change.

## Typical session

```bash
hfr status                       # is the VPS idle?
hfr render ./bodycam-project     # follow it, or --detach and come back
hfr jobs intro
hfr pull 03d91877 --delete       # files in the current folder, verified, VPS space freed
```
