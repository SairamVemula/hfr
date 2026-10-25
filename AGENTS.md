# AGENTS.md

Guide for AI coding agents (and humans in a hurry) working in or with this repository.

## What this is

HFR Remote renders [Hyperframes](https://github.com/heygen-com/hyperframes) video projects on the user's own Linux VPS. A small agent (`hfr-agent`) runs on the VPS; a CLI (`hfr`) and a desktop app (`hfr-gui`) push projects to it through an SSH tunnel, stream logs and server metrics, download the verified result and delete the job to free disk space.

## If you are **using** the tool for someone

Read [`skills/hfr-remote-render/SKILL.md`](skills/hfr-remote-render/SKILL.md). It has the commands, safety rules (confirm before deleting or running setup), JSON output formats and an error-to-fix table.

Quick facts:
- `hfr status --json` proves the connection works and shows VPS load; `hfr jobs --json` lists jobs.
- `hfr render <dir>` blocks until done; `--detach` returns immediately.
- `hfr pull <id> --delete` downloads, verifies SHA-256, then frees the VPS space. Never delete before the user has the video.
- Never print or log the token. SSH keys come from ssh-agent; you cannot type passphrases.

## If you are **changing** the code

Read [`skills/hfr-remote-develop/SKILL.md`](skills/hfr-remote-develop/SKILL.md) and [docs/development.md](docs/development.md). Essentials:

```bash
cargo build                    # all crates
cargo test -p hfr-agent        # safety tests (allowlist, tar, token)
bash scripts/stage-agents.sh   # static Linux agents (x86_64 + aarch64) via rust-lld, no C toolchain
```

| Path | Role |
| --- | --- |
| `crates/proto` | Wire types and the command allowlist (`command.rs`) shared by agent and clients |
| `crates/agent` | VPS service (axum). `safety.rs` = archive extraction, token check |
| `crates/core` | Client library, `render.rs` (options -> command), `transfer.rs` (upload progress/cancel), `setup.rs` / `install.sh` (VPS installation) |
| `crates/cli` | `hfr` (clap) |
| `crates/gui` | `hfr-gui` (GPUI via `gpui-kit`); `theme.rs` palette |
| `docs/` | User and developer documentation (keep in sync with code) |
| `skills/` | Agent skills (mirrored to `.claude/skills/` by `scripts/sync-skills.sh`) |

## Rules

1. **Security first.** The agent executes commands on a server. Anything that validates input (`safety.rs`, upload extraction, auth) needs tests. Never add a way to run arbitrary shell commands. Never log tokens. See [docs/security.md](docs/security.md).
2. **No C/TLS dependencies in client or agent**: the agent must keep cross-compiling to static musl with `rust-lld`; the client only speaks HTTP over the SSH tunnel.
3. **Don't push, publish, tag or release** unless the user explicitly asks. Don't touch the user's VPS (`hfr setup`, `hfr rm`, sudoers) without explicit approval.
4. **Docs and CHANGELOG** change with behavior. API changes go in [docs/agent-api.md](docs/agent-api.md) and `crates/proto`.
5. **The installer must never disturb a running render or change an existing token.** `crates/core/src/install.sh` is idempotent and runs as the SSH user: test files under `/etc/hfr` and `/var/lib/hfr` with `$SUDO`, postpone agent/hyperframes upgrades while a job is running, and keep the token the running agent uses. Details in the `hfr-remote-develop` skill.
6. **Windows quirks:** test `ssh` from PowerShell (Git Bash's ssh cannot see the Windows ssh-agent); use `MSYS_NO_PATHCONV=1` in Git Bash when passing `/c`-style args.
7. Finish builds before editing files they compile; don't leave `target/` locked by a running `hfr-gui.exe`.

## Useful entry points for an LLM

- [`llms.txt`](llms.txt): one-page index of the documentation.
- [docs/agent-api.md](docs/agent-api.md): complete HTTP API.
- [docs/architecture.md](docs/architecture.md): data flow, lifecycle, known limitations (good first issues).
