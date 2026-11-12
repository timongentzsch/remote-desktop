#!/bin/sh
# Checks clipboard sharing in both directions on a headless compositor. Argument: sway or kwin.
mkdir -p $XDG_RUNTIME_DIR && chmod 700 $XDG_RUNTIME_DIR
export KIND=$1
exec dbus-run-session -- sh -c '
  pipewire >/tmp/pipewire.log 2>&1 & sleep 1; wireplumber >/tmp/wireplumber.log 2>&1 &
  sleep 2
  if [ "$KIND" = kwin ]; then
    kwin_wayland --virtual --width 1920 --height 1080 --no-lockscreen --socket wayland-0 >/tmp/compositor.log 2>&1 &
    export WAYLAND_DISPLAY=wayland-0
  else
    WLR_BACKENDS=headless WLR_RENDERER=pixman WLR_LIBINPUT_NO_DEVICES=1 sway >/tmp/compositor.log 2>&1 &
    export WAYLAND_DISPLAY=wayland-1
  fi
  sleep 5
  ./remote-desktop password --none >/dev/null
  ./remote-desktop --listen 127.0.0.1:8765 >/tmp/host.log 2>&1 &
  sleep 2
  export CAFILE=$HOME/.config/remote-desktop/certificate.pem NAME=localhost
  (sleep 3; printf "copied on the desktop: äöü" | wl-copy) &
  python3 wsclient.py "{\"password\":\"\"}" 6 /tmp/out.flv "[\"clipboard\", \"from the client: grüße 123\", 0]" | grep "clipboard\|first video"
  echo "desktop clipboard after the client copied (read before the desktop copy): see below"
  python3 wsclient.py "{\"password\":\"\"}" 2 /tmp/out.flv "[\"clipboard\", \"second text from the client\", 0]" | grep "clipboard"
  echo "desktop clipboard now: $(wl-paste -n 2>/dev/null | head -c 100)"
  cat /tmp/host.log | cut -c1-160
'
