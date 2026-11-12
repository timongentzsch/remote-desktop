#!/bin/sh
# Watches a full-screen video at 60 frames a second through the real client, over a link that delays and loses
# packets, and prints how late its frames were drawn (see late-check.mjs).
# Arguments: the congestion control the host's system starts connections on (cubic or bbr), round trip in ms,
# loss in percent, seconds, and by how many ms the delay swings (default 0). With DETAIL set in the
# environment, the frames are also told second by second; with TCP set, the browser has no UDP to take; SET
# holds JavaScript that chooses settings in the page before the session begins.
cd "$(dirname "$0")"
cc=$1; rtt=$2; loss=$3; seconds=${4:-20}; swing=${5:-0}
clean() { docker rm -f rd-rate-host rd-rate-client >/dev/null 2>&1; }
clean
docker network inspect rd-rate >/dev/null 2>&1 || docker network create rd-rate >/dev/null
docker run -d --name rd-rate-host --network rd-rate --cap-add SYS_NICE --device /dev/uinput \
  --sysctl net.ipv4.tcp_congestion_control=$cc -v "$PWD":/test rd-test-sway ./serve-video.sh full >/dev/null || exit 1
docker run -d --name rd-rate-client --network rd-rate -e CHROME=/usr/bin/chromium -e CHROME_FLAGS="--no-sandbox --disable-gpu" \
  -v "$PWD":/test -w /test rd-test-chrome sleep 1000 >/dev/null || exit 1
# Each end delays and loses what it sends. Packets go out one by one for this: handed over as one large
# segment, a whole burst would be lost or kept together. The queue behind the delay keeps them in order where
# the delay swings.
for end in rd-rate-host rd-rate-client; do
  docker run --rm --network container:$end --cap-add NET_ADMIN rd-test-net sh -c \
    "ethtool -K eth0 tso off gso off >/dev/null && tc qdisc replace dev eth0 root handle 1: netem delay $((rtt / 2))ms ${swing}ms loss $loss% limit 10000 && tc qdisc add dev eth0 parent 1:1 pfifo limit 10000" || exit 1
done
until docker exec rd-rate-host python3 -c "import socket; socket.create_connection(('127.0.0.1', 8765), 1)" 2>/dev/null; do sleep 1; done
sleep 3
printf '%-5s round trip %3s ms%s, loss %4s%%: ' $cc $rtt "${5:+ swinging by $swing}" $loss
# Without WebTransport in the page, video stays on TCP.
docker exec ${DETAIL:+-e DETAIL=1} -e SET="${TCP:+window.WebTransport = undefined; }$SET" rd-rate-client node late-check.mjs https://rd-rate-host:8765/ $seconds
clean
