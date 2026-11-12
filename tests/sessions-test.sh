#!/bin/sh
# Runs several sessions in a row against a headless sway and prints the host's memory after each.
mkdir -p $XDG_RUNTIME_DIR && chmod 700 $XDG_RUNTIME_DIR
printf 'output * resolution 1920x1080\n' > /tmp/sway.conf
WLR_BACKENDS=headless WLR_RENDERER=pixman WLR_LIBINPUT_NO_DEVICES=1 sway -c /tmp/sway.conf >/tmp/sway.log 2>&1 &
sleep 3
export WAYLAND_DISPLAY=wayland-1
./remote-desktop password --none >/dev/null
(./remote-desktop --listen 127.0.0.1:8765 >/tmp/host.log 2>&1; echo "host exited with status $?" >>/tmp/host.log) &
sleep 2
export CAFILE=$HOME/.config/remote-desktop/certificate.pem NAME=localhost
rss() { awk '/VmRSS/ { print $2 " kB" }' /proc/$(pidof remote-desktop)/status; }
echo "at start: $(rss)"
for i in $(seq 1 $1); do
  python3 wsclient.py "{\"password\":\"\",$2\"fps\":\"60\"}" ${3:-1.5} /tmp/out.flv | grep -o "first video after [0-9.]* s" | tr "\n" ","
  echo " then $(rss)"
done
sort /tmp/host.log | uniq -c
