# Security

HFR Remote runs a service that executes a render command on your server, so its security model matters. This page is what is actually implemented, and what is not.

To report a vulnerability, see [SECURITY.md](../SECURITY.md).

## Threat model

**Protected against:** other people on the internet or on your VPS's network reaching the agent; someone without your SSH key reading or controlling jobs; a leaked token alone being enough to run arbitrary commands; malicious archives writing outside a job's folder; the agent process damaging the VPS.

**Not protected against:** someone who has both your SSH access and the token (they can render in, read and delete any project); malicious *project content* you choose to render (Hyperframes compositions are web pages and can run JavaScript in Chrome on the VPS); a compromised VPS (it holds your project files while they render); anyone with admin rights on your own computer.

Do not use a VPS that holds other secrets as a render box for untrusted projects.

## How the connection is protected

| Layer | Control |
| --- | --- |
| Network exposure | The agent listens on `127.0.0.1` only. No firewall port is opened by HFR. |
| Transport | All traffic goes through an SSH tunnel (`ssh -L`), so it is encrypted and authenticated by your SSH key. The HTTP inside the tunnel is plain by design. |
| Authentication | Bearer token on every request: 32 random bytes (hex), generated on the VPS by `openssl rand`. Compared in constant time; failures are delayed 400 ms. |
| SSH credentials | Never typed into or stored by HFR. `ssh` is run with `BatchMode=yes`; keys come from ssh-agent. First connection uses `StrictHostKeyChecking=accept-new` during setup (trust on first use; later connections verify against `known_hosts`). |
| Token at rest (client) | Windows Credential Manager / macOS Keychain; on Linux a `0600` config file. |
| Token at rest (VPS) | `/etc/hfr/agent.env`, mode `0640`, `root:hfr`. Never passed on a command line. |

## What the agent will run

Only `npx hyperframes render` with an allowlist of options; no shell is involved, and output/composition paths must stay inside the project ([list](cli.md)). A stolen token therefore cannot be used to run arbitrary shell commands through the API. `HFR_ALLOW_ANY_COMMAND=1` removes this protection and must never be set in production.

## Uploads

Archives are extracted in Rust (not with `tar`): only regular files and directories, only relative paths without `..`, no links or devices, permissions not preserved. Project and job ids are server-generated UUIDs and are checked against the store before any path is built from them. Sync paths are validated like archive entries, and the file download and delete routes only accept paths recorded as outputs of that job, so a client cannot read or delete source files or anything outside the project through them.

## Updates

The app can check GitHub for a newer release (Settings > *Check for updates automatically*; also `hfr update --check`). Only `api.github.com` and `github.com` are contacted, through the system `curl`, and the only thing sent is `User-Agent: hfr/<version>`. An update is never run unless the release's `SHA256SUMS` carries a valid ed25519 signature from a public key compiled into the app and the downloaded installer's SHA-256 matches the signed list; a mismatch deletes the file. A compromised GitHub account alone therefore cannot push a malicious update; the signing key (a GitHub Actions secret) is what must be protected. Windows SmartScreen may still warn about an installer that is not code-signed.

## Service sandbox

The systemd unit runs as the unprivileged user `hfr` (no login shell, no sudo) with `NoNewPrivileges`, `ProtectSystem=strict` (read-only OS; writable only `/var/lib/hfr`), `PrivateTmp`, `ProtectHome`, kernel-tunable/module/cgroup protection, `RestrictSUIDSGID` and `LockPersonality`. Namespace restrictions are deliberately not applied, because Chrome needs them for its own sandbox.

## What `hfr setup` trusts

It runs as root on your VPS and downloads from: Ubuntu/Debian `apt` mirrors, NodeSource (`deb.nodesource.com/setup_22.x`, executed as root), the npm registry (`hyperframes`), and the Chrome download performed by `hyperframes browser ensure`. Review `crates/core/src/install.sh` (it is short) before running it on a machine you care about.

## Known weaknesses

1. Anyone who can reach the tunnel's local port (`127.0.0.1:17878`) on your computer *and* has the token can use the agent. The token protects against other local processes only as long as it stays secret.
2. The Linux client keeps the token in a file (permissions `0600`), which any process running as you can read.
3. Rendering untrusted compositions executes their JavaScript on the VPS, in the `hfr` account.
4. There are no per-job quotas, timeouts or rate limits beyond the failed-login delay, so a runaway render can fill the disk or CPU.
5. `setup` needs passwordless sudo for the SSH user; remove the rule afterwards if you do not plan to upgrade.
6. The agent has had no external security audit.

## Hardening checklist

- Use a dedicated VPS (or at least a dedicated account) for rendering.
- Use an SSH key with a passphrase, loaded through ssh-agent.
- Remove the temporary sudo rule after setup: `sudo rm /etc/sudoers.d/<user>`.
- Keep the VPS patched (`unattended-upgrades`) and its firewall closed except SSH.
- Rotate the token if a laptop is lost ([configuration.md](configuration.md#rotating-the-token)).
- Delete finished renders promptly (`hfr pull --delete`) so project files do not linger on the server.
