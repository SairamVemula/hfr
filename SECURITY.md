# Security policy

## Reporting a vulnerability

Please **do not open a public issue** for security problems. Use GitHub's private reporting: on the repository page choose **Security → Report a vulnerability**. Include what you found, how to reproduce it, and the version (`hfr --version` / app version, plus the VPS OS).

You can expect an acknowledgement within a few days. Fixes are released as soon as they are ready and credited to the reporter unless you prefer otherwise.

## Scope

In scope: the agent (`crates/agent`), authentication and the tunnel handling in `crates/core`, the installer script and systemd unit, archive extraction, command validation, how credentials are stored.

Out of scope: vulnerabilities in Hyperframes, Chrome, ffmpeg, Node or your VPS provider; attacks that need an already-compromised VPS or an attacker with your SSH key *and* the token (see the [threat model](docs/security.md#threat-model)).

## Supported versions

Only the latest release receives fixes while the project is pre-1.0.
