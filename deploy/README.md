# Running Galley on your own machine

Galley is one static binary with the web app embedded, a SQLite database, and a
git repository per project. There is no separate database server, queue, or Node
runtime. One host runs everything.

This directory holds what you need to run it as a service:

| File | Purpose |
|---|---|
| `install.sh` | Installs the binary, the data directory, the config and the service |
| `galley.service` | A systemd unit for the system install |
| `galley.toml.example` | A configuration file to copy and edit |
| `Caddyfile` | A reverse proxy example for a host with a domain name |

## Install

```sh
cargo build --release
sudo deploy/install.sh      # system-wide, with a systemd service
deploy/install.sh           # or for your user only, with no service
```

The installer ends by running `galley doctor`, which checks the sandbox, the
build engine, the package cache, disk space, the listen address, fonts and the
certificate ports. Run it again at any time.

## Optional TeX Live compilers

Tectonic remains the default. To offer pdfLaTeX, XeLaTeX, LuaLaTeX and LaTeX/DVI, install TeX Live
and its auxiliary tools as the administrator. For Debian/Ubuntu:

```sh
sudo apt-get update
sudo apt-get install texlive-latex-base texlive-latex-recommended texlive-latex-extra \
  texlive-xetex texlive-luatex texlive-fonts-recommended texlive-pstricks texlive-bibtex-extra \
  latexmk biber ghostscript latexdiff
```

Galley checks `latexmk`, `kpsewhich`, the engine executable, BibTeX and Biber. LaTeX/DVI also
requires `dvips` and `ps2pdf` (Ghostscript). Install additional packages and fonts required by your
documents; a missing package produces an actionable build error. Galley does not install or update
TeX Live and does not fall back to another compiler.

`[build].texlive_path` is the directory containing the executables; leave it empty to use `PATH`.
With bubblewrap, Galley mounts the discovered TeX distribution, configuration and fonts read-only,
with writable caches in the build area. All TeX Live passes and bibliography/conversion tools run
without network access. Project and user `latexmkrc` files are ignored, and shell escape is disabled.

For Docker, install tools **inside the compile image**. The default lightweight image remains
unchanged. For example:

```dockerfile
FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends \
    texlive-latex-base texlive-latex-recommended texlive-latex-extra \
    texlive-xetex texlive-luatex texlive-fonts-recommended texlive-pstricks texlive-bibtex-extra \
    latexmk biber ghostscript latexdiff fonts-dejavu-core \
    && rm -rf /var/lib/apt/lists/*
```

Build that image, then set `[build].docker_image` to its tag and `sandbox = "docker"`.
`texlive_path` and tool availability refer to paths inside that image. Host TeX Live installations
are not used by Docker builds. Run `galley doctor` with the same configuration to inspect each
compiler's version and missing requirements.

`[build].engine` sets the initial selection for new projects. Existing projects without an engine
field retain Tectonic. Editors save a project choice using the Compiler selector; readers see it.
Normal builds, drafts, PDF comparisons and submission verification use that choice. REST and MCP
can override it for one build. The submission archive records its compiler, and figure-cache keys
include the engine and version. LaTeX/DVI compiles TikZ normally and accepts EPS/PS; convert SVGs
before uploading because automatic SVG conversion and the PDF figure cache are disabled in that mode.

## The compile sandbox is required

A compile runs arbitrary LaTeX, so it runs inside a sandbox. In public mode,
which means `server.domain` is set, Galley **refuses to start without one**. An
unsandboxed LaTeX compiler must never face the internet.

- **bubblewrap** (`bwrap`) is preferred. It needs no daemon and runs
  unprivileged. It does need unprivileged user namespaces. On Ubuntu 24.04 an
  AppArmor policy restricts them. If the startup log shows `sandbox: none`,
  allow user namespaces:

  ```sh
  echo 'kernel.apparmor_restrict_unprivileged_userns = 0' | \
    sudo tee /etc/sysctl.d/60-galley.conf && sudo sysctl --system
  ```

  The log line `sandbox ready kind=bwrap`, or `kind=docker`, confirms it.

- **Docker** works as a fallback with `sandbox = "docker"`. The host needs the
  Docker engine.

Running Galley itself inside a container is not covered here. The nested compile
sandbox needs privileges and is hard to set up. Run the binary on the host and
let it sandbox the compiles.

## TLS and a domain name

Set `server.domain` in `galley.toml` and Galley obtains a certificate itself,
which needs ports 80 and 443. If something else already holds those ports, put
Galley behind it instead and keep `domain` empty. `Caddyfile` in this directory
is an example of that arrangement. It forwards WebSocket upgrades, which live
editing needs, and allows long responses, which slow compiles need.

Without a domain name, bind to the loopback address and reach the machine over
your own network or a private tunnel.

## Backups

Everything lives under the data directory, `/var/lib/galley` for a system
install. Each project is a plain git repository, so a second copy can also be a
git remote you push to.

```sh
sudo systemctl stop galley
sudo tar czf galley-backup-$(date +%F).tgz -C /var/lib galley
sudo systemctl start galley
```

Stopping the service first keeps the SQLite database consistent.

## Updating

Build the new binary and re-run the installer. It replaces the binary, keeps
your configuration and restarts the service. Database migrations run on start.
Git repositories never need migration.

```sh
git pull && cargo build --release && sudo deploy/install.sh
```
