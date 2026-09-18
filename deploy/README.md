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
