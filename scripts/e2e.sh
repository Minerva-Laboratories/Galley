#!/usr/bin/env bash
# Playwright flow: create a project, edit it in two tabs, build it later, then
# let the auto-commit land in git.
# Needs Node 20+ and `npx playwright install chromium` once.
set -euo pipefail
cd "$(dirname "$0")/.."

PORT="${GALLEY_E2E_PORT:-7411}"
DATA="$(mktemp -d)"
trap 'kill "${SERVER_PID:-}" 2>/dev/null || true; rm -rf "$DATA"' EXIT

# Reuse explicitly prepared engine fixtures for offline, reproducible browser builds.
if [[ -n "${GALLEY_TEST_TECTONIC:-}" || -n "${GALLEY_TEST_TECTONIC_CACHE:-}" ]]; then
  mkdir -p "$DATA/tectonic"
  if [[ -n "${GALLEY_TEST_TECTONIC:-}" ]]; then
    ln -s "$GALLEY_TEST_TECTONIC" "$DATA/tectonic/tectonic"
  fi
  if [[ -n "${GALLEY_TEST_TECTONIC_CACHE:-}" ]]; then
    ln -s "$GALLEY_TEST_TECTONIC_CACHE" "$DATA/tectonic/cache"
  fi
fi
E2E_ARGS=(--data-dir "$DATA")
if [[ -n "${GALLEY_E2E_CONFIG:-}" ]]; then
  E2E_ARGS+=(--config "$GALLEY_E2E_CONFIG")
fi

(cd web && npm run build)
cargo build -p galley-cli
target/debug/galley "${E2E_ARGS[@]}" serve --bind "127.0.0.1:$PORT" &
SERVER_PID=$!

for _ in $(seq 1 50); do
  if curl -fsS "http://127.0.0.1:$PORT/api/health" >/dev/null 2>&1; then break; fi
  sleep 0.2
done

cd web
GALLEY_E2E_URL="http://127.0.0.1:$PORT" npx playwright test "$@"
