#!/bin/sh
# Two screens of different sizes (where the desktop can make them so) on one headless desktop: the host has to
# list both, show the first by default, the second when asked by name, and the first for a name no screen has.
# Argument: sway, mutter, kwin or x11.
mkdir -p $XDG_RUNTIME_DIR && chmod 700 $XDG_RUNTIME_DIR
terminal='printf "\033[41m  red  \033[42m green \033[44m blue  \033[0m\n"; ls -la /usr/bin | head -30; sleep 1000'
session() {  # the settings to log in with; prints what the host listed and the size of the picture it sent
  python3 wsclient.py "{\"password\":\"\",$1\"bitrate\":8000}" 3 /tmp/out.flv | grep "^screens"
  ffprobe -v error -select_streams v -show_entries stream=width,height -of csv=p=0 /tmp/out.flv | sed "s/^/  picture: /"
  ffprobe -v error -select_streams v -show_entries packet=size -of csv=p=0 /tmp/out.flv | head -1 | sed "s/^/  first frame, bytes: /"
}
run() {
  ./remote-desktop password --none >/dev/null
  ./remote-desktop --listen 127.0.0.1:8765 >/tmp/host.log 2>&1 &
  sleep 2
  export CAFILE=$HOME/.config/remote-desktop/certificate.pem NAME=localhost
  echo "no screen asked for:"; session "" | tee /tmp/first.txt
  second=$(sed -n "s/^screens: \(.*\), showing .*/\1/p" /tmp/first.txt | python3 -c "import json, sys; print(json.load(sys.stdin)[1])")
  echo "asked for the second, $second:"; session "\"screen\":\"$second\","
  echo "asked for one that is not there:"; session "\"screen\":\"nowhere\","
  grep -v "^$" /tmp/host.log | sort | uniq -c
}
case $1 in
sway)
  printf 'output HEADLESS-1 resolution 1920x1080 position 0 0\nexec foot -F sh -c %s\n' "'$terminal'" > /tmp/sway.conf
  WLR_BACKENDS=headless WLR_RENDERER=pixman WLR_LIBINPUT_NO_DEVICES=1 sway -c /tmp/sway.conf >/tmp/sway.log 2>&1 &
  sleep 3
  export WAYLAND_DISPLAY=wayland-1 SWAYSOCK=$(ls $XDG_RUNTIME_DIR/sway-ipc.*.sock)
  swaymsg create_output >/dev/null && swaymsg output HEADLESS-2 resolution 1280x720 position 1920 0 >/dev/null
  sleep 1
  run ;;
mutter|kwin)
  dbus-uuidgen --ensure; mkdir -p /run/dbus; dbus-daemon --system --fork 2>/dev/null
  export DESKTOP=$1 TERMINAL=$terminal
  exec dbus-run-session -- sh -c '
    pipewire >/tmp/pipewire.log 2>&1 & sleep 1; wireplumber >/tmp/wireplumber.log 2>&1 &
    sleep 2
    if [ $DESKTOP = mutter ]; then
      mutter --headless --wayland --virtual-monitor 1920x1080 --virtual-monitor 1280x720 >/tmp/compositor.log 2>&1 &
    else
      kwin_wayland --virtual --width 1920 --height 1080 --output-count 2 --no-lockscreen --socket wayland-0 >/tmp/compositor.log 2>&1 &
    fi
    sleep 5
    export WAYLAND_DISPLAY=wayland-0
    foot -F sh -c "$TERMINAL" >/tmp/foot.log 2>&1 &
    sleep 2
    . ./screens-test.sh functions
    run
  ' ;;
x11)
  # A real X server on a dummy display: unlike Xvfb it takes monitors that are parts of its one screen. The
  # display itself stays listed as a monitor too, so there are three: all of it, the left part and the right.
  Xorg :1 -seat seat1 -config ./xorg-two.conf -noreset -logfile /tmp/xorg.log >/tmp/x.out 2>&1 &
  sleep 3
  export DISPLAY=:1
  xrandr --setmonitor left 1920/508x1080/286+0+0 none && xrandr --setmonitor right 1280/339x720/190+1920+0 none
  xsetroot -solid "#203040" -cursor_name left_ptr
  xterm -geometry 120x40+100+100 -e sh -c "$terminal" &
  sleep 1
  xdotool mousemove 2000 300
  run
  echo "asked for the right one, with the pointer painted, which is at 80,300 of it:"; session '"screen":"right","cursor":"yes",'
  ffmpeg -v error -y -i /tmp/out.flv -update 1 -vf "crop=48:48:70:290" -f rawvideo -pix_fmt rgb24 /tmp/crop.rgb
  python3 -c "
data = open('/tmp/crop.rgb', 'rb').read()
colours = {data[i:i + 3] for i in range(0, len(data), 3)}
print('  the pointer is in the picture where it should be' if len(colours) > 4 else '  NO pointer in the picture there')" ;;
esac
