#!/bin/sh
# Headless sway with two screens of different sizes, a terminal on the first, served to the container's network.
mkdir -p $XDG_RUNTIME_DIR && chmod 700 $XDG_RUNTIME_DIR
printf 'output HEADLESS-1 resolution 1920x1080 position 0 0\nexec foot -F sh -c "ls -la /usr/bin; sleep 100000"\n' > /tmp/sway.conf
WLR_BACKENDS=headless WLR_RENDERER=pixman WLR_LIBINPUT_NO_DEVICES=1 sway -c /tmp/sway.conf >/tmp/sway.log 2>&1 &
sleep 3
export WAYLAND_DISPLAY=wayland-1 SWAYSOCK=$(ls $XDG_RUNTIME_DIR/sway-ipc.*.sock)
swaymsg create_output >/dev/null && swaymsg output HEADLESS-2 resolution 1280x720 position 1920 0 >/dev/null
./remote-desktop password --none >/dev/null
exec ./remote-desktop --listen 0.0.0.0:8765
