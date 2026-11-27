#!/bin/sh
# Runs the host on a headless compositor that casts its screen, GNOME's or KDE's, and records what a client
# receives. Arguments: mutter or kwin, seconds to watch, extra login settings as JSON members, "motion" for
# fast-changing text.
dbus-uuidgen --ensure; mkdir -p /run/dbus; dbus-daemon --system --fork
export KIND=$1 SECONDS_TO_WATCH=$2 EXTRA=$3 CONTENT=$4
exec dbus-run-session -- sh -c '
  pipewire >/tmp/pipewire.log 2>&1 & sleep 1; wireplumber >/tmp/wireplumber.log 2>&1 &
  sleep 2
  if [ "$KIND" = kwin ]; then
    kwin_wayland --virtual --width 1920 --height 1080 --no-lockscreen --socket wayland-0 >/tmp/compositor.log 2>&1 &
  else
    mutter --headless --wayland --virtual-monitor 1920x1080 >/tmp/compositor.log 2>&1 &
  fi
  sleep 5
  export WAYLAND_DISPLAY=wayland-0
  if [ "$CONTENT" = motion ]; then
    foot -F sh -c "while :; do head -c 3000 /dev/urandom | base64 -w 0; done" >/tmp/foot.log 2>&1 &
  else
    foot -F sh -c "printf \"\\033[41m  red  \\033[42m green \\033[44m blue  \\033[0m\\n\"; ls -la /usr/bin | head -40; sleep 100" >/tmp/foot.log 2>&1 &
  fi
  sleep 2
  ./remote-desktop password --none >/dev/null
  ./remote-desktop --listen 127.0.0.1:8765 >/tmp/host.log 2>&1 &
  sleep 2
  ticks() { awk "{ print \$14 + \$15 }" /proc/$(pidof remote-desktop)/stat; }
  before=$(ticks)
  python3 wsclient.py "{\"password\":\"\",$EXTRA\"bitrate\":8000}" $SECONDS_TO_WATCH /tmp/out.flv
  echo "host process used $(( ($(ticks) - before) / SECONDS_TO_WATCH ))% of one core"
  cat /tmp/host.log
  ffprobe -v error -select_streams v -show_entries stream=width,height,pix_fmt -of csv=p=0 /tmp/out.flv
  ffprobe -v error -select_streams v -show_entries packet=size -of csv=p=0 /tmp/out.flv | awk -v seconds=$SECONDS_TO_WATCH "{ n++; total += \$1 } END { printf \"%d frames (%.0f fps), %.0f kbit/s\\n\", n, n / seconds, total * 8 / 1000 / seconds }"
  tail -3 /tmp/compositor.log | cut -c1-160
'
