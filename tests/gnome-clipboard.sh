#!/bin/sh
# GNOME (headless Mutter): clipboard sharing in both directions.
mkdir -p $XDG_RUNTIME_DIR && chmod 700 $XDG_RUNTIME_DIR
exec dbus-run-session -- sh -c '
  pipewire >/tmp/pipewire.log 2>&1 & sleep 1; wireplumber >/tmp/wireplumber.log 2>&1 &
  sleep 2
  mutter --headless --wayland --virtual-monitor 1920x1080 >/tmp/compositor.log 2>&1 &
  sleep 5
  export WAYLAND_DISPLAY=wayland-0
  foot -F sh -c "while :; do date; sleep 0.5; done" >/tmp/foot.log 2>&1 &
  sleep 1
  ./remote-desktop password --none >/dev/null
  (./remote-desktop --listen 127.0.0.1:8765 >/tmp/host.log 2>&1; echo "host exited with status $?" >>/tmp/host.log) &
  sleep 2
  export CAFILE=$HOME/.config/remote-desktop/certificate.pem NAME=localhost
  # Applications reach the clipboard here through the X server Mutter runs: its Wayland seat has no keyboard,
  # which Wayland clipboard tools need.
  export DISPLAY=:0 XAUTHORITY=$(ls $XDG_RUNTIME_DIR/.mutter-Xwaylandauth.* | head -1)
  paste() { timeout 3 xclip -selection clipboard -o 2>&1 | tr "\n" " "; }
  printf "on the desktop before anyone connected" | xclip -selection clipboard -i; sleep 0.5
  (sleep 2; echo "pasted on the desktop after the client copied: $(paste)"; sleep 1; printf "copied on the desktop: äöü" | xclip -selection clipboard -i) &
  python3 wsclient.py "{\"password\":\"\"}" 6 /tmp/a.flv "[\"clipboard\", \"from the client: grüße 123\", 0]" | grep "clipboard"; wait $!
  echo "pasted on the desktop after the session: $(paste)"
  sort /tmp/host.log | uniq -c | cut -c1-170
  grep -i "remote\|clipboard\|selection" /tmp/compositor.log | tail -3 | cut -c1-160
'
