#!/bin/sh
# Plays a video on a headless desktop and watches it through the real client in a browser, over a link of a
# given speed. Prints what the client shows second by second, then what the host did. The picture the client
# shows at the end is saved as video-shot.jpg.
# Arguments: full or window (see serve-video.sh), the link in Mbit/s (0 = unshaped), seconds.
cd "$(dirname "$0")"
kind=$1; link=${2:-0}; seconds=${3:-20}
docker rm -f rd-rate-host rd-rate-client >/dev/null 2>&1
docker network inspect rd-rate >/dev/null 2>&1 || docker network create rd-rate >/dev/null
docker run -d --name rd-rate-host --network rd-rate --cap-add SYS_NICE --device /dev/uinput -v "$PWD":/test rd-test-sway ./serve-video.sh $kind >/dev/null || exit 1
[ "$link" != 0 ] && docker run --rm --network container:rd-rate-host --cap-add NET_ADMIN rd-test-net tc qdisc replace dev eth0 root tbf rate ${link}mbit burst 32kb latency 100ms
# The first run makes the video, which takes a while; the host listens once the desktop is up.
until docker exec rd-rate-host python3 -c "import socket; socket.create_connection(('127.0.0.1', 8765), 1)" 2>/dev/null; do sleep 1; done
sleep 3
ticks() { docker exec rd-rate-host sh -c 'awk "{ print \$14 + \$15 }" /proc/$(pidof remote-desktop)/stat'; }
before=$(ticks)
docker run --rm --name rd-rate-client --network rd-rate -e CHROME=/usr/bin/chromium -e CHROME_FLAGS="--no-sandbox --disable-gpu" -e SHOT=/test/video-shot.jpg -v "$PWD":/test -w /test rd-test-chrome \
  node page-check.mjs https://rd-rate-host:8765/ $seconds 2>&1 | grep -v "^page:\|^connect:" | sed -E "s/ \| (Auto|no screen choice)[^|]*//g; s/SLOW: This connection carries about ([0-9.]+) Mbit.s[^|]*/slow link notice/" | cut -c1-150
echo "host process used $(( ($(ticks) - before) / seconds ))% of one core"
docker logs rd-rate-host 2>&1 | grep -v "^$" | sort | uniq -c | cut -c1-120
docker rm -f rd-rate-host >/dev/null 2>&1
