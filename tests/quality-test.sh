#!/bin/sh
# Shows a detailed still picture on a headless sway output and measures how close the stream gets to it.
# Arguments: bitrate in kbit/s, seconds to watch, extra login settings as JSON members (e.g. '"chroma":"444"').
mkdir -p $XDG_RUNTIME_DIR && chmod 700 $XDG_RUNTIME_DIR
ffmpeg -v error -y -f lavfi -i "testsrc2=size=1920x1080,noise=alls=12:allf=u" -frames:v 1 /tmp/picture.png
printf 'output * resolution 1920x1080 bg /tmp/picture.png fill\n' > /tmp/sway.conf
WLR_BACKENDS=headless WLR_RENDERER=pixman WLR_LIBINPUT_NO_DEVICES=1 sway -c /tmp/sway.conf >/tmp/sway.log 2>&1 &
sleep 3
export WAYLAND_DISPLAY=wayland-1
./remote-desktop password --none >/dev/null
./remote-desktop --listen 127.0.0.1:8765 >/tmp/host.log 2>&1 &
sleep 2
grim /tmp/truth.png
export CAFILE=$HOME/.config/remote-desktop/certificate.pem NAME=localhost
python3 wsclient.py "{\"password\":\"\",\"bitrate\":$1,$3\"fps\":\"60\"}" $2 /tmp/out.flv
grep encoding /tmp/host.log
# the last frame of the stream against the screenshot
ffmpeg -v error -y -i /tmp/out.flv -update 1 /tmp/last.png
ffmpeg -v info -i /tmp/last.png -i /tmp/truth.png -lavfi "[0:v]format=gbrp[a];[1:v]format=gbrp[b];[a][b]psnr" -f null - 2>&1 | grep -o "PSNR.*"
ffprobe -v error -select_streams v -show_entries packet=size -of csv=p=0 /tmp/out.flv | awk '{ n++; total += $1; if (n <= 3) first = first " " $1 } END { print n " frames, " int(total / 1024) " kB, first sizes:" first }'
