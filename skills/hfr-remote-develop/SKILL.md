---
name: hfr-remote-develop
description: Work on the HFR codebase (Rust workspace with hfr-agent, hfr-core, hfr-cli, hfr-gui on GPUI/gpui-kit, hfr-proto). Use when fixing bugs, adding CLI commands, agent endpoints or GUI screens, changing the VPS installer, touching command validation, archive extraction, authentication or the SSH tunnel, cross-compiling the Linux agent, packaging installers, or updating the docs of this project.
---

# Developing HFR

Read `AGENTS.md`, `docs/architecture.md` and `docs/development.md` first; this skill is the short version plus recipes.

## Mental model

```
hfr / hfr-gui  ->  hfr-core::Client  --ssh -L-->  hfr-agent (127.0.0.1:7878 on the VPS)  -> npx hyperframes render
```

Data model: **Project** (a folder on the VPS) -> many **Jobs** (`running -> done | failed | cancelled`, one running per project) -> **OutputFiles**. On disk: `$HFR_ROOT/<project-id>/{project.json, project/, jobs/<job-id>/{job.json, log.txt}}`. The client syncs only changed files (manifest -> `sync/plan` -> tar of the needed files), follows logs (SSE), downloads files with `Range` resume and verifies SHA-256. Local folders map to projects through `projects.json` (`crates/core/src/registry.rs`). API version is `API_VERSION` in `hfr-proto`; bump it for breaking changes (clients refuse older agents).

## Commands

```bash
cargo build                         # all crates
cargo test --workspace --exclude hfr-gui   # unit + the agent integration test (crates/agent/tests/e2e.rs)
cargo test -p hfr-agent             # safety tests: allowlist, tar extraction, token
bash scripts/e2e-agent.sh           # in Linux/WSL: ~57 API checks against the musl agent binary with a fake npx
bash scripts/stage-agents.sh        # static Linux agents -> dist/agents/<triple>/hfr-agent (rust-lld, no C toolchain)
cargo check -p hfr-core -p hfr-cli -p hfr-agent --target aarch64-apple-darwin   # cross type-check
cargo packager --release -c Packager.toml --formats nsis                        # installer (needs release builds + staged agents)
```

Run the whole stack locally, no VPS:

```bash
HFR_TOKEN=dev HFR_ROOT=/tmp/hfr HFR_ALLOW_ANY_COMMAND=1 cargo run -p hfr-agent &
export HFR_CONFIG=/tmp/hfr-test.toml          # never touches your real config or credentials
cargo run -p hfr-cli -- init --direct-url http://127.0.0.1:7878 --token dev
cargo run -p hfr-cli -- render ./project -- echo hello
```

`init`/`setup` write the user's real config (`%APPDATA%\hfr\config.toml` + credential store) unless `HFR_CONFIG` points elsewhere. Always set `HFR_CONFIG` for tests; do not overwrite a working user config. The registry and hash cache live next to the config file, so they are isolated too.

## Where things live

| Task | Files |
| --- | --- |
| Add/change an HTTP endpoint | `crates/agent/src/{main,projects,jobs,files}.rs` (route in `main.rs`, handler in the module), `crates/proto/src/lib.rs` (types), `crates/core/src/client.rs` (`Client` method), `docs/agent-api.md`, `scripts/e2e-agent.sh` |
| Projects/jobs on disk, migration | `crates/agent/src/store.rs` (call `persist` after every change) |
| Validate input | commands: `crates/proto/src/command.rs` (allowlist, shared); archives, paths and tokens: `crates/agent/src/safety.rs` — **always with tests** |
| Render options (UI and CLI) | `crates/core/src/render.rs` (`RenderOptions` <-> command; a test checks all choices pass the allowlist) |
| Delta sync, hash cache | `crates/core/src/sync.rs`, `plan_sync` in `crates/proto/src/lib.rs` |
| Known folders -> projects | `crates/core/src/registry.rs` |
| Resumable downloads | `crates/core/src/download.rs` (`.hfrpart` + sidecar, collision naming), Range handling in `crates/agent/src/files.rs` |
| Progress, cancel, speed/ETA | `crates/core/src/transfer.rs` (`Transfer`, `SpeedMeter`, `eta`, `fmt_*`) |
| Add a CLI command | `crates/cli/src/main.rs` (`Cmd` enum + match arm), `docs/cli.md` |
| GUI screen/action | `crates/gui/src/main.rs` (`HfrApp`, shell, polling), `projects_view.rs` (list and project page), `dialogs.rs` (options and confirmations), `transfers.rs`, `pages.rs` (VPS, settings), palette in `theme.rs` |
| VPS install steps | `crates/core/src/install.sh` (idempotent bash), `crates/core/src/setup.rs` |
| Installers/CI | `Packager.toml`, `.github/workflows/{ci,release}.yml`, `scripts/` |

### Recipe: new endpoint
1. Add types to `hfr-proto` if needed. 2. Handler in the right `agent/` module; take `Path(..)` ids and **look them up in the store before building any path**; any user-supplied relative path (like `?path=`) must be checked against the recorded files or `safety.rs` rules; register under the auth layer (all routes are). 3. `Client` method in `hfr-core/src/client.rs`. Add a check to `scripts/e2e-agent.sh`. 4. Use it from CLI/GUI. 5. Document in `docs/agent-api.md`; add a CHANGELOG line.

### Recipe: allow another render option
Add it to `VALUE_FLAGS` or `BOOL_FLAGS` in `crates/proto/src/command.rs` (and `PATH_FLAGS` if its value is a path), add a test there, then expose it in `RenderOptions` (`crates/core/src/render.rs`, both directions: command and `from_command`), the dialog in `crates/gui/src/dialogs.rs` and the CLI `RenderArgs`; update `docs/cli.md`, `docs/gui.md`, the skill `hfr-remote-render` and `docs/agent-api.md`.

## Hard rules

- **No arbitrary command execution**, no shell strings, no logging of tokens. Keep the allowlist strict.
- **No C or TLS dependencies** in `hfr-core`, `hfr-agent`, `hfr-proto` (keeps musl cross-builds trivial and the attack surface small).
- Agent must stay bound to loopback by default; the installer must not open ports.
- Don't push, tag, release or touch the user's VPS without explicit approval.
- Keep docs and skills in sync with behavior; run `bash scripts/sync-skills.sh` after editing a skill.

## Gotchas

- **`bash -s` + stdin:** a command in a piped script can eat the rest of the script. The installer is run as `bash -c "$(cat)"`.
- **Windows:** Git Bash `ssh` cannot reach the Windows ssh-agent; use PowerShell. `MSYS_NO_PATHCONV=1` stops Git Bash rewriting `/c`. A running `hfr-gui.exe` locks `target/debug/hfr-gui.exe`; stop it before rebuilding.
- **Editing during builds:** don't change sources/Cargo.toml while a `cargo build` is compiling them.
- **gpui-kit:** `use gpui_kit::*` = GPUI; components in `gpui_kit::component::*`; icons `gpui_kit::assets::IconName` (only the bundled default icon set renders: check `default-icons.txt` in the `gpui-kit-assets` crate source); `Theme::update(cx, |t| t.colors.x = ...)` for colours (buttons use `button_primary*`, not `primary`); scrolling needs `.id(..)` before `.overflow_y_scroll()`; async from GPUI to tokio works by awaiting `Handle::spawn` JoinHandles.
- **cargo-packager** only finds config with `-c Packager.toml` in this workspace; NSIS key is `installer-mode`.
- **install.sh runs as the SSH user, not root.** Any test on a path inside `/etc/hfr` or `/var/lib/hfr` (both mode 750) must use `$SUDO test ...`, otherwise it silently reports "missing". This once made setup generate a new token while the old agent kept running (401 for every client). Never regenerate `/etc/hfr/agent.env` when it exists; the script prints the token the *running* agent holds (from `/proc/<pid>/environ`) and repairs the file if they differ.
- **Never disturb a running render.** The installer skips the agent replacement and the global `npm install -g hyperframes` while a job is `running`, and holds the agent update while folders with neither `meta.json` nor `project.json` exist. Busy detection uses `GET /status` (`running_jobs`) and falls back to `GET /jobs` for old agents. Keep that behaviour when you change `install.sh`; test with a render in progress.
- **Cancel kills a process group**: the agent spawns renders with `process_group(0)` and sends SIGTERM then SIGKILL to `-pgid`. This is Unix-only code; test it for real (WSL: run the musl agent with a fake `npx` that spawns `sleep`), not just on Windows.
- **State is persisted** (`project.json`, `jobs/<id>/job.json`, `crates/agent/src/store.rs`); call `persist(..)` after every state or command change. Startup migrates the old `<id>/{project,log.txt,meta.json}` layout; keep that test passing.
- **One running job per project** is what makes output attribution (files modified inside the job's window) correct. Do not relax it without redesigning that.
- **The GUI main thread on Windows has a 16 MB stack** (`crates/gui/build.rs`); the default 1 MB overflows in debug builds when GPUI renders deep element trees.
- **Windows PowerShell 5.1 pipes add a BOM and CRLF** when piping text into `ssh`/`bash -s`; copy a script with `scp` and run it instead.
- **Testing against a throw-away agent** without touching the real config: `HFR_CONFIG=<file>` with `direct_url = "http://127.0.0.1:<port>"` (never touches the credential store). The Linux agent in WSL is reachable from Windows on localhost.
- **Driving the GUI from a script** (screenshots, clicks): before every click or keystroke verify the foreground window belongs to `hfr-gui` (compare `GetWindowThreadProcessId` of `GetForegroundWindow` with the app pid) and wait for `MainWindowHandle` first; otherwise input lands in whatever window the user has open. Never name a PowerShell function `Type` (alias of `Get-Content`).
- Test uploads/renders against a real VPS only with a project you created, and delete only that; the user may have real renders there. Prefer the WSL/local rig.
- **A new app needs agent API 2.** If the user's VPS still runs the old agent, do not upgrade it while their render is running; ask.
- Screenshots for docs must not contain real IPs/usernames/tokens.

## Definition of done

`cargo build` and `cargo test -p hfr-agent` pass; cross `cargo check` for at least one other OS if you touched `hfr-core`/`hfr-cli`; docs + CHANGELOG updated; for GUI changes, a screenshot checked by eye; for installer or agent changes, a real run against a VPS (ask the user) or the local `--direct-url` flow.
