# Troubleshooting

## "ssh tunnel exited early" / "could not connect"

`ssh` ran non-interactively and failed. Check, in this order:

1. `ssh user@host echo ok` works **without a prompt**. If it asks for a passphrase, load the key: `ssh-add <key>` ([guide](getting-started.md#2-let-hfr-use-your-ssh-key)).
2. The key is the one the server accepts (`ssh -v user@host` shows which keys are offered).
3. On Windows with Git for Windows installed: run `hfr` from PowerShell. Git's `ssh` cannot see the Windows ssh-agent.
4. The host, user and port in the config are right (Settings page, or `config.toml`).
5. Port 17878 on your computer is free (change `local_port` in `config.toml` otherwise).

## Permission denied (publickey,password) during setup

Same as above; setup also runs with `BatchMode=yes`, so password logins are never offered. Use a key.

## Installer: "sudo: a password is required"

The SSH user needs root or passwordless sudo for setup. See [step 3](getting-started.md#3-allow-passwordless-sudo-non-root-users-only).

## "only Debian/Ubuntu is supported by this installer"

The installer uses `apt`. On other distributions install Node 22, ffmpeg, Chrome libraries and `npm i -g hyperframes` yourself, copy `hfr-agent` to `/usr/local/bin`, create the service (see `crates/core/src/install.sh` for the exact unit), then `hfr init --host ... --token ...`.

## "Linux agent for this VPS not found"

`hfr setup` could not find a bundled or built agent. Build it and retry:

```bash
bash scripts/stage-agents.sh      # or:
rustup target add x86_64-unknown-linux-musl
cargo build -p hfr-agent --release --target x86_64-unknown-linux-musl
```
or pass `--agent <path>`. Use `aarch64-unknown-linux-musl` for ARM VPSes.

## `agent returned 400 Bad Request: ...`

- `only npx hyperframes render [options] is allowed` / `flag ... is not allowed`: the command was rejected by the allowlist ([list](cli.md)).
- `unsafe path in archive` / `disallowed entry type`: the project contains something the agent will not extract (symlinks and similar). Remove it or add it to `.hfrignore`.

## `agent returned 401`

The saved token does not match `/etc/hfr/agent.env` on the VPS (for example after reinstalling the VPS). Re-run `hfr setup`, or `hfr init --token <value>`.

## Render fails on the VPS

`hfr logs <job-id>` shows the full output. Common causes:

- Chrome download blocked or incomplete: `ssh vps "sudo -u hfr -H hyperframes doctor"`.
- Out of memory with many workers: each Chrome worker needs roughly 256 MB. Pass `-w 2`.
- A composition referencing files outside the project folder (absolute local paths). Keep assets inside the project.
- The project pins another Hyperframes version than the VPS has; check `hyperframes --version` on both.

## "download verification failed"

Size or SHA-256 differed from what the VPS reported (interrupted transfer). The partial file is deleted; run `hfr pull` again. Nothing is removed from the VPS when verification fails.

## The app starts but shows Offline

The tunnel dropped (laptop sleep, network change). Restart the app; it reconnects at startup. Your renders keep running on the VPS.

## Jobs disappeared after restarting the agent

Job state is in memory ([limitations](architecture.md#known-limitations)). The folders are still in `/var/lib/hfr/jobs/`; the video can be copied out with `scp` and the folder removed with `sudo rm -rf`.

## Disk is filling up

`hfr jobs` and delete finished ones (`hfr rm <id>`). `hfr status` shows free space. Orphaned folders after an agent restart (see above) must be removed by hand.
