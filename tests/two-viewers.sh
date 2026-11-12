#!/bin/sh
# Several clients at once and in turn. All get video; where the GPU encodes, one of them at a time has it.
mkdir -p $XDG_RUNTIME_DIR && chmod 700 $XDG_RUNTIME_DIR
printf 'output * resolution 1920x1080\nexec foot -F sh -c "while :; do date; sleep 0.1; done"\n' > /tmp/sway.conf
WLR_BACKENDS=headless WLR_RENDERER=pixman WLR_LIBINPUT_NO_DEVICES=1 sway -c /tmp/sway.conf >/tmp/sway.log 2>&1 &
sleep 3
export WAYLAND_DISPLAY=wayland-1
./remote-desktop password --none >/dev/null
(./remote-desktop --listen 127.0.0.1:8765 >/tmp/host.log 2>&1; echo "host exited with status $?" >>/tmp/host.log) &
sleep 2
export CAFILE=$HOME/.config/remote-desktop/certificate.pem NAME=localhost
viewer() { python3 wsclient.py '{"password":""}' $1 /tmp/$2.flv | grep -o "first video after [0-9.None]* s, [0-9]* ws messages" | sed "s/^/$2: /"; }
viewer 6 first &
first=$!
sleep 1.5
viewer 3 second
viewer 1 third-while-first-still-watches
wait $first
viewer 1 fourth-after-all-left
cat /tmp/host.log
