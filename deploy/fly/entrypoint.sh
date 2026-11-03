#!/bin/sh
# Write galley.toml from the environment, which is the Fly [env] block and the
# Fly secrets, onto the persistent volume. Then start the server. All of the
# state lives under /data.
set -e

CONF=/data/galley.toml
mkdir -p /data

cat > "$CONF" <<EOF
[server]
bind = "0.0.0.0:7000"
domain = "${GALLEY_DOMAIN:-}"
data_dir = "/data"
public_signup = ${GALLEY_PUBLIC_SIGNUP:-false}

[build]
engine = "${GALLEY_ENGINE:-tectonic}"
auto_build = ${GALLEY_AUTO_BUILD:-false}
auto_build_delay_s = ${GALLEY_AUTO_BUILD_DELAY_S:-10}
sandbox = "${GALLEY_SANDBOX:-auto}"
timeout_s = 120
memory_mb = ${GALLEY_MEMORY_MB:-900}
cpus = 1.0
max_concurrent = ${GALLEY_MAX_BUILDS:-1}
allow_unsandboxed = ${GALLEY_ALLOW_UNSANDBOXED:-false}
worker_url = "${GALLEY_WORKER_URL:-}"

[limits]
# Fly's proxy connects from a private fdaa: address and sets Fly-Client-IP to the real
# client. Without this line every user would share the proxy's rate-limit budget.
client_ip_header = "Fly-Client-IP"
EOF

# The same image runs as a build worker when GALLEY_ROLE=worker. A worker keeps only build copies,
# which it can rebuild, so it needs no volume.
if [ "${GALLEY_ROLE:-server}" = "worker" ]; then
  exec galley --config "$CONF" --data-dir /data worker --bind 0.0.0.0:7100
fi
exec galley --config "$CONF" --data-dir /data serve
