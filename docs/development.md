# Development

## Prerequisites

- Rust stable (1.80+; developed on 1.99) with `rustup`.
- OpenSSH (`ssh`, `scp`) on your PATH.
- GUI: a GPU driver with Vulkan / DirectX 12 / Metal. On Linux also: `libxkbcommon-x11-dev libwayland-dev libx11-xcb-dev libxcb-xkb-dev libfontconfig-dev libvulkan-dev libasound2-dev pkg-config`.
- Windows: the MSVC build tools and Windows SDK (needed for the icon resource compiler too).

## Build and test

```bash
cargo build                         # everything (debug)
cargo test -p hfr-agent             # agent safety tests (command allowlist, tar extraction, token check)
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

## Branding assets

`assets/branding/icon.svg` and `mark.svg` are the source. Regenerate the PNG/ICO/ICNS set with any SVG rasteriser; the committed files were produced with `@resvg/resvg-js` and `png2icons`. The colour palette lives in `crates/gui/src/theme.rs`.

## Code map

```
crates/proto/src/lib.rs        wire types, output extensions
crates/proto/src/command.rs    command allowlist (shared: agent enforces, clients are tested against it)
crates/agent/src/main.rs       HTTP routes, job runner, metrics
crates/agent/src/safety.rs     tar extraction, token check (+ tests)
crates/core/src/lib.rs         Config, credential store, Tunnel, Client, pack + tracked upload
crates/core/src/render.rs      RenderOptions -> command (shared by CLI and app)
crates/core/src/transfer.rs    upload progress, cancel, speed/ETA helpers
crates/core/src/setup.rs       VPS installation (uses install.sh)
crates/core/src/install.sh     script run on the VPS (idempotent, bash, Debian/Ubuntu)
crates/cli/src/main.rs         clap commands
crates/gui/src/main.rs         app state, actions, main views
crates/gui/src/render_flow.rs  new-render flow: options dialog, upload panel
crates/gui/src/theme.rs        palette and gpui-kit theme overrides
```

## Conventions

- Keep the agent API additive; clients and agents of different versions should keep working where possible.
- New agent endpoints must go behind the auth middleware and validate any id against the job table before building paths.
- Anything that executes input needs an allowlist and a test (`crates/proto/src/command.rs` for commands, `crates/agent/src/safety.rs` for archives and tokens).
- No network calls other than to the user's own VPS (and the installer's package downloads).
- Match the surrounding style; run `cargo fmt` and `cargo clippy` before sending a PR.

## Gotchas learned the hard way

- A command inside a script piped to `bash -s` can read the rest of the script from stdin. The installer is run as `bash -c "$(cat)"` for that reason.
- Git Bash's `ssh` cannot talk to the Windows ssh-agent; test from PowerShell.
- Tools that edit files while `cargo build` runs can make the build see half-applied changes; wait for builds to finish.
- gpui-kit: `use gpui_kit::*` gives GPUI itself; components are under `gpui_kit::component::*`; icons come from `gpui_kit::assets::IconName` and only the icons in the bundled default set render.
