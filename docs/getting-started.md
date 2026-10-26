# Getting started

## What you need

| On your computer | On the VPS |
| --- | --- |
| Windows 10+, macOS or Linux | Debian or Ubuntu (x86_64 or ARM64) |
| OpenSSH client (`ssh`, `scp`) | SSH access as `root`, or a user with **passwordless sudo** |
| An SSH key the VPS accepts | About 2 GB free for tools, plus space for your renders |

The installer needs `apt`. Other distributions are not supported yet.

## 1. Install HFR

Download an installer from the [releases page](https://github.com/SairamVemula/hfr/releases) (Windows x64 and ARM64 `.exe`, Linux x64 and ARM64 `.AppImage` / `.deb`; there is no macOS installer in releases yet, build from source with `cargo build --release -p hfr-gui` and `cargo packager ... --formats dmg`). It installs the desktop app (`hfr-gui`) and the command line (`hfr`) together, and bundles the Linux agent that gets uploaded to your VPS. On Windows the CLI is `hfr.exe` in the install folder; add that folder to your `PATH` to run `hfr` from any terminal.

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

Or open **HFR**: the first-run screen asks for the same three things (host, user, optional key file).

What it does, step by step: detects the VPS CPU type, uploads the matching static agent, installs ffmpeg + Chrome libraries + Node 22 + `hyperframes`, creates an unprivileged `hfr` user and a hardened systemd service, generates a random token, saves the connection locally, and checks the tunnel. It is safe to re-run: every step first checks what is already there and only installs what is missing or outdated, so a second run changes nothing and takes seconds. It keeps your token and jobs, and it never interrupts a running render: upgrades that could do so are postponed until the VPS is idle (`hfr setup --force` reinstalls everything).

## 5. Render something

```bash
hfr render ./my-project          # creates the project, uploads, starts, follows logs
hfr render ./my-project --quality delivery --fps 60   # again with other options: uploads only what changed
```

The project folder is the one with `index.html` / `hyperframes.json`. `node_modules` and `.git` are skipped automatically; add a `.hfrignore` file (gitignore syntax) to skip more, such as old `renders/`.

In the app, click **New render**, pick the folder and choose the options. The folder becomes a *project* on the VPS; it keeps all its renders, and the next render of the same folder uploads only what you changed.

## 6. Get the video and free the space

```bash
hfr projects                      # your projects and their renders
hfr files <job>                   # the videos a render wrote
hfr pull <job> --delete           # download them (resumable), verify SHA-256, then delete on the VPS
```

In the app, open the project, select the render and press **Download** next to a file (or **Download all**). The Transfers bar shows percentage, speed and time left, with Pause and Resume. Files go to `Downloads/HFR/<project>` (CLI: current folder, or `-o <dir>`); an existing file is never overwritten. Keep **Delete a file from the VPS after it was downloaded and verified** ticked in Settings to free the space in the same step.

Nothing is deleted unless the download verified. Deleting without downloading always asks first: **Delete** next to a file, a render or a project in the app (the dialog says what goes), or `hfr rm file|job|project`.

## Where to go next

- [CLI reference](cli.md)
- [Troubleshooting](troubleshooting.md)
- [Security](security.md) - read this before putting anything sensitive through it
