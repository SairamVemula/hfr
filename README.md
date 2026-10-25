# hyperframes-remote

Render big Hyperframes projects on your own VPS instead of your PC: push the project, watch logs and VPS load, pull the video, delete the job.

| Part | What | Runs on |
| --- | --- | --- |
| `hfr` (crates/cli) | CLI | Windows, macOS, Linux |
| `hfr-gui` (crates/gui) | GPUI desktop app | Windows, macOS, Linux |
| `hfr-agent` (crates/agent) | Service on the VPS | Linux x86_64 / aarch64 (static musl) |

Requirements on your computer: `ssh` and `scp` (OpenSSH) and your key loaded in ssh-agent (`ssh-add`).
VPS: Debian/Ubuntu with SSH access and root or passwordless sudo.

```
cargo build -p hfr-agent --release --target x86_64-unknown-linux-musl   # or aarch64-unknown-linux-musl
cargo build -p hfr-cli --release
hfr setup --host <ip> --user <user>        # one-time install on the VPS
hfr render ./my-project                    # push + render + follow logs
hfr pull <job-id> --delete                 # download (sha256-verified) and free the VPS space
hfr-gui                                    # same thing with live dashboards
```

Security: the agent listens on 127.0.0.1 only; the client reaches it through an SSH tunnel and a bearer token
(kept in Windows Credential Manager / macOS Keychain, or a 0600 config file on Linux). Jobs may only run
`npx hyperframes render` with an allowlist of options.
