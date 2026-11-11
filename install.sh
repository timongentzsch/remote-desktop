#!/bin/sh
# Sets the login password, grants the logged-in user access to /dev/uinput
# (needs sudo once) and runs the host as a systemd user service.
set -eu

dir=$(cd "$(dirname "$0")" && pwd)
unit=${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user/remote-desktop.service

# With no argument the existing password file is kept.
[ $# -eq 0 ] || (umask 077 && printf %s "$1" >"$dir/password")
[ -s "$dir/password" ] || { echo "usage: $0 <password>" >&2; exit 1; }
command -v gpu-screen-recorder >/dev/null || { echo "install gpu-screen-recorder first" >&2; exit 1; }

# "uaccess" gives the device to whoever is logged in on the seat; the number must stay below 73.
echo 'KERNEL=="uinput", SUBSYSTEM=="misc", TAG+="uaccess", OPTIONS+="static_node=uinput"' |
  sudo tee /etc/udev/rules.d/70-remote-desktop.rules >/dev/null
sudo modprobe uinput
sudo udevadm control --reload
sudo udevadm trigger --name-match=uinput

mkdir -p "$(dirname "$unit")"
cat >"$unit" <<EOF
[Unit]
Description=Remote desktop host
PartOf=graphical-session.target
After=graphical-session.target

[Service]
ExecStart=$dir/host.py
Restart=on-failure

[Install]
WantedBy=graphical-session.target
EOF
systemctl --user daemon-reload
systemctl --user enable --now remote-desktop.service
