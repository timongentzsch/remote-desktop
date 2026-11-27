"""Test client: log in, optionally send input messages, record the video stream for a few seconds."""
import json, os, ssl, struct, sys, time

from websocket_client import WebSocket, video_frame

password, seconds, out = sys.argv[1], float(sys.argv[2]), sys.argv[3]
messages = [json.loads(m) for m in sys.argv[4:]]

host = os.environ.get("HOST", "127.0.0.1")
tls = ssl.create_default_context(cafile=os.path.expanduser("~/.config/remote-desktop/certificate.pem"))
ws = WebSocket(host, int(os.environ.get("PORT", "8765")), tls, os.environ.get("NAME", os.environ.get("HOST", "localhost")))
print(ws.status)
start = time.time()
ws.send(password if password.startswith("{") else json.dumps({"password": password}))
first = None
arrivals = []
flv = open(out, "wb")
sound = open(out + ".opus", "wb")
packets = 0
stats = []
sent = 0
received, rtt, last_ping = 0, 0.0, 0.0
while time.time() - start < seconds:
    if time.time() - last_ping >= 0.1:  # as a browser does: the host sets the bitrate by what these report
        last_ping = time.time()
        ws.send(json.dumps(["ping", last_ping, round(rtt * 1000), received]))
    # send queued input messages spread out after the first second
    if sent < len(messages) and time.time() - start > 1 + sent * 0.05:
        ws.send(json.dumps(messages[sent]))
        sent += 1
    opcode, data = ws.receive(0.05)
    if opcode == 8:
        print("server closed after %.2fs" % (time.time() - start))
        break
    received += len(data)
    if opcode == 2 and data:
        if data[0] == 0:  # padding used to measure the link
            continue
        if data[0] == 7:  # the pointer's shape: sizes, tip, then red, green, blue, alpha
            width, height, tip_x, tip_y = struct.unpack_from(">4H", data, 1)
            pixels = data[9:]
            opaque = sum(alpha > 127 for alpha in pixels[3::4])
            print("pointer shape: %dx%d, tip at %d,%d, %d of %d pixels opaque" % (width, height, tip_x, tip_y, opaque, len(pixels) // 4))
            continue
        arrivals.append(time.time())
        if data[0] == 8:  # one Opus packet, saved with a 2-byte length in front
            sound.write(struct.pack(">H", len(data) - 1) + data[1:])
            packets += 1
        else:
            if first is None and video_frame(data):
                first = time.time() - start
            flv.write(data)
    elif opcode == 1:
        kind, *values = json.loads(data)
        if kind == "ping":
            rtt = time.time() - values[0]
        elif kind == "stats":
            stats.append(values[0])
        elif kind == "screens":
            names, shown = values
            print("screens: %s, showing %s" % (json.dumps(names), shown))
        elif kind == "clipboard":
            print("clipboard text from the host: %r" % values[0][:80])
flv.close()
if stats:
    print("host encode ms per frame, sampled each second: " + " ".join("%.1f" % encode for encode in stats))
sound.close()
print("%d sound packets, %d bytes" % (packets, os.path.getsize(out + ".opus")))
gaps = sorted(b - a for a, b in zip(arrivals, arrivals[1:]))
print("first video after %s s, %d ws messages, %d bytes" % (first and round(first, 2), len(arrivals), os.path.getsize(out)))
if gaps:
    print("gap between messages ms: median %.1f p99 %.1f max %.1f" % (gaps[len(gaps) // 2] * 1e3, gaps[int(len(gaps) * .99)] * 1e3, gaps[-1] * 1e3))
