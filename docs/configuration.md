# Configuration

## On your computer

### Connection config

Created by `hfr setup`, `hfr init` or the app's first-run screen.

| OS | Path |
| --- | --- |
| Windows | `%APPDATA%\hfr\config.toml` |
| macOS | `~/Library/Application Support/hfr/config.toml` |
| Linux | `~/.config/hfr/config.toml` |

```toml
host = "203.0.113.10"
user = "root"
# key = "C:\Users\me\.ssh\id_ed25519"   # optional; default is whatever ssh-agent offers
ssh_port = 22
token = ""            # empty when the token is in the OS credential store
remote_port = 7878    # agent port on the VPS (loopback)
local_port = 17878    # local end of the SSH tunnel
# direct_url = "http://127.0.0.1:7878"      # skip the tunnel (testing only)
```

### Where the token lives

| OS | Storage |
| --- | --- |
| Windows | Credential Manager (service `hfr`, user `agent-token`) |
| macOS | Keychain (same names) |
| Linux | In `config.toml`, file mode `0600` |

An old config holding a plaintext token is migrated into the credential store the next time it is loaded (Windows/macOS).

To forget everything locally: delete `config.toml` and remove the `hfr` / `agent-token` credential.

### Project upload rules

Files skipped: `node_modules/`, `.git/`, and anything matched by a `.hfrignore` in the project root (gitignore syntax). Symlinks are not uploaded.

## On the VPS

`hfr setup` creates:

| Path | Purpose |
| --- | --- |
| `/usr/local/bin/hfr-agent` | The agent binary |
| `/etc/systemd/system/hfr-agent.service` | Service unit (runs as user `hfr`, hardened) |
| `/etc/hfr/agent.env` | `HFR_TOKEN=...`, mode `0640`, owner `root:hfr` |
| `/var/lib/hfr/jobs/<id>/` | One folder per job: `project/`, `log.txt`, `meta.json` (saved job state; lets the agent restore its job list after a restart) |
| `/var/lib/hfr/.cache/hyperframes/` | Chrome download used for rendering |

Service management: `sudo systemctl status hfr-agent`, `sudo journalctl -u hfr-agent -f`, `sudo systemctl restart hfr-agent`.

### Agent environment variables

| Variable | Default | Meaning |
| --- | --- | --- |
| `HFR_TOKEN` | required | Bearer token clients must send |
| `HFR_ADDR` | `127.0.0.1:7878` | Listen address. Keep it on loopback |
| `HFR_ROOT` | `./hfr-data` (`/var/lib/hfr/jobs` under systemd) | Where job folders live |
| `HFR_ALLOW_ANY_COMMAND` | unset | `1` disables the command allowlist. Development only |

To change one under systemd: `sudo systemctl edit hfr-agent` and add `[Service]` / `Environment=...`.

### Client environment variable

`HFR_CONFIG=<path>` makes `hfr` and the app read and write that config file instead of the default one. A config that sets `direct_url` never touches the OS credential store, so a throw-away test setup cannot overwrite your real token. Intended for development and tests.

### Rotating the token

```bash
ssh you@vps "sudo sed -i \"s/^HFR_TOKEN=.*/HFR_TOKEN=$(openssl rand -hex 32)/\" /etc/hfr/agent.env && sudo systemctl restart hfr-agent"
```
then `hfr init --host ... --user ... --token <new>` locally (or delete the config and re-run setup after removing `/etc/hfr/agent.env`).
