#!/bin/sh
# Starts the desktop and audio server, plays a tone into a null sink and records what a client receives.
mkdir -p $XDG_RUNTIME_DIR && chmod 700 $XDG_RUNTIME_DIR
exec dbus-run-session -- sh -c '
  pipewire >/tmp/pipewire.log 2>&1 & sleep 1; wireplumber >/tmp/wireplumber.log 2>&1 & pipewire-pulse >/tmp/pulse.log 2>&1 &
  sleep 3
  pactl load-module module-null-sink sink_name=test >/dev/null
  pactl set-default-sink test; pactl info | grep -E "Server Name|Default Sink"
  WLR_BACKENDS=headless WLR_RENDERER=pixman WLR_LIBINPUT_NO_DEVICES=1 sway >/tmp/sway.log 2>&1 & sleep 3
  export WAYLAND_DISPLAY=wayland-1
  ./remote-desktop password --none >/dev/null
  ./remote-desktop --listen 127.0.0.1:8765 >/tmp/host.log 2>&1 & sleep 2
  sox -n -r 48000 -c 2 /tmp/tone.wav synth 8 sine 440 sine 880 vol 0.5
  export CAFILE=$HOME/.config/remote-desktop/certificate.pem NAME=localhost
  # two seconds of silence, then the tone: left 440 Hz, right 880 Hz
  (sleep 2; paplay /tmp/tone.wav) &
  python3 wsclient.py "{\"password\":\"\",\"sound\":\"on\"}" 7 /tmp/out.flv
  python3 opus2ogg.py /tmp/out.flv.opus /tmp/out.ogg
  ffmpeg -v error -y -i /tmp/out.ogg -c:a pcm_s16le /tmp/out.wav && python3 pitch.py /tmp/out.wav
  echo "--- host log"; cat /tmp/host.log; pactl list short source-outputs | sed "s/^/still recording: /"
'
