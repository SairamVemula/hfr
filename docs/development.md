# Development

## Prerequisites

- Rust stable (1.80+; developed on 1.99) with `rustup`.
- OpenSSH (`ssh`, `scp`) on your PATH.
- GUI: a GPU driver with Vulkan / DirectX 12 / Metal. On Linux also: `libxkbcommon-x11-dev libwayland-dev libx11-xcb-dev libxcb-xkb-dev libfontconfig-dev libvulkan-dev libasound2-dev pkg-config`.
- Windows: the MSVC build tools and Windows SDK (needed for the icon resource compiler too).

## Build and test

```bash
cargo build                         # everything (debug)
cargo test --workspace --exclude hfr-gui   # unit tests and the agent integration test (crates/agent/tests/e2e.rs)
cargo test -p hfr-agent             # agent safety tests (command allowlist, tar extraction, token check)
bash scripts/e2e-agent.sh           # Linux/WSL: ~57 API checks against the musl agent with a fake npx
cargo build -p hfr-cli              # just the CLI -> target/debug/hfr
cargo build -p hfr-gui              # just the app -> target/debug/hfr-gui
```

The agent is Linux-targeted but also builds and runs on Windows/macOS for local testing.

### Run everything locally without a VPS

```bash
export HFR_TOKEN=dev HFR_ROOT=/tmp/hfr-data HFR_ALLOW_ANY_COMMAND=1
cargo run -p hfr-agent &                       # listens on 127.0.0.1:7878
cargo run -p hfr-cli -- init --direct-url http://127.0.0.1:7878 --token dev
cargo run -p hfr-cli -- render ./some-project -- echo hello      # any command works because of ALLOW_ANY
```

`--direct-url` skips the SSH tunnel. On Windows, set `MSYS_NO_PATHCONV=1` in Git Bash so `cmd /c` is not rewritten.

## Cross-compiling the agent

The agent is pure Rust, so static Linux binaries build from any OS with the bundled `rust-lld` linker (see `.cargo/config.toml`), no C toolchain or Docker:

```bash
bash scripts/stage-agents.sh        # x86_64 + aarch64 musl -> dist/agents/<triple>/hfr-agent
```

Type-check the other crates for other platforms without linking:

```bash
rustup target add aarch64-apple-darwin x86_64-unknown-linux-gnu
cargo check -p hfr-proto -p hfr-core -p hfr-cli -p hfr-agent --target aarch64-apple-darwin
```

The client avoids C dependencies (no TLS stack: it only talks plain HTTP through the SSH tunnel) to keep this easy. Please do not add one without a good reason.

## Packaging installers

Uses [cargo-packager](https://github.com/crabnebula-dev/cargo-packager) and `Packager.toml`:

```bash
cargo install cargo-packager --locked
bash scripts/stage-agents.sh
cargo build --release -p hfr-cli -p hfr-gui
cargo packager --release -c Packager.toml --formats nsis            # Windows installer
cargo packager --release -c Packager.toml --formats dmg             # macOS
cargo packager --release -c Packager.toml --formats appimage,deb    # Linux
```

Output lands in `dist/installers/`. Installers include both binaries and the two Linux agents (`agents/<triple>/hfr-agent`, found by `hfr setup`).

### Releasing

Tag and push: `git tag v0.1.0 && git push --tags`. `.github/workflows/release.yml` builds installers on Windows, macOS and Linux and attaches them to a GitHub release. `.github/workflows/ci.yml` builds and tests on all three OSes for every push.

## Releasing and updates

Installed apps look for a newer release on GitHub (`hfr-core/src/update.rs`), download the installer and run it only after two checks: the release's `SHA256SUMS` must carry a valid ed25519 signature (`SHA256SUMS.sig`) from a key built into the app (`PUBLIC_KEYS`), and the installer's SHA-256 must be listed in it. Windows installs silently and restarts the app; macOS and Linux open the verified file.

**Signing key (one time).** `cargo run -p hfr-core --example release_key -- keygen <file>` writes the private key to `<file>` and prints the public key. Put the public key into `PUBLIC_KEYS` (several are allowed, which is how a key is rotated: ship the new public key in release N, sign release N+1 with the new private key), and the file's content into the repository secret `HFR_UPDATE_KEY`. Back the private key up somewhere safe; if it is lost, users must install one release by hand.

**Release checklist**

1. Bump `version` in `Cargo.toml` (workspace) and `Packager.toml`; add the CHANGELOG entry (the app shows the release notes).
2. If the agent protocol changed incompatibly, bump `API_VERSION` and `MIN_API` in `crates/proto/src/lib.rs` (and the marker follows automatically).
3. `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`.
4. Commit, then tag `vX.Y.Z` and push the tag (only when you decide to release).
5. The workflow checks tag == versions, rebuilds and checks the bundled agents, builds the installers into a **draft** release, then `sign-and-publish` writes and signs `SHA256SUMS` and publishes. Apps never see an unsigned release.
6. On a machine with the previous version: Settings > Check now, update, confirm the new version and that the agent notice (if any) behaves.

**Testing updates locally** (debug builds only): serve a folder with a `latest` JSON (GitHub's `releases/latest` shape), an installer, `SHA256SUMS` and `SHA256SUMS.sig`; run with `HFR_UPDATE_URL=http://127.0.0.1:8765/latest`, `HFR_UPDATE_PUBKEY=<test public key>` and `HFR_UPDATE_NO_INSTALL=1`, and use a throw-away `HFR_CONFIG`.

## Branding assets

`assets/branding/icon.svg` and `mark.svg` are the source. Regenerate the PNG/ICO/ICNS set with any SVG rasteriser; the committed files were produced with `@resvg/resvg-js` and `png2icons`. The colour palette lives in `crates/gui/src/theme.rs`.

## Code map

```
crates/proto/src/lib.rs        wire types (Project, Job, OutputFile), sync plan, API_VERSION
crates/proto/src/command.rs    command allowlist (shared: agent enforces, clients are tested against it)
crates/agent/src/main.rs       routes, auth, overview
crates/agent/src/store.rs      projects and jobs on disk, persistence, migration of the old layout
crates/agent/src/projects.rs   create/delete, manifest, sync plan, sync upload
crates/agent/src/jobs.rs       job runner, cancel, logs
crates/agent/src/files.rs      output files: list, ranged download, delete
crates/agent/src/safety.rs     tar extraction, path and token checks (+ tests)
crates/agent/tests/e2e.rs      integration test: real agent driven through hfr-core
crates/core/src/config.rs      Config, credential store
crates/core/src/client.rs      Tunnel, Client (every API call)
crates/core/src/sync.rs        scan, hash cache, delta upload
crates/core/src/registry.rs    local folder -> project
crates/core/src/download.rs    resumable verified downloads
crates/core/src/render.rs      RenderOptions <-> command (shared by CLI and app)
crates/core/src/transfer.rs    progress, cancel, speed/ETA helpers
crates/core/src/setup.rs       VPS installation (uses install.sh)
crates/core/src/install.sh     script run on the VPS (idempotent, bash, Debian/Ubuntu)
crates/cli/src/main.rs         clap commands
crates/gui/src/main.rs         app shell, state, polling
crates/gui/src/projects_view.rs project list and project page
crates/gui/src/dialogs.rs      render options, confirmations, new project
crates/gui/src/transfers.rs    uploads, downloads, transfer bar and page
crates/gui/src/pages.rs        VPS and settings pages
crates/gui/src/theme.rs        palette and gpui-kit theme overrides
scripts/e2e-agent.sh           API checks against a Linux agent binary (fake npx)
```

## Conventions

- Keep the agent API additive; clients and agents of different versions should keep working where possible.
- New agent endpoints must go behind the auth middleware and validate any project or job id against the store, and any `?path=` against the recorded files or `safety.rs` rules, before building paths. Bump `API_VERSION` for breaking changes.
- Anything that executes input needs an allowlist and a test (`crates/proto/src/command.rs` for commands, `crates/agent/src/safety.rs` for archives and tokens).
- No network calls other than to the user's own VPS, the installer's package downloads on the VPS, and the update check (`api.github.com` / `github.com`, via the system `curl`, sending only a `User-Agent: hfr/<version>`). Keep it that way and keep it switchable (`check_updates` / Settings).
- Match the surrounding style; run `cargo fmt` and `cargo clippy` before sending a PR.

## Gotchas learned the hard way

- A command inside a script piped to `bash -s` can read the rest of the script from stdin. The installer is run as `bash -c "$(cat)"` for that reason.
- Git Bash's `ssh` cannot talk to the Windows ssh-agent; test from PowerShell.
- Tools that edit files while `cargo build` runs can make the build see half-applied changes; wait for builds to finish.
- gpui-kit: `use gpui_kit::*` gives GPUI itself; components are under `gpui_kit::component::*`; icons come from `gpui_kit::assets::IconName` and only the icons in the bundled default set render.
