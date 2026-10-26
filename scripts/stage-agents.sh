#!/usr/bin/env bash
# Builds the static Linux agents (x86_64 + aarch64) and stages them in dist/agents for packaging.
set -euo pipefail
cd "$(dirname "$0")/.."
for t in x86_64-unknown-linux-musl aarch64-unknown-linux-musl; do
  rustup target add "$t"
  cargo build -p hfr-agent --release --target "$t"
  mkdir -p "dist/agents/$t"
  cp "target/$t/release/hfr-agent" "dist/agents/$t/hfr-agent"
done
echo "staged:"; ls -l dist/agents/*/hfr-agent
