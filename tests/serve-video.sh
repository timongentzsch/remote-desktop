#!/bin/sh
# Headless sway playing a video with sound, served to the container's network. Argument: full (the video fills
# the screen at 60 frames a second) or window (a 720p window of it at 30 over a terminal).
mkdir -p $XDG_RUNTIME_DIR && chmod 700 $XDG_RUNTIME_DIR
clip=/test/video-$1.mkv
if [ ! -s $clip ]; then  # made once: a zoom into a fractal, detailed and moving all over, with a tone
  [ "$1" = full ] && size=1920x1080:rate=60 || size=1280x720:rate=30
  ffmpeg -v error -y -f lavfi -i "mandelbrot=size=$size" -f lavfi -i "sine=frequency=440:sample_rate=48000" -t 20 \
    -c:v libx264 -preset ultrafast -crf 14 -pix_fmt yuv420p -c:a libopus -ac 2 $clip
fi
[ "$1" = full ] && player="mpv --really-quiet --vo=wlshm --loop --fs $clip" || player="mpv --really-quiet --vo=wlshm --loop --geometry=1280x720 $clip"
exec dbus-run-session -- sh -c "
  pipewire >/tmp/pipewire.log 2>&1 & sleep 1; wireplumber >/tmp/wireplumber.log 2>&1 & pipewire-pulse >/tmp/pulse.log 2>&1 &
  sleep 3
  pactl load-module module-null-sink sink_name=test >/dev/null; pactl set-default-sink test
  printf 'output * resolution 1920x1080\nfor_window [app_id=\"mpv\"] floating enable\nexec foot sh -c \"ls -la /usr/bin; sleep 100000\"\n' > /tmp/sway.conf
  WLR_BACKENDS=headless WLR_RENDERER=pixman WLR_LIBINPUT_NO_DEVICES=1 sway -c /tmp/sway.conf >/tmp/sway.log 2>&1 & sleep 3
  export WAYLAND_DISPLAY=wayland-1
  $player >/tmp/mpv.log 2>&1 &
  ./remote-desktop password --none >/dev/null
  exec ./remote-desktop --listen 0.0.0.0:8765
"
