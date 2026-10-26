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
local_port = 17878    # only a fallback: each connection picks a free local port for its tunnel
# direct_url = "http://127.0.0.1:7878"      # skip the tunnel (testing only)
# download_dir = 'D:\Renders'                # default: Downloads/HFR
free_after_download = true   # delete a file from the VPS after it was downloaded and verified
```

### Where the token lives

| OS | Storage |
| --- | --- |
| Windows | Credential Manager (service `hfr`, user `agent-token`) |
| macOS | Keychain (same names) |
| Linux | In `config.toml`, file mode `0600` |

An old config holding a plaintext token is migrated into the credential store the next time it is loaded (Windows/macOS).

To forget everything locally: delete `config.toml` and remove the `hfr` / `agent-token` credential.

### Remembered folders

Next to `config.toml`, in the same folder:

| File | Purpose |
| --- | --- |
| `projects.json` | Which local folder (canonical path) is which project on the VPS, and when it was last synced. This is how `hfr render ./intro` and the app know to sync into the existing project instead of creating a second one. Delete an entry (or the file) to make the folder look new |
| `hashcache.json` | SHA-256 of local files keyed by path, size and modified time, so syncing a large folder does not re-read every file. Safe to delete |

Both are shared by the CLI and the app. A folder is identified by its path, so two folders with the same name are two projects, and a moved folder looks new.

### Downloads

Files are saved to `<download folder>/<project name>/<file>`; the download folder defaults to `Downloads/HFR` and can be changed in the app's Settings or with `download_dir`. An unfinished download is `<file>.hfrpart` plus `<file>.hfrpart.json` next to it; run the download again to resume, or delete both files to discard it. Existing files are never overwritten.

### Project upload rules

Files skipped: `node_modules/`, `.git/`, and anything matched by a `.hfrignore` in the project root (gitignore syntax). Symlinks are not uploaded. Only files that are new or changed since the last sync are sent; files you deleted locally are deleted from the project on the VPS at the next sync (render outputs are never removed by a sync).

## On the VPS

`hfr setup` creates:

| Path | Purpose |
| --- | --- |
| `/usr/local/bin/hfr-agent` | The agent binary |
| `/etc/systemd/system/hfr-agent.service` | Service unit (runs as user `hfr`, hardened) |
| `/etc/hfr/agent.env` | `HFR_TOKEN=...`, mode `0640`, owner `root:hfr` |
| `/var/lib/hfr/jobs/<project-id>/` | One folder per project: `project.json` (name and manifest of synced files), `project/` (the files and the renders' outputs) and `jobs/<job-id>/` with `job.json` and `log.txt` for each render. The saved state lets the agent restore everything after a restart |
| `/var/lib/hfr/.cache/hyperframes/` | Chrome download used for rendering |

Service management: `sudo systemctl status hfr-agent`, `sudo journalctl -u hfr-agent -f`, `sudo systemctl restart hfr-agent`.

### Agent environment variables

| Variable | Default | Meaning |
| --- | --- | --- |
| `HFR_TOKEN` | required | Bearer token clients must send |
| `HFR_ADDR` | `127.0.0.1:7878` | Listen address. Keep it on loopback |
| `HFR_ROOT` | `./hfr-data` (`/var/lib/hfr/jobs` under systemd) | Where project folders live |
| `HFR_ALLOW_ANY_COMMAND` | unset | `1` disables the command allowlist. Development only |

To change one under systemd: `sudo systemctl edit hfr-agent` and add `[Service]` / `Environment=...`.

### Update check

`update.json` next to the config file remembers the last check, the newer version found, whether automatic checks are on and which version you skipped. Automatic checks run at start-up and at most once an hour; turn them off in Settings. Debug builds also read `HFR_UPDATE_URL`, `HFR_UPDATE_PUBKEY` and `HFR_UPDATE_NO_INSTALL` for testing ([development.md](development.md#releasing-and-updates)).

### Client environment variable

`HFR_CONFIG=<path>` makes `hfr` and the app read and write that config file instead of the default one. A config that sets `direct_url` never touches the OS credential store, so a throw-away test setup cannot overwrite your real token. Intended for development and tests.

### Rotating the token

```bash
ssh you@vps "sudo sed -i \"s/^HFR_TOKEN=.*/HFR_TOKEN=$(openssl rand -hex 32)/\" /etc/hfr/agent.env && sudo systemctl restart hfr-agent"
```
then `hfr init --host ... --user ... --token <new>` locally (or delete the config and re-run setup after removing `/etc/hfr/agent.env`).
