# Changelog

All notable changes are listed here. The format follows [Keep a Changelog](https://keepachangelog.com/) and the project aims to follow [Semantic Versioning](https://semver.org/) once it reaches 1.0.

## [Unreleased]

### Added
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
