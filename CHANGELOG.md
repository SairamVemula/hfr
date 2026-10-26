# Changelog

All notable changes are listed here. The format follows [Keep a Changelog](https://keepachangelog.com/) and the project aims to follow [Semantic Versioning](https://semver.org/) once it reaches 1.0.

## [Unreleased]

### Added
- **Render queue.** The agent now runs a limited number of renders at once; the rest wait as `queued` (with their place in line) and start by themselves, oldest first, one render per project. The limit is chosen automatically from the VPS (one per 4 CPU cores and 4 GiB of memory, at least 1) and can be changed in the app (VPS > Render queue), with `hfr limit`, or `PUT /settings`. A project no longer rejects a second render while one runs: it queues. Queued renders survive an agent restart; the installer counts them as busy. App: *Queued* pill, queue notice, Remove from queue. CLI: `hfr queue`, `hfr limit`, waiting/started messages in `hfr render`. Agent API 3 (`MIN_API` stays 2: a new app works with an older agent, which simply has no queue).

### Changed
- **Rebranded as HFR** (Hyperframes Render): new logo (an H of two film strips with a play button), new app icon and a full brand kit (`assets/branding`: colourways, dark/light/mono marks, wordmark lockups, GitHub social preview), generated from one source by `node build.mjs`. The product name is now "HFR" everywhere it is shown (window title, installer, download folder `Downloads/HFR`). If you installed an earlier build, uninstall "HFR Remote" first.

## [0.1.2] - 2026-10-26

(`v0.1.1` was tagged but its release workflow failed before publishing anything; this is the first published release.)

### Distribution
- Releases carry installers for **Windows (x64, ARM64)**, **Linux (x64, ARM64)** and **macOS (Apple Silicon, Intel)**. Every build except Windows x64 and Linux x64 is marked experimental: it may fail without blocking the release. The update check only offers an installer for the computer's own CPU architecture.
- Fixed the release workflow (the agent staging script was not executable in git).

### Changed (breaking: agent API 2)
- **Projects, renders and files.** The model is now *project -> renders -> output files* instead of one flat job per upload. A project is a folder on the VPS; rendering it again with other options creates a new render in the same project and uploads nothing. The old `/jobs/...` endpoints are gone; see [docs/agent-api.md](docs/agent-api.md). Clients refuse an older agent and say to run `hfr setup`. An upgraded agent migrates existing job folders into projects with one render each.
- **Only changes are uploaded.** Syncing compares SHA-256 manifests (with a local hash cache) and sends just the new or changed files; deleted files are removed on the VPS. An unchanged folder uploads 0 bytes.
- **Folders are remembered by path** (`projects.json` next to the config). Two folders with the same name are two projects, shown with their paths. Picking a known folder offers Sync / Render / Open / Create a separate project.
- One render runs per project at a time (`409` otherwise); renders in different projects can run together.
- CLI: `hfr push` creates or syncs a project, `hfr render` syncs then renders (`--project`, `--new`, `--no-sync`, `--like`), `hfr rm project|job|file`, `hfr pull <job> [file]`. New `hfr projects` and `hfr files`. `hfr rerun` and `hfr start` are replaced by `hfr render --like <job>`.
- App rebuilt around projects: Projects list, a project page with its renders, Files and Log tabs, per-file Download and Delete, and a Transfers page.

### Added
- **Updates.** The app (and `hfr update`) checks GitHub Releases at start-up and at most hourly, shows a banner with the release notes, downloads the installer with progress, speed and time left, verifies a signed checksum list (ed25519, key built into the app) and the installer's SHA-256, then installs it (Windows: silent install and restart; macOS/Linux: opens the verified file). Switchable in Settings; only `github.com` is contacted, via the system `curl`. The release workflow now checks the tag against the versions, refuses stale bundled agents, builds a draft and publishes it only after signing `SHA256SUMS`.
- **Resolution** option in the Render options dialog and as `--resolution` (1080p/4K landscape, portrait, square) in the CLI.
- The app accepts any compatible agent API (`MIN_API`..=`API_VERSION`) and shows a soft banner when the VPS agent is older than the app.
- A rejected render option now says the agent on the VPS is older than the app and points to Upgrade agent.
- Each SSH tunnel uses a free local port, and the app closes its tunnel before upgrading the agent (a stale tunnel could make an upgraded agent look old).
- Agent binaries carry an `hfr-agent-api=N` marker; `hfr setup` refuses a bundled agent of the wrong API.
- **Per-file download and delete**, per-render and per-project delete, each behind a confirmation that names what is removed. Output files are recorded per render (a modification-time window inside the render, which is why only one render runs per project).
- **Download experience:** percentage, bytes, speed and time left, pause and resume, resume after a lost connection or restart (`Range` requests, `.hfrpart` files), SHA-256 verification before the file appears, and no overwriting (`name (1).mp4`). A compact transfer bar shows on every page; the CLI prints the same progress line.
- Download folder and *free the VPS copy after a verified download* are saved in the config (`download_dir`, `free_after_download`).
- `scripts/e2e-agent.sh` (57 checks of the HTTP API against a real agent binary with a fake `npx`) and a CI job that runs it; a Rust integration test drives the agent through `hfr-core`.
- `GET /status`, used by the installer to detect a running render.
- `theme::accent_text()`: a lighter violet for text on dark panels (contrast 4.5:1).

### Fixed
- `hfr setup` could generate a new token when run by a non-root SSH user (it could not see `/etc/hfr`), leaving a still-running agent and the client with different tokens. File tests in the installer now run as root, the existing token is never regenerated, and the token handed to the client is read from the running agent.
- A render started from the app's upload flow did not stream its log until the next refresh.
- On Windows the app overflowed the 1 MB main-thread stack in debug builds when a transfer was shown; the executable now reserves 16 MB.

### Added earlier in 0.1 (parts superseded by the project model above)
- **Cancel a render and re-render with different options.** Agent: `POST /jobs/{id}/cancel` (stops the whole process tree) and `POST /jobs/{id}/rerun` (reuses the uploaded project, no re-upload; cancels first when running), new `cancelled` job state. App: **Cancel render** (with a confirm step), **Change options and restart** / **Re-render with other options** using the same Render options dialog, pre-filled. CLI: `hfr cancel`, `hfr rerun`.
- **Idempotent installer:** `hfr setup` checks what is already installed (system packages, Node 22, hyperframes, Chrome, agent) and only installs or upgrades what is missing or outdated, with a per-step report; `--force` reinstalls everything. It never interrupts a running render and holds the agent update while jobs from an older agent are on the VPS.
- **Job state survives agent restarts** (`meta.json` per job); a render that was running at that moment shows as failed.
- `HFR_CONFIG` to point the client at another config file (development, tests).
- New render flow in the app: folder picker, **Render options** dialog (composition, frame rate, quality, format, colour range, workers; defaults preset), then an upload view with progress bar, speed, time left and **Cancel upload** (the half-created job is removed from the VPS).
- `hfr render` / `hfr push` options (`--fps`, `--quality`, `--format`, `--workers`, `--hdr`/`--sdr`, `--composition`), an upload progress line and Ctrl+C cancel.
- GIF output is recognised as a render result; the command allowlist moved into `hfr-proto` so clients can test against it.
- Rust workspace: `hfr-agent` (VPS service), `hfr-core` (client library), `hfr` (CLI), `hfr-gui` (desktop app, GPUI), `hfr-proto` (shared types).
- `hfr setup`: one-command VPS installation (agent, Node 22, ffmpeg, Chrome, hyperframes, hardened systemd service) for x86_64 and ARM64 Debian/Ubuntu.
- Jobs: upload (tar over an SSH tunnel), render, live logs, verified download (SHA-256), delete.
- Live VPS metrics (CPU per core, memory, swap, disk) in the app and `hfr status`.
- Dark desktop UI with progress bar, first-run setup wizard and settings; app icon and branding.
- Security: loopback-only agent, SSH tunnel, bearer token, command allowlist, safe archive extraction, constant-time token check, systemd sandbox, token in the OS credential store.
- `--json` output for `hfr jobs` and `hfr status`.
- Installers via cargo-packager (NSIS, DMG, AppImage, deb) and CI for Windows, macOS and Linux.
- Documentation, agent skills and contribution guides.
