#!/bin/sh
# A headless X server with a terminal on it, served to the container's network. The pointer starts on the desktop.
mkdir -p $XDG_RUNTIME_DIR && chmod 700 $XDG_RUNTIME_DIR
Xvfb :1 -screen 0 1280x720x24 >/tmp/x.log 2>&1 &
sleep 2
export DISPLAY=:1
xsetroot -solid "#203040" -cursor_name left_ptr
xterm -geometry 80x24+100+100 -e sh -c "ls -la /usr/bin | head -20; sleep 100000" &
sleep 1
xdotool mousemove 1000 600
./remote-desktop password --none >/dev/null
exec ./remote-desktop --listen 0.0.0.0:8765
