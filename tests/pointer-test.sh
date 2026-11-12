#!/bin/sh
# The pointer's shape has to reach a client that draws the pointer itself, again when it changes, and not at
# all when the host paints the pointer into the picture. Argument: x11 or mutter.
mkdir -p $XDG_RUNTIME_DIR && chmod 700 $XDG_RUNTIME_DIR
watch() {  # login settings; prints the shapes the client is sent while the pointer is moved between two places
  python3 wsclient.py "{\"password\":\"\",$1\"bitrate\":8000}" 6 /tmp/out.flv > /tmp/client.txt &
  client=$!
  sleep 2; "$2"; sleep 1; "$3"; sleep 1
  wait $client
  grep -c "^pointer shape" /tmp/client.txt | sed "s/^/  shapes sent: /"; grep "^pointer shape" /tmp/client.txt | sort | uniq -c | sed "s/^/  /"
}
serve() {
  ./remote-desktop password --none >/dev/null
  ./remote-desktop --listen 127.0.0.1:8765 >/tmp/host.log 2>&1 &
  sleep 2
  export CAFILE=$HOME/.config/remote-desktop/certificate.pem NAME=localhost
}
case $1 in
x11)
  Xvfb :1 -screen 0 1280x720x24 >/tmp/x.log 2>&1 &
  sleep 2
  export DISPLAY=:1
  xsetroot -solid "#203040" -cursor_name left_ptr
  xterm -geometry 80x24+100+100 -e sh -c "ls -la /usr/bin | head -20; sleep 1000" &
  sleep 1
  xdotool mousemove 1000 600
  serve
  over_text() { xdotool mousemove 300 250; }
  over_desktop() { xdotool mousemove 1000 600; }
  echo "the client draws the pointer; it moves onto the terminal's text and back:"; watch "" over_text over_desktop
  echo "the host paints the pointer:"; watch '"cursor":"yes",' over_text over_desktop ;;
mutter)
  dbus-uuidgen --ensure; mkdir -p /run/dbus; dbus-daemon --system --fork 2>/dev/null
  exec dbus-run-session -- sh -c '
    pipewire >/tmp/pipewire.log 2>&1 & sleep 1; wireplumber >/tmp/wireplumber.log 2>&1 &
    sleep 2
    mutter --headless --wayland --virtual-monitor 1280x720 >/tmp/compositor.log 2>&1 &
    sleep 5
    export WAYLAND_DISPLAY=wayland-0
    foot -F sh -c "ls -la /usr/bin | head -20; sleep 1000" >/tmp/foot.log 2>&1 &
    sleep 2
    . ./pointer-test.sh functions
    serve
    # A headless Mutter has no pointer until something moves one, which mutter-pointer.py does all the while.
    python3 mutter-pointer.py 20 &
    idle() { :; }
    echo "the client draws the pointer:"; watch "" idle idle
    echo "the host paints the pointer:"; watch "\"cursor\":\"yes\"," idle idle
    grep -v "^$" /tmp/host.log | sort | uniq -c
  ' ;;
esac
