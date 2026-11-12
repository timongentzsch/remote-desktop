#!/bin/sh
# Fills a headless sway output with fast-changing text and reports what the stream costs.
# Arguments: bitrate in kbit/s, seconds to watch, extra login settings as JSON members.
mkdir -p $XDG_RUNTIME_DIR && chmod 700 $XDG_RUNTIME_DIR
printf 'output * resolution 1920x1080\nexec foot -F sh -c "while :; do head -c 3000 /dev/urandom | base64 -w 0; done"\n' > /tmp/sway.conf
WLR_BACKENDS=headless WLR_RENDERER=pixman WLR_LIBINPUT_NO_DEVICES=1 sway -c /tmp/sway.conf >/tmp/sway.log 2>&1 &
sleep 3
export WAYLAND_DISPLAY=wayland-1
./remote-desktop password --none >/dev/null
./remote-desktop --listen 127.0.0.1:8765 >/tmp/host.log 2>&1 &
sleep 2
export CAFILE=$HOME/.config/remote-desktop/certificate.pem NAME=localhost
ticks() { awk '{ print $14 + $15 }' /proc/$(pidof remote-desktop)/stat; }
before=$(ticks)
python3 wsclient.py "{\"password\":\"\",\"bitrate\":$1,$3\"fps\":\"60\"}" $2 /tmp/out.flv | grep "encode ms\|gap"
echo "host process used $(( ($(ticks) - before) / $2 ))% of one core"
grep encoding /tmp/host.log
ffprobe -v error -select_streams v -show_entries packet=size -of csv=p=0 /tmp/out.flv | awk -v seconds=$2 -v cap=$1 '{ n++; total += $1; if ($1 > biggest) biggest = $1 } END { printf "%d frames (%.0f fps), %.0f kbit/s of %d allowed, biggest frame %d kB\n", n, n / seconds, total * 8 / 1000 / seconds, cap, biggest / 1024 }'
ffmpeg -v error -y -sseof -0.3 -i /tmp/out.flv -update 1 /tmp/last.png && echo "the stream decodes to the end"
