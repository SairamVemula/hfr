# Contributing to HFR Remote

Thanks for helping! This project is small enough that most changes need no prior discussion; for anything large (new endpoint, new platform, UI overhaul) please open an issue first.

## Ways to help

- **Try it on macOS or Linux** and report what breaks (the clients build there but have had little manual testing).
- **Distros:** add an installer path for Fedora/RHEL, Alpine or Arch ([`crates/core/src/install.sh`](crates/core/src/install.sh)).
- **Known limitations** in [docs/architecture.md](docs/architecture.md#known-limitations): persistent job state, cancel, resumable transfers, per-job timeouts.
- Docs, screenshots, translations, bug reports with logs.

## Setup

See [docs/development.md](docs/development.md). In short: install Rust, then `cargo build` and `cargo test -p hfr-agent`. You can run the whole stack on one machine without a VPS using `--direct-url` (documented there).

## Pull requests

1. Fork, create a branch, keep the change focused.
2. `cargo fmt`, `cargo clippy` and `cargo test` should pass; CI runs a build and tests on Windows, macOS and Linux.
3. Add or update tests when you touch `crates/agent/src/safety.rs` or anything that validates input.
4. Update the docs and `CHANGELOG.md` for user-visible changes.
5. Describe what you tested and on which OS. UI changes: attach a screenshot (blur your host/IP).

## Security-sensitive areas

The agent runs commands on a server. Changes to command validation, archive extraction, authentication, the installer script or the systemd unit get extra review. Never log or print the token. Please read [docs/security.md](docs/security.md) first, and report vulnerabilities privately as described in [SECURITY.md](SECURITY.md).

## Style

Match the surrounding code. Comments explain *why*, not *what*. No new heavy dependencies without a reason (the client deliberately has no TLS stack; the agent builds as a static musl binary with no C code).

## License

By contributing you agree that your work is licensed under the project's dual MIT / Apache-2.0 license ([LICENSE](LICENSE)).
