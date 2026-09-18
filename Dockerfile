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
COPY --from=build /src/target/release/galley /usr/local/bin/galley
COPY deploy/fly/entrypoint.sh /usr/local/bin/entrypoint.sh
RUN chmod +x /usr/local/bin/entrypoint.sh
# The defaults. Override them in the fly.toml [env] block. GALLEY_DOMAIN turns
# on public mode.
ENV GALLEY_SANDBOX=auto \
    GALLEY_MEMORY_MB=900 \
    GALLEY_PUBLIC_SIGNUP=false
EXPOSE 7000
ENTRYPOINT ["/usr/local/bin/entrypoint.sh"]
