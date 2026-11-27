#!/bin/sh
# X11: a client that asks for a screen of its window's size gets the X screen at that size, and the screen is
# as it was once the client has gone; a window larger than the server's screen can be is shown the screen as
# it is. Argument: xorg (a dummy display) or xvfb.
case "$1" in
xorg) Xorg :1 -seat seat1 -config ./xorg-two.conf -noreset -logfile /tmp/xorg.log >/tmp/x.out 2>&1 & ;;
xvfb) Xvfb :1 -screen 0 1920x1080x24 >/tmp/x.log 2>&1 & ;;
esac
sleep 3
state() { echo "$(xrandr | sed -n 's/.*current \([0-9]*\) x \([0-9]*\),.*/\1x\2/p'), $(xrandr | grep -c remote-desktop) modes made"; }
echo "before: $(state)"
./remote-desktop password --none >/dev/null
./remote-desktop --listen 127.0.0.1:8765 >/tmp/host.log 2>&1 &
sleep 2
session() {  # the client's window; prints the screen during the session and after it, and the picture's size
  (sleep 3; echo "during: $(state)") &
  python3 wsclient.py "{\"password\":\"\",\"size\":\"client\",\"window\":$1,\"bitrate\":8000}" 5 /tmp/c.flv >/dev/null; wait $!
  sleep 1
  echo "after: $(state)"
  echo "picture: $(ffprobe -v error -select_streams v -show_entries frame=width,height -of csv=p=0 /tmp/c.flv | uniq | tr '\n' ' ')"
}
echo "a window of 1201x801 at two pixels each:"; session "[1201,801,2]"
echo "a window of 5000x3000:"; session "[5000,3000,1]"
sort /tmp/host.log | uniq -c | cut -c1-170
