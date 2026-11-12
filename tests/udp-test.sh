#!/bin/sh
# Watches a video through the real client while UDP between the two is cut off for a while and comes back: the
# video should go on over TCP meanwhile, and return to UDP after. Prints what the client shows second by second;
# the way the video takes is at the end of its bitrate.
# Arguments: the second at which UDP is cut off (default 8), and the one at which it comes back (default 18).
# GPU in the environment holds Docker's options that give the host a GPU to encode on.
cd "$(dirname "$0")"
off=${1:-8}; on=${2:-18}
clean() { docker rm -f rd-rate-host rd-rate-client >/dev/null 2>&1; }
clean
docker network inspect rd-rate >/dev/null 2>&1 || docker network create rd-rate >/dev/null
docker run -d --name rd-rate-host --network rd-rate --cap-add SYS_NICE --device /dev/uinput $GPU -v "$PWD":/test rd-test-sway ./serve-video.sh full >/dev/null || exit 1
until docker exec rd-rate-host python3 -c "import socket; socket.create_connection(('127.0.0.1', 8765), 1)" 2>/dev/null; do sleep 1; done
sleep 3
docker run -d --name rd-rate-client --network rd-rate -e CHROME=/usr/bin/chromium -e CHROME_FLAGS="--no-sandbox --disable-gpu" \
  -v "$PWD":/test -w /test rd-test-chrome node page-check.mjs https://rd-rate-host:8765/ $((on + 10)) >/dev/null || exit 1
# The client's end drops every UDP packet it would send: the host hears nothing more from it that way.
at_client() { docker run --rm --network container:rd-rate-client --cap-add NET_ADMIN rd-test-net sh -c "$1"; }
sleep $((off + 2))
at_client "tc qdisc add dev eth0 root handle 1: prio && tc filter add dev eth0 parent 1: protocol ip u32 match ip protocol 17 0xff action drop" || exit 1
sleep $((on - off))
at_client "tc qdisc del dev eth0 root"
docker wait rd-rate-client >/dev/null
docker logs rd-rate-client 2>&1 | grep -v "^page:\|^connect:" | sed -E "s/ \| (Auto|no screen choice)[^|]*//g; s/ \| sound.*//" | cut -c1-110
docker logs rd-rate-host 2>&1 | grep -m1 encoding
clean
