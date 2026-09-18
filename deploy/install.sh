#!/usr/bin/env bash
# Install Galley. The script is idempotent. Run it again to upgrade the binary.
#
#   sudo deploy/install.sh          system-wide, with a systemd service
#   deploy/install.sh               for your user only, in ~/.local/bin, no service
#
# Run it from the repo root after `cargo build --release`, or point BIN_SRC at a
# binary you downloaded. It does NOT install a reverse proxy and it does NOT set
# your domain. See deploy/README.md.
set -euo pipefail

BIN_SRC="${BIN_SRC:-target/release/galley}"
SVC=/etc/systemd/system/galley.service
# Answer the optional prompts without a terminal: ASSUME_YES=1 deploy/install.sh
ASSUME_YES="${ASSUME_YES:-}"

if [[ $EUID -eq 0 ]]; then
	SYSTEM=1
	BIN_DST=/usr/local/bin/galley
	CONF_DIR=/etc/galley
	DATA_DIR=/var/lib/galley
	AS_GALLEY=(sudo -u galley)
else
	SYSTEM=0
	BIN_DST="$HOME/.local/bin/galley"
	CONF_DIR="$HOME/.galley"
	DATA_DIR="$HOME/.galley"
	AS_GALLEY=()
fi

say() { printf '%s\n' "$*"; }
die() {
	printf '%s\n' "$*" >&2
	exit 1
}
ask() {
	# ask "question" returns 0 for yes. The default is no when nobody can answer.
	[[ -n "$ASSUME_YES" ]] && return 0
	[[ -t 0 ]] || return 1
	read -r -p "$1 [y/N] " reply
	[[ $reply == [yY]* ]]
}

[[ -x "$BIN_SRC" ]] || die "No binary at $BIN_SRC. Build it first: cargo build --release"
case "$(uname -m)" in
x86_64 | aarch64 | arm64) ;;
*) say "Note: $(uname -m) is not a platform we test on (x86_64 and arm64 are)." ;;
esac

# 1. A dedicated unprivileged user. The system install only.
if [[ $SYSTEM -eq 1 ]] && ! id galley &>/dev/null; then
	useradd --system --home "$DATA_DIR" --shell /usr/sbin/nologin galley
	say "Created system user 'galley'."
fi

# 2. A compile sandbox. bubblewrap is preferred because it needs no daemon.
#    Without a sandbox, Galley refuses to serve publicly. Install one now.
if ! command -v bwrap &>/dev/null && ! command -v docker &>/dev/null; then
	if [[ $SYSTEM -eq 1 ]] && command -v apt-get &>/dev/null; then
		say "Installing bubblewrap for the compile sandbox..."
		apt-get update -qq && apt-get install -y bubblewrap
	else
		say "No compile sandbox found. Install one before serving publicly: sudo apt install bubblewrap"
	fi
fi

# 3. Fonts. Without them, text inside SVG figures comes out blank.
if [[ $SYSTEM -eq 1 ]] && command -v apt-get &>/dev/null && ! ls /usr/share/fonts/* &>/dev/null; then
	apt-get install -y fonts-dejavu-core || say "Font install skipped; SVG figures with text may render blank."
fi

# 4. Binary.
install -d "$(dirname "$BIN_DST")"
install -m 0755 "$BIN_SRC" "$BIN_DST"
say "Installed $("$BIN_DST" --version 2>/dev/null || echo galley) to $BIN_DST."
if [[ $SYSTEM -eq 0 ]] && ! command -v galley &>/dev/null; then
	say "Add it to your PATH:  echo 'export PATH=\"\$HOME/.local/bin:\$PATH\"' >> ~/.profile"
fi

# 5. Directories and config. Never overwrite an existing config.
mkdir -p "$CONF_DIR"
if [[ $SYSTEM -eq 1 ]]; then
	install -d -o galley -g galley "$DATA_DIR"
else
	install -d "$DATA_DIR"
fi
if [[ -f "$CONF_DIR/galley.toml" ]]; then
	say "Kept existing $CONF_DIR/galley.toml."
elif [[ $SYSTEM -eq 1 ]]; then
	# A server config. It binds loopback, has a domain to edit, and keeps data
	# in /var/lib/galley.
	install -m 0644 deploy/galley.toml.example "$CONF_DIR/galley.toml"
	say "Wrote $CONF_DIR/galley.toml. Edit 'domain' before going public."
else
	# Galley's own defaults. Localhost, no domain, no public mode.
	"$BIN_DST" --config "$CONF_DIR/galley.toml" init
fi

# 6. Fetch the Tectonic engine now, so that the first build is fast.
"${AS_GALLEY[@]}" "$BIN_DST" --data-dir "$DATA_DIR" engine install ||
	say "Engine prefetch skipped (it will download on the first build)."

# 7. LanguageTool is optional and needs Java. Never install it silently.
if [[ $SYSTEM -eq 1 ]] && ! command -v java &>/dev/null && command -v apt-get &>/dev/null; then
	if ask "Install a Java runtime, for optional LanguageTool grammar checking?"; then
		apt-get install -y default-jre-headless
		say "Java installed. Run a LanguageTool server, then set [grammar] languagetool in $CONF_DIR/galley.toml."
	else
		say "Skipped. Grammar checking stays off; everything else works."
	fi
fi

# 8. The systemd unit. The system install only.
if [[ $SYSTEM -eq 1 ]]; then
	install -m 0644 deploy/galley.service "$SVC"
	systemctl daemon-reload
	systemctl enable --now galley
	sleep 1
	systemctl --no-pager --lines=0 status galley || true
fi

# 9. Let Galley report what is in place and what is missing.
say ""
"${AS_GALLEY[@]}" "$BIN_DST" --data-dir "$DATA_DIR" doctor || true

say ""
if [[ $SYSTEM -eq 1 ]]; then
	cat <<'NEXT'
Galley is installed and running on 127.0.0.1:7000.

Next:
  1. Create the first admin account (or open the site and use the first-run screen):
       sudo -u galley galley --data-dir /var/lib/galley admin create-user \
         you@example.com --name "Your Name" --password 'a-strong-password' --admin
  2. Put a TLS reverse proxy in front of it (deploy/Caddyfile), or expose it
     on your own network. See deploy/README.md.
NEXT
else
	cat <<'NEXT'
Galley is installed for your user. Start it with:

    galley serve          → http://127.0.0.1:7000

The first screen asks you to create an admin account. To run it as a background
service that survives logout:

    systemctl --user enable --now galley    # after copying deploy/galley.service
    sudo loginctl enable-linger "$USER"
NEXT
fi
