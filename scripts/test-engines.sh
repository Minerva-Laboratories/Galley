#!/usr/bin/env bash
# Explicit developer/CI setup; Galley itself never installs TeX Live.
# TeX Live, latexmk, Biber, Ghostscript, latexdiff and DejaVu fonts must already be installed.
set -euo pipefail
cd "$(dirname "$0")/.."

ENGINE_TEST_ROOT="${GALLEY_ENGINE_TEST_ROOT:-$(mktemp -d)}"
mkdir -p "$ENGINE_TEST_ROOT/tectonic" "$ENGINE_TEST_ROOT/cache" "$ENGINE_TEST_ROOT/warmup"
if [[ -z "${GALLEY_TEST_TECTONIC:-}" ]]; then
  case "$(uname -m)" in
    x86_64) ENGINE_TEST_ARCH=x86_64 ;;
    aarch64) ENGINE_TEST_ARCH=aarch64 ;;
    *) echo "Set GALLEY_TEST_TECTONIC for this architecture" >&2; exit 1 ;;
  esac
  curl --fail --location --retry 2 \
    "https://github.com/tectonic-typesetting/tectonic/releases/download/tectonic%400.17.0/tectonic-0.17.0-${ENGINE_TEST_ARCH}-unknown-linux-musl.tar.gz" \
    -o "$ENGINE_TEST_ROOT/tectonic.tar.gz"
  tar -xzf "$ENGINE_TEST_ROOT/tectonic.tar.gz" -C "$ENGINE_TEST_ROOT/tectonic"
  export GALLEY_TEST_TECTONIC="$ENGINE_TEST_ROOT/tectonic/tectonic"
fi
if [[ -z "${GALLEY_TEST_TECTONIC_CACHE:-}" ]]; then
  export GALLEY_TEST_TECTONIC_CACHE="$ENGINE_TEST_ROOT/cache"
  cat > "$ENGINE_TEST_ROOT/warmup/main.tex" <<'TEX'
\documentclass{article}
\usepackage{graphicx,amsmath,booktabs,natbib,xcolor}
\usepackage[normalem]{ulem}
\usepackage{tikz}
\usetikzlibrary{external}
\begin{document}
Hello Galley.\label{here} See page~\pageref{here}.
\textsc{Galley} \texttt{Galley}
\begin{tikzpicture}\draw (0,0)--(1,1);\end{tikzpicture}
\end{document}
TEX
  TECTONIC_CACHE_DIR="$GALLEY_TEST_TECTONIC_CACHE" XDG_CACHE_HOME="$GALLEY_TEST_TECTONIC_CACHE" "$GALLEY_TEST_TECTONIC" \
    --untrusted --keep-logs "$ENGINE_TEST_ROOT/warmup/main.tex"
  cp templates/blank-article/main.tex "$ENGINE_TEST_ROOT/warmup/template.tex"
  touch "$ENGINE_TEST_ROOT/warmup/refs.bib"
  TECTONIC_CACHE_DIR="$GALLEY_TEST_TECTONIC_CACHE" XDG_CACHE_HOME="$GALLEY_TEST_TECTONIC_CACHE" "$GALLEY_TEST_TECTONIC" \
    --untrusted --keep-logs "$ENGINE_TEST_ROOT/warmup/template.tex"
fi
export GALLEY_TEST_TEXLIVE_PATH="${GALLEY_TEST_TEXLIVE_PATH:-$(dirname "$(command -v latexmk)")}"
export GALLEY_TEST_ENGINE_MATRIX=1
export GALLEY_TEST_SANDBOX=bwrap
cargo test -p galley-build --test engine_matrix --test engine_workflows --locked -- --nocapture
cargo test -p galley-server --test sync real_mcp_engine_matrix_preserves_project_selection --locked -- --nocapture
