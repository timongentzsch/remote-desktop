#!/bin/sh
# Installs the remote desktop host for the desktop session this runs in, in one line:
#
#     curl -fsSL https://github.com/timongentzsch/remote-desktop/releases/latest/download/install.sh | sh
#
# Options follow `sh -s --` and go to `remote-desktop install`:
#
#     --tailscale     a certificate from Tailscale, and only devices on the tailnet are answered
#     --no-password   no login password (with --tailscale: anyone on the tailnet who may reach this machine)
#
# REMOTE_DESKTOP_RELEASE names where the binaries are, for another version than the latest, such as
# https://github.com/timongentzsch/remote-desktop/releases/download/v0.3.0.
set -eu

release=${REMOTE_DESKTOP_RELEASE:-https://github.com/timongentzsch/remote-desktop/releases/latest/download}

case $(uname -m) in
    x86_64 | amd64) architecture=x86_64 ;;
    aarch64 | arm64) architecture=aarch64 ;;
    *) echo "remote-desktop: there is no build for $(uname -m), only x86_64 and aarch64" >&2; exit 1 ;;
esac

directory=$(mktemp -d)
trap 'rm -rf "$directory"' EXIT
echo "Downloading remote-desktop for $architecture"
curl -fsSL -o "$directory/remote-desktop" "$release/remote-desktop-$architecture"
chmod +x "$directory/remote-desktop"

# Piped into sh, standard input is this script: the password prompt and sudo need the terminal instead.
# Without one, as over a plain ssh command, only --no-password and passwordless sudo get through.
if (: </dev/tty) 2>/dev/null; then
    "$directory/remote-desktop" install "$@" </dev/tty
else
    "$directory/remote-desktop" install "$@" </dev/null
fi
