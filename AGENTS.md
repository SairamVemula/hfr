# AGENTS.md

Guide for AI coding agents (and humans in a hurry) working in or with this repository.

## What this is

HFR renders [Hyperframes](https://github.com/heygen-com/hyperframes) video projects on the user's own Linux VPS. A small agent (`hfr-agent`) runs on the VPS; a CLI (`hfr`) and a desktop app (`hfr-gui`) sync projects to it through an SSH tunnel (only changed files are uploaded), render them with any options, stream logs and server metrics, download each output file (verified, resumable) and delete files, renders or projects to free disk space.

## If you are **using** the tool for someone

Read [`skills/hfr-remote-render/SKILL.md`](skills/hfr-remote-render/SKILL.md). It has the commands, safety rules (confirm before deleting or running setup), JSON output formats and an error-to-fix table.

Quick facts:
- A **project** (folder on the VPS) has many **renders**; each render lists its output **files**. `hfr projects --json` / `hfr jobs --json` / `hfr files <job> --json` list them; `hfr status --json` proves the connection works and shows VPS load.
- `hfr render <dir>` syncs only changes, starts a render and blocks until done; `--detach` returns immediately. Rendering again with other options uploads nothing.
- `hfr pull <job> [file] --delete` downloads (resumes if interrupted), verifies SHA-256, then frees the VPS space. Never delete before the user has the video. `hfr rm project|job|file` asks before deleting.
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
| `crates/core` | Client library: `client.rs`, `sync.rs` (delta upload), `registry.rs` (folder -> project), `download.rs` (resumable), `transfer.rs` (progress/cancel), `render.rs` (options -> command), `setup.rs` / `install.sh` (VPS installation) |
| `crates/cli` | `hfr` (clap) |
| `crates/gui` | `hfr-gui` (GPUI via `gpui-kit`): `projects_view.rs`, `dialogs.rs`, `transfers.rs`, `pages.rs`; `theme.rs` palette |
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

## Agent skills

### Issue tracker

GitHub Issues on SairamVemula/hfr, via the `gh` CLI. See `docs/agents/issue-tracker.md`.

### Triage labels

Default vocabulary: needs-triage, needs-info, ready-for-agent, ready-for-human, wontfix. See `docs/agents/triage-labels.md`.

### Domain docs

Single-context: one `GLOSSARY.md` and `docs/adr/` at the repo root. See `docs/agents/domain.md`.
