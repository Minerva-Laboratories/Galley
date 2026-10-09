# syntax=docker/dockerfile:1
# Container image for any OCI host. Galley runs as one
# binary with the web app embedded. It sandboxes the compiles with bubblewrap
# inside the microVM.

# 1. Build the web app. Only this stage needs Node.
FROM node:20-bookworm-slim AS web
WORKDIR /web
COPY web/package.json web/package-lock.json ./
RUN npm ci --no-audit --no-fund
COPY web/ ./
RUN npm run build

# 2. Build the Rust binary. It embeds the web/dist built above.
FROM rust:1-bookworm AS build
# libgit2-sys and libz-sys build from source with cc. They need cmake.
RUN apt-get update && apt-get install -y --no-install-recommends cmake \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /src
COPY . .
COPY --from=web /web/dist ./web/dist
# The web app is already built. Do not call npm inside the Rust build.
ENV GALLEY_SKIP_WEB_BUILD=1
RUN cargo build --release -p galley-cli

# 3. A small runtime. It holds the binary, a CA bundle and bubblewrap. Tectonic
#    uses the CA bundle to fetch its own bundle over TLS on the first build.
#    bubblewrap provides the compile sandbox.
FROM debian:bookworm-slim
# latexdiff is a Perl script. With --no-install-recommends it pulls in no TeX Live.
# It drives the optional "Compare PDFs" feature. The bundled Tectonic still
# compiles its marked-up output.
# The DejaVu fonts render the text inside SVG figures. Galley converts those
# figures to PDF itself.
RUN apt-get update && apt-get install -y --no-install-recommends \
      ca-certificates bubblewrap latexdiff fonts-dejavu-core \
    && rm -rf /var/lib/apt/lists/*
# TeX Live adds pdfLaTeX, XeLaTeX, LuaLaTeX and LaTeX/DVI to the Compiler menu. It is off by default
# because it adds about 1.5 GB to the image. Build with --build-arg TEXLIVE=1 to include it.
ARG TEXLIVE=0
RUN if [ "$TEXLIVE" = "1" ]; then \
      apt-get update && apt-get install -y --no-install-recommends \
        texlive-latex-base texlive-latex-recommended texlive-latex-extra \
        texlive-xetex texlive-luatex texlive-fonts-recommended texlive-pstricks \
        texlive-bibtex-extra texlive-science latexmk biber ghostscript \
      && rm -rf /var/lib/apt/lists/*; \
    fi
COPY --from=build /src/target/release/galley /usr/local/bin/galley
# Tectonic with a warm package cache in the image. A machine with no volume, such as a build worker,
# otherwise downloads Tectonic and its packages on a user's first build, which takes about a minute.
# A server's volume mounted at /data hides this copy and keeps its own. Build with
# --build-arg WARM_TECTONIC=1 to include it.
ARG WARM_TECTONIC=0
COPY templates /opt/galley-warm/templates
COPY deploy/tectonic-warm.tex /opt/galley-warm/warm/main.tex
RUN if [ "$WARM_TECTONIC" = "1" ]; then \
      galley --data-dir /data engine install \
      && for d in /opt/galley-warm/warm /opt/galley-warm/templates/*/; do \
           [ -f "$d/main.tex" ] || continue; \
           (cd "$d" && TECTONIC_CACHE_DIR=/data/tectonic/cache XDG_CACHE_HOME=/data/tectonic/cache HOME=/tmp \
             /data/tectonic/tectonic -X compile main.tex -o /tmp >/dev/null 2>&1 || echo "warm-up: $d did not compile"); \
         done \
      && du -sh /data/tectonic/cache; \
    fi
COPY deploy/fly/entrypoint.sh /usr/local/bin/entrypoint.sh
RUN chmod +x /usr/local/bin/entrypoint.sh
# The defaults. Override them in the fly.toml [env] block. GALLEY_DOMAIN turns
# on public mode.
ENV GALLEY_SANDBOX=auto \
    GALLEY_MEMORY_MB=900 \
    GALLEY_PUBLIC_SIGNUP=false
EXPOSE 7000
ENTRYPOINT ["/usr/local/bin/entrypoint.sh"]
