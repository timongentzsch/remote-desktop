#!/bin/sh
# Runs a session over a shaped link and prints what the bitrate did, second by second.
# Arguments: congestion control of the host (cubic or bbr), round trip in ms, moving or still,
# then the link's phases as "seconds:Mbit/s ..." (default "20:50 30:2 30:50"), then the queue's length in ms
# that the slow link buffers before it drops (default 100), then login settings as JSON. With RELAY=1 in the
# environment a relay stands between the two, and it is the relay's way to the client that is shaped.
cd "$(dirname "$0")"
cc=$1; rtt=$2; content=$3; phases=${4:-20:50 30:2 30:50}; queue=${5:-100}; login=${6:-{\}}
clean() { docker rm -f rd-rate-host rd-rate-client rd-rate-relay >/dev/null 2>&1; }
clean
docker network inspect rd-rate >/dev/null 2>&1 || docker network create rd-rate >/dev/null
tc_at() { where=$1; shift; docker run --rm --network container:$where --cap-add NET_ADMIN rd-test-net tc qdisc replace dev eth0 root "$@"; }
docker run -d --name rd-rate-host --network rd-rate --cap-add SYS_NICE --device /dev/uinput \
  --sysctl net.ipv4.tcp_congestion_control=$cc -v "$PWD":/test rd-test-sway ./serve-rate.sh $content >/dev/null || exit 1
shaped=rd-rate-host
if [ -n "$RELAY" ]; then
  shaped=rd-rate-relay
  docker run -d --name rd-rate-relay --network rd-rate -v "$PWD":/test -w /test rd-test-net python3 relay.py rd-rate-host >/dev/null
fi
total=0; for phase in $phases; do total=$((total + ${phase%%:*})); done
first=${phases%% *}
tc_at $shaped tbf rate ${first##*:}mbit burst 32kb latency ${queue}ms
sleep 7
echo "${RELAY:+behind a relay, }host: $(docker exec rd-rate-host cat /proc/sys/net/ipv4/tcp_congestion_control), round trip $rtt ms, $content screen, link $phases (s:Mbit/s), queue $queue ms"
docker run -d --name rd-rate-client --network rd-rate --cap-add NET_ADMIN -v "$PWD":/test -w /test rd-test-net \
  sh -c "tc qdisc replace dev eth0 root netem delay ${rtt}ms limit 100000; exec python3 rateclient.py $shaped $total '$login'" >/dev/null
at=0
for phase in $phases; do
  [ $at -gt 0 ] && tc_at $shaped tbf rate ${phase##*:}mbit burst 32kb latency ${queue}ms && echo "link ${phase##*:} Mbit/s from second $at"
  sleep ${phase%%:*}; at=$((at + ${phase%%:*}))
done
sleep 2
docker logs rd-rate-client 2>&1
docker logs rd-rate-host 2>&1 | grep -v "^$" | tail -3
clean
