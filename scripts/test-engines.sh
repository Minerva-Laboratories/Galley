#!/usr/bin/env bash
# Explicit developer/CI setup; Galley itself never installs TeX Live.
# TeX Live, latexmk, Biber, Ghostscript, latexdiff and DejaVu fonts must already be installed.
set -euo pipefail
cd "$(dirname "$0")/.."

ENGINE_TEST_ROOT="${GALLEY_ENGINE_TEST_ROOT:-$(mktemp -d)}"
if [[ -z "${GALLEY_TEST_TECTONIC_ENGINES:-}" ]]; then
  # The pinned engine, laid out exactly as in the image. GALLEY_TEST_BUNDLE_TAR is a local copy of
  # the pinned bundle tar, which skips its 2.9 GB download; its hash is checked all the same.
  export GALLEY_TEST_TECTONIC_ENGINES="$ENGINE_TEST_ROOT/tectonic"
  bash deploy/tectonic-engine.sh "$GALLEY_TEST_TECTONIC_ENGINES" "${GALLEY_TEST_BUNDLE_TAR:-}"
fi
export GALLEY_TEST_TEXLIVE_PATH="${GALLEY_TEST_TEXLIVE_PATH:-$(dirname "$(command -v latexmk)")}"
export GALLEY_TEST_ENGINE_MATRIX=1
export GALLEY_TEST_SANDBOX=bwrap
cargo test -p galley-build --test engine_matrix --test engine_workflows --locked -- --nocapture
cargo test -p galley-server --test sync real_mcp_engine_matrix_preserves_project_selection --locked -- --nocapture
