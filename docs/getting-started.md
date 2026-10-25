# Getting started

## What you need

| On your computer | On the VPS |
| --- | --- |
| Windows 10+, macOS or Linux | Debian or Ubuntu (x86_64 or ARM64) |
| OpenSSH client (`ssh`, `scp`) | SSH access as `root`, or a user with **passwordless sudo** |
| An SSH key the VPS accepts | About 2 GB free for tools, plus space for your renders |

The installer needs `apt`. Other distributions are not supported yet.

## 1. Install HFR Remote

Download an installer from the [releases page](https://github.com/SairamVemula/hfr/releases) (Windows `.exe`, macOS `.dmg`, Linux `.AppImage` / `.deb`). It installs the desktop app (`hfr-gui`) and the command line (`hfr`) together, and bundles the Linux agent that gets uploaded to your VPS. On Windows the CLI is `hfr.exe` in the install folder; add that folder to your `PATH` to run `hfr` from any terminal.

Or build from source: see [development.md](development.md).

## 2. Let HFR use your SSH key

HFR never asks for or stores your SSH password or key passphrase. It runs your system `ssh` in non-interactive mode, so the key must already be available through **ssh-agent**.

**Windows** (PowerShell as Administrator, once):

```powershell
Set-Service ssh-agent -StartupType Manual
Start-Service ssh-agent
```

Then in any terminal (asks for the passphrase once):

```powershell
ssh-add $env:USERPROFILE\.ssh\id_ed25519
```

**macOS:** `ssh-add --apple-use-keychain ~/.ssh/id_ed25519`
**Linux:** `ssh-add ~/.ssh/id_ed25519` (start an agent first with `eval "$(ssh-agent -s)"` if needed).

Check it worked: `ssh-add -l` lists the key, and `ssh user@your-vps echo ok` prints `ok` without prompting.

> If you use Git for Windows, its bundled `ssh` is a different program that cannot see the Windows ssh-agent. Run `hfr` from PowerShell or the desktop app.

## 3. Allow passwordless sudo (non-root users only)

The installer needs root. If you log in as a normal user, allow it temporarily:

```bash
ssh -t you@your-vps "echo 'you ALL=(ALL) NOPASSWD:ALL' | sudo tee /etc/sudoers.d/you && sudo chmod 440 /etc/sudoers.d/you"
```

You can remove it after setup (`sudo rm /etc/sudoers.d/you`). Re-running setup later (to upgrade the agent) needs it again.

## 4. Install the agent on the VPS

Command line:

```bash
hfr setup --host 203.0.113.10 --user you
```

Or open **HFR Remote**: the first-run screen asks for the same three things (host, user, optional key file).

What it does, step by step: detects the VPS CPU type, uploads the matching static agent, installs ffmpeg + Chrome libraries + Node 22 + `hyperframes`, creates an unprivileged `hfr` user and a hardened systemd service, generates a random token, saves the connection locally, and checks the tunnel. It is safe to re-run (it keeps your token and jobs).

## 5. Render something

```bash
hfr render ./my-project          # packs, uploads, starts, follows logs
```

The project folder is the one with `index.html` / `hyperframes.json`. `node_modules` and `.git` are skipped automatically; add a `.hfrignore` file (gitignore syntax) to skip more, such as old `renders/`.

In the app, click **New render** and pick the folder.

## 6. Get the video and free the space

```bash
hfr jobs                          # find the job id and state
hfr pull <job-id> --delete        # download, verify SHA-256, then delete on the VPS
```

In the app, select the job, keep **Free VPS space after download** ticked and press **Download video**. Files go to your Downloads folder (CLI: current folder, or `-o <dir>`).

Nothing is deleted unless the download verified. Deleting without downloading needs an explicit second click ("Really delete?") in the app, or `hfr rm <job-id>`.

## Where to go next

- [CLI reference](cli.md)
- [Troubleshooting](troubleshooting.md)
- [Security](security.md) - read this before putting anything sensitive through it
