#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")"

cargo install --path .

echo "Deployed: $(code-index --version 2>/dev/null || echo 'ok')"
