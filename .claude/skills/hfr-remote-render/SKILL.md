---
name: hfr-remote-render
description: Operate HFR Remote (the `hfr` CLI / `hfr-gui` app) to render a Hyperframes video project on the user's own VPS instead of their PC. Use when the user wants to render a Hyperframes or "hyperframes" project remotely, offload a heavy render, push or upload a project to their server, follow a remote render, check the VPS's CPU/RAM/disk, download the finished video, free space on the VPS, set up or repair the VPS connection, or mentions `hfr`, "HFR Remote" or "hfr-agent".
---

# Rendering Hyperframes projects on the user's VPS with HFR Remote

HFR Remote moves heavy Hyperframes renders off the user's computer. `hfr` packs the project, uploads it through an SSH tunnel to `hfr-agent` on their Linux VPS, the VPS renders it, and `hfr pull` brings back the SHA-256-verified video and can delete the job to free disk.

Full docs: `docs/cli.md`, `docs/troubleshooting.md`, `docs/security.md` in the repository.

## Before you do anything

1. **Is it set up?** Run `hfr status --json`.
   - JSON with `cpu_total`, `mem_total`, `disk_total` -> connected; proceed.
   - `no config ... run hfr init first` -> not set up; go to *Setup*.
   - `ssh tunnel exited early` / `could not connect` -> go to *Connection problems*.
2. **Is the VPS busy?** Look at `cpu_total` and `hfr jobs --json` (jobs with `"state": "running"`). A second heavy render will slow both.
3. **Is there room?** `disk_total - disk_used` should comfortably exceed the project size plus the output (video can be several GB).

## Rendering

```bash
hfr render <project-dir>              # packs, uploads, starts, follows logs until done; exit code != 0 if it failed
hfr render <project-dir> --detach     # returns right after starting; prints the job id
hfr render <dir> --fps 60 --quality delivery --workers 2   # options (defaults in brackets below)
```

Options, the same ones the app's *Render options* dialog offers: `--fps <1-240>` [composition's own], `--quality draft|looks|delivery` [looks], `--format mp4|webm|mov|gif` [mp4; webm and mov keep transparency], `--workers <n>` [auto; each Chrome worker needs ~256 MB RAM, so check `hfr status` first], `--hdr` / `--sdr` [auto-detect], `--composition <file.html>` [index.html]. Leave an option out to keep Hyperframes' default; only pass what the user asked for. `-- <full command>` replaces them all (advanced).

While uploading, a progress line shows percent, bytes, speed and ETA. **Ctrl+C cancels the upload** and removes the half-created job from the VPS. If you run `hfr` in the background and must stop it, terminate the process; do not leave uploads running unattended.

- `<project-dir>` is the folder containing `index.html` / `hyperframes.json`.
- Uploads skip `node_modules`, `.git` and anything in `.hfrignore` (gitignore syntax). If the project has old `renders/` or large unused footage, suggest adding them to `.hfrignore` before uploading; ask before editing user files.
- Only `npx hyperframes render` with these options is accepted: `-c/--composition -o/--output -f/--fps -q/--quality --format -w/--workers --crf --video-bitrate --vp9-cpu-used --variables --variables-file --video-frame-format --gif-loop --hls-segment-seconds --hdr --sdr --quiet --strict --strict-all --gpu --browser-gpu --no-browser-gpu --best-effort --no-best-effort --strict-variables --lint-verbose`. Output/composition/variables-file paths must be relative to the project. Anything else is rejected with `400`; do not try to bypass it.
- Do not poll in a tight loop. For long renders use `--detach`, then check `hfr jobs --json` no more often than every 30-60 s, or run the blocking `hfr render` in the background.

### Reading state

`hfr jobs --json` returns an array:

```json
[{"id":"14511aaa-...","name":"my-project","state":"running","command":["npx","hyperframes","render"],"created_unix":1792960606,"size_bytes":12345678}]
```

`state`: `created` -> `uploaded` -> `running` -> `done` | `failed`.
`hfr logs <id>` prints the whole log; `hfr logs <id> -f` follows a running job. Progress lines look like `@hf-progress {"code":"capture","pct":55}`.

## Getting the result (and freeing space)

```bash
hfr pull <id> -o <output-dir>          # download only; verified by size + SHA-256
hfr pull <id> -o <output-dir> --delete # download, verify, then delete the job on the VPS
hfr rm <id>                            # delete without downloading
```

**Rules:**
- Only use `--delete` after the user has said they want the space freed, or when they asked you to "download and clean up". The deletion happens only after a verified download.
- **Never run `hfr rm` (or delete a job) on your own**, and never delete a job you did not create, without the user's explicit yes. Deleted jobs cannot be recovered.
- Report the saved file path and size to the user when done.

## Setup (only when asked, and only with the user's go-ahead)

`hfr setup --host <ip> --user <user> [--key <path>]` installs Node 22, ffmpeg, Chrome libraries, `hyperframes` and the agent on the VPS **as root** (Debian/Ubuntu only; x86_64 or ARM64). It needs the user's SSH key loaded in ssh-agent and root or passwordless sudo. This changes a server: confirm the host and user with the user first, and never edit sudoers or any server file yourself unless explicitly told to.

## Connection problems

| Symptom | Likely cause and fix |
| --- | --- |
| `ssh tunnel exited early`, `Permission denied (publickey)` | SSH key not loaded. The user must run `ssh-add <key>` (you cannot type passphrases). On Windows, run `hfr` from PowerShell: Git Bash's `ssh` cannot see the Windows ssh-agent. |
| `agent returned 401` | Token mismatch (VPS reinstalled?). Ask the user to re-run `hfr setup`. |
| `timed out waiting for ssh tunnel` | Port 17878 busy or network down; change `local_port` in the config or retry. |
| `agent returned 400 ... not allowed` | Option outside the allowlist. Use only the options listed above. |
| `unsafe path in archive` | Project contains symlinks/odd files. Add them to `.hfrignore`. |
| Render `failed` | `hfr logs <id>`; common: out of memory (use `-w 2`), assets outside the project folder, missing fonts. |
| `download verification failed` | Interrupted transfer; run `hfr pull` again. Nothing was deleted. |

More: `docs/troubleshooting.md`.

## Safety and privacy

- **Never print, log, paste or store the token** (Windows Credential Manager / macOS Keychain / `config.toml`, and `/etc/hfr/agent.env` on the VPS). If you need to show configuration, show host and user only.
- Do not run `hfr init --token ...` with a token copied from chat unless the user supplied it for that purpose.
- Projects are uploaded to the user's own server only; do not send them anywhere else.
- Do not suggest exposing the agent port publicly; it is meant to stay on `127.0.0.1` behind the SSH tunnel.

## Desktop app

`hfr-gui` offers the same actions with live charts. If the user prefers a window, tell them to open **HFR Remote**: *New render* picks a folder, shows the options dialog, uploads with a cancellable progress view, then the job page shows render progress and logs; *Download video* with *Free VPS space after download* ticked does the pull + delete.
