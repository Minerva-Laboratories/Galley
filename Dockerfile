# syntax=docker/dockerfile:1
# Container image for any OCI host. Galley runs as one
# binary with the web app embedded. It sandboxes the compiles with bubblewrap
# inside the microVM.

# The Tectonic engine with its whole package bundle (2.9 GB), so builds never download anything.
# Off by default for a small image; build with --build-arg WARM_TECTONIC=1 to include it.
ARG WARM_TECTONIC=0

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

# The engine stage depends only on the pins and the script, so Docker reuses it until the pins
# change: a code change downloads and pushes no new bundle. Every download is checked against
# deploy/tectonic.pins before it is unpacked.
FROM debian:bookworm-slim AS tectonic-1
ARG TARGETARCH
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/*
COPY deploy/tectonic.pins deploy/tectonic-engine.sh /tmp/engine/
RUN TARGETARCH="$TARGETARCH" bash /tmp/engine/tectonic-engine.sh /opt/galley/tectonic
FROM debian:bookworm-slim AS tectonic-0
RUN mkdir -p /opt/galley/tectonic
FROM tectonic-${WARM_TECTONIC} AS tectonic

# 3. A small runtime. It holds the binary, a CA bundle and bubblewrap.
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
# The engine from the stage above, read-only, where Galley looks first. Empty without
# WARM_TECTONIC=1: then `galley engine install` puts one in the data directory. It comes before
# the binary, so a code change reuses this layer instead of pushing the bundle again.
COPY --from=tectonic /opt/galley/tectonic /opt/galley/tectonic
COPY --from=build /src/target/release/galley /usr/local/bin/galley
COPY deploy/fly/entrypoint.sh /usr/local/bin/entrypoint.sh
RUN chmod +x /usr/local/bin/entrypoint.sh
# The defaults. Override them in the fly.toml [env] block. GALLEY_DOMAIN turns
# on public mode.
ENV GALLEY_SANDBOX=auto \
    GALLEY_MEMORY_MB=900 \
    GALLEY_PUBLIC_SIGNUP=false
# glibc gives each busy thread its own memory pool and rarely hands freed memory back. With two
# pools, six imports at once peaked at 105 MB instead of 245 MB on a 512 MB machine.
ENV MALLOC_ARENA_MAX=2
EXPOSE 7000
ENTRYPOINT ["/usr/local/bin/entrypoint.sh"]
