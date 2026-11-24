"""Plays a browser's part for the bitrate controller: logs in, pings ten times a second with the last round
trip and what has arrived, and prints second by second what the host allowed and what arrived.
Arguments: host, seconds, login settings as JSON (optional)."""
import json, sys, time

from websocket_client import WebSocket, test_tls, video_frame

host, seconds = sys.argv[1], float(sys.argv[2])
login = {"password": "", "fps": "60", **(json.loads(sys.argv[3]) if len(sys.argv) > 3 else {})}

ws = WebSocket(host, 8765, test_tls())
start = time.time()
ws.send(json.dumps(login))

allowed, held, rtt, last_ping, received = 0, False, 0.0, 0.0, 0
video = padding = frames = 0
last_frame, gap, second = start, 0.0, 1
print("   s  allowed  video  padding  fps  gap ms  rtt ms  (kbit/s; * = the host says the link holds the rate back)")
while True:
    now = time.time()
    if now - start >= second:
        print("%4d %8d %6d %8d %4d %7.0f %7.0f %s" % (second, allowed, video * 8 // 1000, padding * 8 // 1000, frames, gap * 1000, rtt * 1000, "*" if held else ""), flush=True)
        video = padding = frames = 0
        gap = 0.0
        second += 1
        if second > seconds:
            break
    if now - last_ping >= 0.1:
        last_ping = now
        ws.send(json.dumps(["ping", now, round(rtt * 1000), received]))
    opcode, data = ws.receive(0.02)
    if opcode == 8:
        print("the host closed the connection after %.1f s" % (time.time() - start))
        break
    received += len(data)
    if opcode == 1:
        message = json.loads(data)
        if message[0] == "ping":
            rtt = time.time() - message[1]
        elif message[0] == "stats":
            allowed, held = message[3], message[4]
    elif opcode == 2 and data:
        if data[0] == 0:
            padding += len(data)
        elif data[0] not in (7, 8):
            video += len(data)
            if video_frame(data):
                frames += 1
                arrived = time.time()
                gap, last_frame = max(gap, arrived - last_frame), arrived
