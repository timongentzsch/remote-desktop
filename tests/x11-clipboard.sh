#!/bin/sh
# X11: clipboard sharing in both directions, and a screen that changes size mid-session.
mkdir -p $XDG_RUNTIME_DIR && chmod 700 $XDG_RUNTIME_DIR
Xvfb :1 -screen 0 1920x1080x24 >/tmp/x.log 2>&1 &
sleep 2
xterm -geometry 120x40+100+100 -e sh -c 'ls -la /usr/bin | head -30; sleep 100' 2>/dev/null &
sleep 1
./remote-desktop password --none >/dev/null
(./remote-desktop --listen 127.0.0.1:8765 >/tmp/host.log 2>&1; echo "host exited with status $?" >>/tmp/host.log) &
sleep 2
export CAFILE=$HOME/.config/remote-desktop/certificate.pem NAME=localhost
paste() { timeout 3 xclip -selection clipboard -o "$@" 2>&1 | tr '\n' ' '; }
printf 'on the desktop before anyone connected' | xclip -selection clipboard -i
(sleep 2; echo "pasted on the desktop after the client copied: $(paste)"; echo "forms offered to a pasting application: $(paste -t TARGETS)"; sleep 1; printf 'copied on the desktop: äöü' | xclip -selection clipboard -i) &
python3 wsclient.py '{"password":""}' 6 /tmp/a.flv '["clipboard", "from the client: grüße 123", 0]' | grep "clipboard"; wait $!
echo "pasted on the desktop after the session: $(paste)"
(sleep 2; xrandr -s 1280x720 2>&1 | head -1) &
python3 wsclient.py '{"password":"","bitrate":8000}' 6 /tmp/c.flv >/dev/null; wait $!
echo "sizes seen while the screen was resized: $(ffprobe -v error -select_streams v -show_entries frame=width,height -of csv=p=0 /tmp/c.flv | sort | uniq -c | tr '\n' ';')"
sort /tmp/host.log | uniq -c | cut -c1-170
