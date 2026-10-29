#!/usr/bin/env bash
# Playwright flow: create a project, edit it in two tabs, build it later, then
# let the auto-commit land in git.
# Needs Node 20+ and `npx playwright install chromium` once.
set -euo pipefail
cd "$(dirname "$0")/.."

PORT="${GALLEY_E2E_PORT:-7411}"
DATA="$(mktemp -d)"
trap 'kill "${SERVER_PID:-}" 2>/dev/null || true; rm -rf "$DATA"' EXIT

# Reuse an installed Tectonic engine (see scripts/test-engines.sh) for offline browser builds. Only
# the engine folder is linked, so nothing Galley tidies at startup reaches the fixture.
if [[ -n "${GALLEY_TEST_TECTONIC_ENGINES:-}" ]]; then
  mkdir -p "$DATA/tectonic"
  for engine in "$GALLEY_TEST_TECTONIC_ENGINES"/engine-*; do
    ln -s "$engine" "$DATA/tectonic/"
  done
fi
E2E_ARGS=(--data-dir "$DATA")
if [[ -n "${GALLEY_E2E_CONFIG:-}" ]]; then
  E2E_ARGS+=(--config "$GALLEY_E2E_CONFIG")
else
  # Separate real users exercise invitations and presence. This config exists only in the
  # disposable fixture directory and never changes the deployment's signup policy. The browser
  # tests sign up and sign in far more often than the rate limits allow one client, so they are
  # off here; a custom GALLEY_E2E_CONFIG must also set `[limits] enabled = false`.
  cat > "$DATA/e2e.toml" <<'CONFIG'
[server]
public_signup = true

[limits]
enabled = false
CONFIG
  E2E_ARGS+=(--config "$DATA/e2e.toml")
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
