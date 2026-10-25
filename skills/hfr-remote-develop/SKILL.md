---
name: hfr-remote-develop
description: Work on the HFR Remote codebase (Rust workspace with hfr-agent, hfr-core, hfr-cli, hfr-gui on GPUI/gpui-kit, hfr-proto). Use when fixing bugs, adding CLI commands, agent endpoints or GUI screens, changing the VPS installer, touching command validation, archive extraction, authentication or the SSH tunnel, cross-compiling the Linux agent, packaging installers, or updating the docs of this project.
---

# Developing HFR Remote

Read `AGENTS.md`, `docs/architecture.md` and `docs/development.md` first; this skill is the short version plus recipes.

## Mental model

```
hfr / hfr-gui  ->  hfr-core::Client  --ssh -L-->  hfr-agent (127.0.0.1:7878 on the VPS)  -> npx hyperframes render
```

Jobs: `created -> uploaded -> running -> done | failed`, stored in memory in the agent; files under `$HFR_ROOT/<uuid>/{project,log.txt}`. The client packs a tar, streams it up, follows logs (SSE), downloads the output and verifies SHA-256.

## Commands

```bash
cargo build                         # all crates
cargo test -p hfr-agent             # safety tests: allowlist, tar extraction, token
bash scripts/stage-agents.sh        # static Linux agents -> dist/agents/<triple>/hfr-agent (rust-lld, no C toolchain)
cargo check -p hfr-core -p hfr-cli -p hfr-agent --target aarch64-apple-darwin   # cross type-check
cargo packager --release -c Packager.toml --formats nsis                        # installer (needs release builds + staged agents)
```

Run the whole stack locally, no VPS:

```bash
HFR_TOKEN=dev HFR_ROOT=/tmp/hfr HFR_ALLOW_ANY_COMMAND=1 cargo run -p hfr-agent &
cargo run -p hfr-cli -- init --direct-url http://127.0.0.1:7878 --token dev
cargo run -p hfr-cli -- render ./project -- echo hello
```

`init`/`setup` write the user's real config (`%APPDATA%\hfr\config.toml` + credential store). If you test with `--direct-url`, restore or delete it afterwards; do not overwrite a working user config without saying so.

## Where things live

| Task | Files |
| --- | --- |
| Add/change an HTTP endpoint | `crates/agent/src/main.rs` (route + handler), `crates/proto/src/lib.rs` (types), `crates/core/src/lib.rs` (`Client` method), `docs/agent-api.md` |
| Validate input | commands: `crates/proto/src/command.rs` (allowlist, shared); archives and tokens: `crates/agent/src/safety.rs` — **always with tests** |
| Render options (UI and CLI) | `crates/core/src/render.rs` (`RenderOptions` -> command; a test checks all choices pass the allowlist) |
| Upload progress, cancel, speed/ETA | `crates/core/src/transfer.rs`, `Client::upload_dir_tracked` in `crates/core/src/lib.rs` |
| Add a CLI command | `crates/cli/src/main.rs` (`Cmd` enum + match arm), `docs/cli.md` |
| GUI screen/action | `crates/gui/src/main.rs` (`HfrApp`, main views), `crates/gui/src/render_flow.rs` (new-render dialog and upload panel), palette in `theme.rs` |
| VPS install steps | `crates/core/src/install.sh` (idempotent bash), `crates/core/src/setup.rs` |
| Installers/CI | `Packager.toml`, `.github/workflows/{ci,release}.yml`, `scripts/` |

### Recipe: new endpoint
1. Add types to `hfr-proto` if needed. 2. Handler in `agent/main.rs`; take `Path(id)` and **look the id up in the job table before building any path**; register under the auth layer (all routes are). 3. `Client` method in `hfr-core`. 4. Use it from CLI/GUI. 5. Document in `docs/agent-api.md`; add a CHANGELOG line.

### Recipe: allow another render option
Add it to `VALUE_FLAGS` or `BOOL_FLAGS` in `crates/proto/src/command.rs` (and `PATH_FLAGS` if its value is a path), add a test there, then expose it in `RenderOptions` (`crates/core/src/render.rs`), the dialog in `render_flow.rs` and the CLI `RenderArgs`; update `docs/cli.md`, `docs/gui.md`, the skill `hfr-remote-render` and `docs/agent-api.md`.

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
- Job state is in memory: an agent restart loses the list (known limitation).
- **install.sh runs as the SSH user, not root.** Any test on a path inside `/etc/hfr` or `/var/lib/hfr` (both mode 750) must use `$SUDO test ...`, otherwise it silently reports "missing". This once made setup generate a new token while the old agent kept running (401 for every client). Never regenerate `/etc/hfr/agent.env` when it exists; the script prints the token the *running* agent holds (from `/proc/<pid>/environ`) and repairs the file if they differ.
- **Never disturb a running render.** The installer skips the agent replacement and the global `npm install -g hyperframes` while a job is `running`, and holds the agent update while job folders without `meta.json` (created by an older agent) exist. Keep that behaviour when you change `install.sh`; test with a render in progress.
- **Cancel kills a process group**: the agent spawns renders with `process_group(0)` and sends SIGTERM then SIGKILL to `-pgid`. This is Unix-only code; test it for real (WSL: run the musl agent with a fake `npx` that spawns `sleep`), not just on Windows.
- **Job state is persisted** (`<root>/<id>/meta.json`, `crates/agent/src/store.rs`); call `persist(..)` after every state or command change.
- **Windows PowerShell 5.1 pipes add a BOM and CRLF** when piping text into `ssh`/`bash -s`; copy a script with `scp` and run it instead.
- **Testing against a throw-away agent** without touching the real config: `HFR_CONFIG=<file>` with `direct_url = "http://127.0.0.1:<port>"` (never touches the credential store). The Linux agent in WSL is reachable from Windows on localhost.
- **Driving the GUI from a script** (screenshots, clicks): before every click or keystroke verify the foreground window belongs to `hfr-gui` (compare `GetWindowThreadProcessId` of `GetForegroundWindow` with the app pid) and wait for `MainWindowHandle` first; otherwise input lands in whatever window the user has open. Never name a PowerShell function `Type` (alias of `Get-Content`).
- Test uploads/renders against a real VPS only with a job you created, and delete only that job; the user may have real renders there.
- Screenshots for docs must not contain real IPs/usernames/tokens.

## Definition of done

`cargo build` and `cargo test -p hfr-agent` pass; cross `cargo check` for at least one other OS if you touched `hfr-core`/`hfr-cli`; docs + CHANGELOG updated; for GUI changes, a screenshot checked by eye; for installer or agent changes, a real run against a VPS (ask the user) or the local `--direct-url` flow.
