#!/bin/sh
# Runs the host on a headless X server and records what a client receives.
# Arguments: seconds to watch, extra login settings as JSON members, "motion" for fast-changing text.
mkdir -p $XDG_RUNTIME_DIR && chmod 700 $XDG_RUNTIME_DIR
Xvfb :1 -screen 0 1920x1080x24 >/tmp/x.log 2>&1 &
sleep 2
xsetroot -solid "#203040"
if [ "$3" = motion ]; then
  xterm -geometry 230x70+0+0 -e sh -c 'while :; do head -c 3000 /dev/urandom | base64 -w 0; done' &
else
  xterm -geometry 120x40+100+100 -e sh -c 'printf "\033[41m  red  \033[42m green \033[44m blue  \033[0m\n"; ls -la /usr/bin | head -30; sleep 100' &
fi
sleep 1
xdotool mousemove 400 300
./remote-desktop password --none >/dev/null
./remote-desktop --listen 127.0.0.1:8765 >/tmp/host.log 2>&1 &
sleep 2
export CAFILE=$HOME/.config/remote-desktop/certificate.pem NAME=localhost
ticks() { awk '{ print $14 + $15 }' /proc/$(pidof remote-desktop)/stat; }
before=$(ticks)
python3 wsclient.py "{\"password\":\"\",$2\"bitrate\":8000}" $1 /tmp/out.flv
echo "host process used $(( ($(ticks) - before) / $1 ))% of one core"
cat /tmp/host.log
ffprobe -v error -select_streams v -show_entries stream=width,height,pix_fmt -of csv=p=0 /tmp/out.flv
ffprobe -v error -select_streams v -show_entries packet=size -of csv=p=0 /tmp/out.flv | awk -v seconds=$1 '{ n++; total += $1 } END { printf "%d frames (%.0f fps), %.0f kbit/s\n", n, n / seconds, total * 8 / 1000 / seconds }'
