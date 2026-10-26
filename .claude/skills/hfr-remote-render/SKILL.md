---
name: hfr-remote-render
description: Operate HFR (the `hfr` CLI / `hfr-gui` app) to render a Hyperframes video project on the user's own VPS instead of their PC. Use when the user wants to render a Hyperframes or "hyperframes" project remotely, offload a heavy render, push or upload a project to their server, follow a remote render, render again with other options, check the VPS's CPU/RAM/disk, download the finished video, free space on the VPS, set up or repair the VPS connection, or mentions `hfr`, "HFR" or "hfr-agent".
---

# Rendering Hyperframes projects on the user's VPS with HFR

HFR moves heavy Hyperframes renders off the user's computer. `hfr` syncs the project to their Linux VPS through an SSH tunnel to `hfr-agent`, the VPS renders it, and `hfr pull` brings back the SHA-256-verified video and can delete it from the VPS to free disk.

Full docs: `docs/cli.md`, `docs/troubleshooting.md`, `docs/security.md` in the repository.

## The model (read this once)

- **Project**: a folder of files on the VPS, created from a local folder. A folder is remembered by its path; rendering it again goes to the same project. Two folders with the same name are two different projects.
- **Render** (a "job"): one run with its own options and log, inside a project. A project has many renders; **only one runs at a time per project**.
- **File**: a video a render wrote. Each file can be downloaded or deleted on its own.
- Re-rendering never re-uploads the project: only files that changed since the last sync are sent (an unchanged folder uploads 0 bytes).
- Ids can be shortened to a unique prefix (`03d91877`); projects can also be addressed by name.

## Before you do anything

1. **Is it set up?** Run `hfr status --json`.
   - JSON with `cpu_total`, `mem_total`, `disk_total` -> connected; proceed.
   - `no config ... run hfr init first` -> not set up; go to *Setup*.
   - `ssh tunnel exited early` / `could not connect` -> go to *Connection problems*.
   - `the agent on the VPS is too old ... API 1` -> the VPS agent needs `hfr setup` (ask the user; it waits while a render runs).
2. **Is the VPS busy?** Look at `cpu_total` and `hfr jobs --json` (renders with `"state": "running"`). A second heavy render will slow both.
3. **Is there room?** `disk_total - disk_used` should comfortably exceed the changed files plus the output (video can be several GB).

## Rendering

```bash
hfr render <project-dir>              # syncs changes, starts a render, follows logs until done; exit code != 0 if it failed
hfr render <project-dir> --detach     # returns right after starting; prints the render id
hfr render <dir> --fps 60 --quality delivery --workers 2
hfr render <dir> --like <job> --format webm     # same options as an earlier render, but WebM
hfr render <dir> --no-sync                      # use the files already on the VPS
hfr push <dir>                                  # upload/sync only, no render
```

Options, the same ones the app's *Render options* dialog offers: `--fps <1-240>` [composition's own], `--quality draft|looks|delivery` [looks], `--format mp4|webm|mov|gif` [mp4; webm and mov keep transparency], `--workers <n>` [auto; each Chrome worker needs ~256 MB RAM, so check `hfr status` first], `--resolution landscape|landscape-4k|portrait|portrait-4k|square|square-4k` [composition's own; shape must match the composition; not with `--hdr`], `--hdr` / `--sdr` [auto-detect], `--composition <file.html>` [index.html]. Leave an option out to keep Hyperframes' default; only pass what the user asked for. `-- <full command>` replaces them all (advanced). `--project <id or name>` picks a project explicitly; `--new` forces a new one.

While uploading, a progress line shows percent, bytes, speed and ETA. **Ctrl+C cancels the upload**; files already sent stay on the VPS and are not sent again. If you run `hfr` in the background and must stop it, terminate the process; do not leave uploads running unattended.

- `<project-dir>` is the folder containing `index.html` / `hyperframes.json`.
- Sync skips `node_modules`, `.git` and anything in `.hfrignore` (gitignore syntax). If the project has old `renders/` or large unused footage, suggest adding them to `.hfrignore` before uploading; ask before editing user files. Files deleted locally are deleted from the project on the next sync.
- Only `npx hyperframes render` with these options is accepted: `-c/--composition -o/--output -f/--fps -q/--quality --resolution --format -w/--workers --crf --video-bitrate --vp9-cpu-used --variables --variables-file --video-frame-format --gif-loop --hls-segment-seconds --hdr --sdr --quiet --strict --strict-all --gpu --browser-gpu --no-browser-gpu --best-effort --no-best-effort --strict-variables --lint-verbose`. Output/composition/variables-file paths must be relative to the project. Anything else is rejected with `400`; do not try to bypass it.
- Do not poll in a tight loop. For long renders use `--detach`, then check `hfr jobs --json` no more often than every 30-60 s, or run the blocking `hfr render` in the background.

### Reading state

```bash
hfr projects --json     # [{ "project": {id,name,size_bytes,state}, "jobs": [ {id,state,command,files:[{path,name,size,sha256}],...} ] }]
hfr jobs [project] --json
hfr files <job> --json
hfr logs <job>          # whole log;  hfr logs <job> -f  follows a running render
```

Render `state`: `running` -> `done` | `failed` | `cancelled`. A render's options are the arguments in its `command`. Progress lines in logs look like `@hf-progress {"code":"capture","pct":55}`.

## Cancelling and rendering again

```bash
hfr cancel <job>                                          # stop a running render (whole process tree); it becomes "cancelled"
hfr render <dir> --like <job> --quality delivery --fps 60 # a NEW render in the same project; the earlier render and its files stay
```

- Rendering again always creates a new render; it syncs only what you changed. Use it for "that looked wrong, try again at 60 fps" or "it ran out of memory, use fewer workers".
- A project runs one render at a time: `cancel` the running one first (the CLI reports a conflict otherwise).
- **Only cancel a render the user asked you to.** A cancelled render loses its progress, and the VPS may be running the user's other renders; check `hfr jobs --json` and never touch a render you did not start.
- A render that was running when the agent restarted shows as `failed`; render again to retry.

## Getting the result (and freeing space)

```bash
hfr pull <job> -o <output-dir>            # all files of the render; verified by SHA-256; resumes if interrupted
hfr pull <job> intro.mp4 -o <output-dir>  # one file
hfr pull <job> -o <output-dir> --delete   # delete each file from the VPS after it was verified
hfr rm file <job> <file>                  # delete one output file without downloading
hfr rm job <job>                          # delete a render and its files (the project stays)
hfr rm project <project>                  # delete a project with all renders and files (local folder untouched)
```

**Rules:**
- Only use `--delete` after the user has said they want the space freed, or when they asked you to "download and clean up". The deletion happens only after a verified download. An existing file with the same name is never overwritten; the new one becomes `name (1).mp4`.
- If a download is interrupted, run the same `hfr pull` again: it resumes.
- **Never run `hfr rm` on your own**, and never delete a project, render or file you did not create without the user's explicit yes. `rm` asks for confirmation; do not add `--yes` unless the user told you to. Deleted data cannot be recovered. Download first.
- Report the saved file path and size to the user when done.

## Setup (only when asked, and only with the user's go-ahead)

`hfr setup` is safe to re-run: it checks what is already installed and only installs or upgrades what is missing or outdated. It will not upgrade the agent or hyperframes while a render is running (it says "postponed"); do not work around that, tell the user to re-run when the render is done. `--force` reinstalls everything and should only be used when the user asks. An upgrade keeps the token and all projects.

`hfr setup --host <ip> --user <user> [--key <path>]` installs Node 22, ffmpeg, Chrome libraries, `hyperframes` and the agent on the VPS **as root** (Debian/Ubuntu only; x86_64 or ARM64). It needs the user's SSH key loaded in ssh-agent and root or passwordless sudo. This changes a server: confirm the host and user with the user first, and never edit sudoers or any server file yourself unless explicitly told to.

## Connection problems

| Symptom | Likely cause and fix |
| --- | --- |
| `ssh tunnel exited early`, `Permission denied (publickey)` | SSH key not loaded. The user must run `ssh-add <key>` (you cannot type passphrases). On Windows, run `hfr` from PowerShell: Git Bash's `ssh` cannot see the Windows ssh-agent. |
| `agent returned 401` | Token mismatch (VPS reinstalled?). Ask the user to re-run `hfr setup`. |
| `agent ... too old ... API 1` | Old agent on the VPS. Ask the user to run `hfr setup` when no render is running. |
| `timed out waiting for ssh tunnel` | Network down or the VPS unreachable; check `ssh user@host echo ok` and retry. |
| `agent returned 400 ... not allowed` | Option outside the allowlist. Use only the options listed above. |
| `agent returned 409` | A render is already running in that project (cancel it or wait), or the project has no files yet. |
| `unsafe path in archive` | Project contains symlinks/odd files. Add them to `.hfrignore`. |
| Render `failed` | `hfr logs <job>`; common: out of memory (use `-w 2`), assets outside the project folder, missing fonts. |
| `download verification failed` | Transfer corrupted; run `hfr pull` again. Nothing was deleted. |

More: `docs/troubleshooting.md`.

## Updating HFR itself

`hfr update --check --json` says whether a newer version exists (`available`, `latest`, `notes`); `hfr update` downloads, verifies and installs it (Windows closes the program and restarts it, so only run it when the user asked and nothing is in progress). The VPS agent is updated separately with `hfr setup` (only with the user's go-ahead, and never while a render is running).

## Safety and privacy

- **Never print, log, paste or store the token** (Windows Credential Manager / macOS Keychain / `config.toml`, and `/etc/hfr/agent.env` on the VPS). If you need to show configuration, show host and user only.
- Do not run `hfr init --token ...` with a token copied from chat unless the user supplied it for that purpose.
- Projects are uploaded to the user's own server only; do not send them anywhere else.
- Do not suggest exposing the agent port publicly; it is meant to stay on `127.0.0.1` behind the SSH tunnel.

## Desktop app

`hfr-gui` offers the same actions with live charts. If the user prefers a window, tell them to open **HFR**: *Projects* lists every project; *New render* picks a folder (a known folder offers *Sync changes* / *Render*), shows the options dialog and uploads only changes with a cancellable progress view; a project's page lists its renders and each render's output files with **Download** and **Delete** per file; *Transfers* shows percentage, speed and time left for every upload and download, with pause and resume.
