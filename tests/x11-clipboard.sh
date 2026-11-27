#!/bin/sh
# X11: clipboard sharing in both directions.
Xvfb :1 -screen 0 1920x1080x24 >/tmp/x.log 2>&1 &
sleep 2
xterm -geometry 120x40+100+100 -e sh -c 'ls -la /usr/bin | head -30; sleep 100' 2>/dev/null &
sleep 1
./remote-desktop password --none >/dev/null
(./remote-desktop --listen 127.0.0.1:8765 >/tmp/host.log 2>&1; echo "host exited with status $?" >>/tmp/host.log) &
sleep 2
paste() { timeout 3 xclip -selection clipboard -o "$@" 2>&1 | tr '\n' ' '; }
printf 'on the desktop before anyone connected' | xclip -selection clipboard -i
(sleep 2; echo "pasted on the desktop after the client copied: $(paste)"; echo "forms offered to a pasting application: $(paste -t TARGETS)"; sleep 1; printf 'copied on the desktop: äöü' | xclip -selection clipboard -i) &
python3 wsclient.py '{"password":""}' 6 /tmp/a.flv '["clipboard", "from the client: grüße 123", 0]' | grep "clipboard"; wait $!
echo "pasted on the desktop after the session: $(paste)"
sort /tmp/host.log | uniq -c | cut -c1-170
