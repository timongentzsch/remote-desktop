#!/bin/sh
# Installs the remote desktop host for the desktop session this runs in, in one line:
#
#     curl -fsSL https://github.com/timongentzsch/remote-desktop/releases/latest/download/install.sh | sh
#
# Options follow `sh -s --` and go to `remote-desktop install`:
#
#     --tailscale     a certificate from Tailscale, and only devices on the tailnet are answered
#     --no-password   no login password (with --tailscale: anyone on the tailnet who may reach this machine)
#     --port=PORT     another port than HTTPS's own, 443, where that one is taken
#
# REMOTE_DESKTOP_RELEASE names where the binaries are, for another version than the latest, such as
# https://github.com/timongentzsch/remote-desktop/releases/download/v0.3.0.
set -eu

release=${REMOTE_DESKTOP_RELEASE:-https://github.com/timongentzsch/remote-desktop/releases/latest/download}

fail() { echo "remote-desktop: $1" >&2; exit 1; }

case $(uname -m) in
    x86_64 | amd64) architecture=x86_64 ;;
    aarch64 | arm64) architecture=aarch64 ;;
    *) fail "there is no build for $(uname -m), only x86_64 and aarch64" ;;
esac

# Which system this is: what cannot work is said before anything is downloaded, and what the install will ask
# for is said before it asks.
case $(uname -s) in
    Darwin)
        [ "$architecture" = aarch64 ] || fail "on macOS there is a build for Apple Silicon only"
        architecture=aarch64-macos
        echo "This is a Mac. The install asks for your password once, to put the program in /usr/local/bin."
        echo "macOS then asks to allow remote-desktop under System Settings, Privacy & Security, in both"
        echo "Screen & System Audio Recording and Accessibility. Allow both: the install waits until it is."
        ;;
    Linux)
        case $(uname -r) in
            *[Mm]icrosoft*) fail "this is Linux inside Windows, which has no desktop of its own. In Windows, download
    $release/remote-desktop-x86_64-windows.exe
and run it in a terminal with: install" ;;
        esac
        [ ! -e /etc/NIXOS ] || fail "NixOS runs no binaries built for other distributions as they are"
        getconf GNU_LIBC_VERSION >/dev/null 2>&1 || fail "this system has no glibc, which the builds need (2.28 or newer)"
        echo "This is Linux${XDG_CURRENT_DESKTOP:+, with $XDG_CURRENT_DESKTOP}${XDG_SESSION_TYPE:+ on $XDG_SESSION_TYPE}. The install asks for sudo: to put the program in"
        echo "/usr/local/bin, to let you create input devices (a udev rule), and to open its port in ufw or firewalld."
        if [ -z "${WAYLAND_DISPLAY:-}${DISPLAY:-}" ]; then
            echo "No desktop is seen from this terminal, as over ssh. The host shows the desktop session of the user who"
            echo "installs it: if it shows nothing, run this again in a terminal of that desktop."
        fi
        ;;
    MINGW* | MSYS* | CYGWIN*) fail "on Windows, download
    $release/remote-desktop-x86_64-windows.exe
and run it in a terminal with: install" ;;
    *) fail "there is no build for $(uname -s), only Linux, macOS and Windows" ;;
esac
case " $* " in
    *" --tailscale "*) command -v tailscale >/dev/null || fail "--tailscale needs Tailscale's command, tailscale, on this machine and in the PATH: https://tailscale.com/download" ;;
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
