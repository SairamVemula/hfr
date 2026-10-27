#!/usr/bin/env bash
# Prints the release notes of one version: its CHANGELOG.md section plus a short download guide.
#   bash scripts/release-notes.sh 0.1.6 > notes.md
# The release workflow uses it for the GitHub release page. It fails when the version has no changelog entry, so a
# release can never go out with an empty description.
set -euo pipefail
v="${1:?usage: release-notes.sh <version, e.g. 0.1.6>}"
cd "$(dirname "$0")/.."

section=$(awk -v h="## [$v]" 'index($0, h) == 1 { p = 1; next } /^## \[/ { p = 0 } p' CHANGELOG.md)
if [ -z "$(printf '%s' "$section" | tr -d '[:space:]')" ]; then
  echo "CHANGELOG.md has no entry for version $v: add a '## [$v] - <date>' section before releasing" >&2
  exit 1
fi

printf "## What's new in %s\n%s\n\n" "$v" "$section"
cat <<EOF
---

### Download

| Your computer | File |
| --- | --- |
| Windows (64-bit) | \`hfr-gui_${v}_x64-setup.exe\` |
| Windows on ARM | \`hfr-gui_${v}_arm64-setup.exe\` |
| macOS (Apple Silicon) | \`HFR_${v}_aarch64.dmg\` |
| macOS (Intel) | \`HFR_${v}_x64.dmg\` |
| Linux (64-bit) | \`hfr-gui_${v}_x86_64.AppImage\` or \`hfr-gui_${v}_amd64.deb\` |
| Linux on ARM | \`hfr-gui_${v}_aarch64.AppImage\` or \`hfr-gui_${v}_arm64.deb\` |

### Updating

- **Already installed?** HFR offers the update itself: Settings > About and updates > Check now. Updates are checked against \`SHA256SUMS\`, which is signed (\`SHA256SUMS.sig\`).
- **After updating the app**, press **Upgrade agent** in Settings so the agent on your VPS gets the same version (it waits while a render is running).
- Windows may show a SmartScreen warning because the installer is not code-signed yet: choose **More info > Run anyway**.
EOF
