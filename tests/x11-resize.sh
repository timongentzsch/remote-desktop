#!/bin/sh
# X11: a screen that changes size mid-session keeps streaming at the new size.
mkdir -p $XDG_RUNTIME_DIR && chmod 700 $XDG_RUNTIME_DIR
Xvfb :1 -screen 0 1920x1080x24 >/tmp/x.log 2>&1 &
sleep 2
xterm -geometry 100x30+50+50 -e sh -c 'while :; do date; sleep 0.2; done' 2>/dev/null &
sleep 1
echo "modes: $(xrandr | grep -o "^ *[0-9]*x[0-9]*" | tr -s ' \n' ' ')"
./remote-desktop password --none >/dev/null
(./remote-desktop --listen 127.0.0.1:8765 >/tmp/host.log 2>&1; echo "host exited with status $?" >>/tmp/host.log) &
sleep 2
export CAFILE=$HOME/.config/remote-desktop/certificate.pem NAME=localhost
(sleep 2; xrandr --fb 1280x720 >/dev/null 2>&1; sleep 0.3; xdotool mousemove 5 5) &
python3 wsclient.py '{"password":"","bitrate":8000}' 7 /tmp/c.flv >/dev/null; wait $!
echo "sizes seen: $(ffprobe -v error -select_streams v -show_entries frame=width,height -of csv=p=0 /tmp/c.flv | uniq -c | tr '\n' ';')"
sort /tmp/host.log | uniq -c | cut -c1-170
