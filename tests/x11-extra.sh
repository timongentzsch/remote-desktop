#!/bin/sh
# X11: a pointer that moves over a still screen must produce frames when it is painted into the picture, and a
# screen that changes size must keep streaming at the new size.
mkdir -p $XDG_RUNTIME_DIR && chmod 700 $XDG_RUNTIME_DIR
Xvfb :1 -screen 0 1920x1080x24 >/tmp/x.log 2>&1 &
sleep 2
xterm -geometry 120x40+100+100 -e sh -c 'ls -la /usr/bin | head -30; sleep 100' &
sleep 1
./remote-desktop password --none >/dev/null
(./remote-desktop --listen 127.0.0.1:8765 >/tmp/host.log 2>&1; echo "host exited with status $?" >>/tmp/host.log) &
sleep 2
export CAFILE=$HOME/.config/remote-desktop/certificate.pem NAME=localhost
frames() { ffprobe -v error -select_streams v -show_entries packet=size -of csv=p=0 $1 | wc -l; }
move() { for i in $(seq 1 60); do xdotool mousemove $((300 + i * 5)) $((300 + i * 2)); sleep 0.05; done; }
(sleep 1; move) &
python3 wsclient.py '{"password":"","cursor":"yes","bitrate":8000}' 5 /tmp/a.flv >/dev/null; wait $!
echo "pointer painted, moving over a still screen: $(frames /tmp/a.flv) frames"
(sleep 1; move) &
python3 wsclient.py '{"password":"","bitrate":8000}' 5 /tmp/b.flv >/dev/null; wait $!
echo "pointer drawn by the client, same movement: $(frames /tmp/b.flv) frames"
(sleep 2; xrandr --fb 1280x720 2>&1 | head -2; xdotool mousemove 10 10) &
python3 wsclient.py '{"password":"","bitrate":8000}' 6 /tmp/c.flv >/dev/null; wait $!
echo "sizes seen while the screen was resized: $(ffprobe -v error -select_streams v -show_entries frame=width,height -of csv=p=0 /tmp/c.flv | sort | uniq -c | tr '\n' ';')"
for i in $(seq 1 30); do python3 wsclient.py '{"password":"","sound":"on"}' 0.5 /tmp/d.flv >/dev/null; done
echo "after 30 more sessions: $(awk '/VmRSS/ { print $2 }' /proc/$(pidof remote-desktop)/status) kB, $(ls /proc/$(pidof remote-desktop)/fd | wc -l) open files, $(ls /proc/$(pidof remote-desktop)/task | wc -l) threads"
sort /tmp/host.log | uniq -c | cut -c1-150
