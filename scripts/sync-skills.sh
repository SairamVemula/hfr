#!/usr/bin/env bash
# Mirrors skills/ into .claude/skills/ so Claude Code picks them up in this repo. `--check` fails if they differ (used in CI).
set -euo pipefail
cd "$(dirname "$0")/.."
if [ "${1:-}" = "--check" ]; then
  diff -r skills/hfr-remote-render .claude/skills/hfr-remote-render
  diff -r skills/hfr-remote-develop .claude/skills/hfr-remote-develop
  echo "skills in sync"; exit 0
fi
mkdir -p .claude/skills
for s in hfr-remote-render hfr-remote-develop; do
  rm -rf ".claude/skills/$s"
  cp -r "skills/$s" ".claude/skills/$s"
done
echo "synced skills -> .claude/skills"
