#!/usr/bin/env bash
# Playwright flow: create a project, edit it in two tabs, build it later, then
# let the auto-commit land in git.
# Needs Node 20+ and `npx playwright install chromium` once.
set -euo pipefail
cd "$(dirname "$0")/.."

PORT="${GALLEY_E2E_PORT:-7411}"
DATA="$(mktemp -d)"
trap 'kill "${SERVER_PID:-}" 2>/dev/null || true; rm -rf "$DATA"' EXIT

(cd web && npm run build)
cargo build -p galley-cli
target/debug/galley serve --data-dir "$DATA" --bind "127.0.0.1:$PORT" &
SERVER_PID=$!

for _ in $(seq 1 50); do
  if curl -fsS "http://127.0.0.1:$PORT/api/health" >/dev/null 2>&1; then break; fi
  sleep 0.2
done

cd web
GALLEY_E2E_URL="http://127.0.0.1:$PORT" npx playwright test "$@"
