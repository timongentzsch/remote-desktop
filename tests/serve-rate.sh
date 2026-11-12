#!/bin/sh
# Headless sway, with fast-changing text ($1 = moving, or movingN to stop after N seconds) or a still screen,
# served to the container's network.
mkdir -p $XDG_RUNTIME_DIR && chmod 700 $XDG_RUNTIME_DIR
if [ "$1" = moving ]; then
  show='exec foot -F sh -c "while :; do head -c 3000 /dev/urandom | base64 -w 0; done"'
elif [ "$1" = scroll ]; then
  show='exec foot -F sh -c "while :; do ls -lR /usr/lib 2>/dev/null | while read line; do echo \"\$line\"; sleep 0.004; done; done"'
elif [ "${1#moving}" != "$1" ]; then
  show="exec foot -F sh -c \"timeout ${1#moving} sh -c 'while :; do head -c 3000 /dev/urandom | base64 -w 0; done'; sleep 100000\""
else
  show='exec foot -F sh -c "ls -l /usr/bin; sleep 100000"'
fi
printf 'output * resolution 1920x1080\n%s\n' "$show" > /tmp/sway.conf
WLR_BACKENDS=headless WLR_RENDERER=pixman WLR_LIBINPUT_NO_DEVICES=1 sway -c /tmp/sway.conf >/tmp/sway.log 2>&1 &
sleep 3
export WAYLAND_DISPLAY=wayland-1
./remote-desktop password --none >/dev/null
exec ./remote-desktop --listen 0.0.0.0:8765
