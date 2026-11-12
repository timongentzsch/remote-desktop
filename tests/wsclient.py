"""Test client: log in, optionally send input messages, record the video stream for a few seconds."""
import base64, json, os, socket, struct, sys, time

password, seconds, out = sys.argv[1], float(sys.argv[2]), sys.argv[3]
messages = [json.loads(m) for m in sys.argv[4:]]


def frame(payload, opcode=1):
    mask = os.urandom(4)
    return bytes([0x80 | opcode, 0x80 | len(payload)]) + mask + bytes(b ^ mask[i % 4] for i, b in enumerate(payload))


s = socket.create_connection((os.environ.get("HOST", "127.0.0.1"), int(os.environ.get("PORT", "8765"))))
if os.environ.get("CAFILE"):  # TLS, verifying the server against this certificate
    import ssl
    s = ssl.create_default_context(cafile=os.environ["CAFILE"]).wrap_socket(s, server_hostname=os.environ.get("NAME", os.environ.get("HOST", "localhost")))
s.sendall(b"GET /ws HTTP/1.1\r\nHost: x\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n"
          b"Sec-WebSocket-Key: " + base64.b64encode(os.urandom(16)) + b"\r\nSec-WebSocket-Version: 13\r\n\r\n")
buf = b""
while b"\r\n\r\n" not in buf:
    buf += s.recv(4096)
head, buf = buf.split(b"\r\n\r\n", 1)
print(head.split(b"\r\n")[0].decode())
start = time.time()
s.sendall(frame(password.encode() if password.startswith("{") else json.dumps({"password": password}).encode()))
s.settimeout(0.05)
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
        s.sendall(frame(json.dumps(["ping", last_ping, round(rtt * 1000), received]).encode()))
    # send queued input messages spread out after the first second
    if sent < len(messages) and time.time() - start > 1 + sent * 0.05:
        s.sendall(frame(json.dumps(messages[sent]).encode()))
        sent += 1
    try:
        data = s.recv(1 << 20)
    except socket.timeout:
        continue
    if not data:
        print("server closed after %.2fs" % (time.time() - start))
        break
    buf += data
    at = 0  # parsed up to here; the buffer is cut once per read, which keeps up with thousands of messages a second
    while len(buf) - at >= 2:
        n, off = buf[at + 1] & 127, 2
        if n == 126:
            if len(buf) - at < 4: break
            n, off = struct.unpack(">H", buf[at + 2:at + 4])[0], 4
        elif n == 127:
            if len(buf) - at < 10: break
            n, off = struct.unpack(">Q", buf[at + 2:at + 10])[0], 10
        if len(buf) - at < off + n: break
        body = at + off
        received += n
        if first is None:
            first = time.time() - start
        if buf[at] & 15 == 2 and buf[body] == 0:  # padding, with which the host finds out what the link carries
            pass
        elif buf[at] & 15 == 2 and buf[body] == 7:  # the pointer's shape: sizes, tip, then red, green, blue, alpha
            width, height, tip_x, tip_y = struct.unpack(">4H", buf[body + 1:body + 9])
            pixels = buf[body + 9:body + n]
            opaque = sum(1 for i in range(3, len(pixels), 4) if pixels[i] > 127)
            print("pointer shape: %dx%d, tip at %d,%d, %d of %d pixels opaque" % (width, height, tip_x, tip_y, opaque, len(pixels) // 4))
        elif buf[at] & 15 == 2 and buf[body] == 8:  # sound: one Opus packet, saved with a 2-byte length in front
            arrivals.append(time.time())
            sound.write(struct.pack(">H", n - 1) + buf[body + 1:body + n])
            packets += 1
        elif buf[at] & 15 == 2:  # other binary frames are video; text frames are the host's statistics and ping replies
            arrivals.append(time.time())
            flv.write(buf[body:body + n])
        elif buf[body:body + 7] == b'["ping"':
            rtt = time.time() - json.loads(buf[body:body + n])[1]
        elif buf[body:body + 8] == b'["stats"':
            stats.append(json.loads(buf[body:body + n]))
        elif buf[body:body + 10] == b'["screens"':
            _, names, shown = json.loads(buf[body:body + n])
            print("screens: %s, showing %s" % (json.dumps(names), shown))
        elif buf[body:body + 12] == b'["clipboard"':
            print("clipboard text from the host: %r" % json.loads(buf[body:body + n])[1][:80])
        at = body + n
    buf = buf[at:]
flv.close()
if stats:
    print("host encode ms per second: " + " ".join("%.1f" % s[1] for s in stats))
sound.close()
print("%d sound packets, %d bytes" % (packets, os.path.getsize(out + ".opus")))
gaps = sorted(b - a for a, b in zip(arrivals, arrivals[1:]))
print("first video after %s s, %d ws messages, %d bytes" % (first and round(first, 2), len(arrivals), os.path.getsize(out)))
if gaps:
    print("gap between messages ms: median %.1f p99 %.1f max %.1f" % (gaps[len(gaps) // 2] * 1e3, gaps[int(len(gaps) * .99)] * 1e3, gaps[-1] * 1e3))
