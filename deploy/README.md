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

## Build workers

One machine holds the documents, the accounts and the queue, and needs little CPU. Compiles need
a lot of it, in bursts. To add capacity, run build workers on other machines:

```sh
GALLEY_WORKER_TOKEN=<a long random secret> galley worker --bind 0.0.0.0:7100
```

and point the server at them, with the same token in its environment:

```toml
[build]
worker_url = "http://workers.internal:7100"
max_concurrent = 8        # now the number of compiles across all workers
```

Use the same image or packages on the server and on the workers: the server still checks which
compilers exist, compares PDFs and verifies submission packages itself. Each worker needs
bubblewrap or Docker, like the server. Keep workers on a private network; they accept work from
anything that holds the token.

A worker holds no documents beyond copies it can rebuild, so workers can stop when idle and start
on demand behind any load balancer that sends one compile to each worker at a time. A worker
keeps its copy of each project it built, so a repeated build there is faster, but any worker can
build any project.

## Backups

### Continuous backup to object storage

A disk on one machine can fail. Set a bucket on any S3-compatible store
(Cloudflare R2, Backblaze B2, AWS S3, Tigris, MinIO) and Galley copies every
project that changed, and the database, every two minutes:

```toml
[backup]
endpoint = "https://<account>.r2.cloudflarestorage.com"
bucket = "galley-backup"
interval_s = 120
```

Keep the keys out of the file: set `GALLEY_BACKUP_ACCESS_KEY_ID` and
`GALLEY_BACKUP_SECRET_ACCESS_KEY` in the environment. Galley also reads the
standard `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`, `AWS_ENDPOINT_URL_S3` and
`BUCKET_NAME` variables, which some hosts set when they attach a bucket.

The bucket holds `galley.db.gz` (accounts, members, comments, tokens), one
database copy per day under `daily/`, and `projects/<id>.tar.gz` for each
project: its git repository, files, live editing state and settings. Build
output is left out. Turn on object versioning in the bucket if you want older
copies of each project archive as well; the git history inside already keeps
every version of the text.

`/api/health` reports whether the last pass worked and when. A failed upload
is logged and retried on the next pass. It never delays a save.

To rebuild a server, stop Galley, then restore into an empty data directory and
start Galley on it:

```sh
galley --data-dir /var/lib/galley-restored backup restore
galley --data-dir /var/lib/galley-restored serve
```

A restore refuses a data directory that already holds a database or projects.
`galley backup run` makes one pass by hand.

### A local copy

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
