#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")"

echo "=== cargo test ==="
cargo test

echo ""
echo "=== cargo check (warnings) ==="
cargo check 2>&1 | grep -c "warning:" || true
