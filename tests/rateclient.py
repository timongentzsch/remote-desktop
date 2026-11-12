"""Plays a browser's part for the bitrate controller: logs in, pings ten times a second with the last round
trip and what has arrived, and prints second by second what the host allowed and what arrived.
Arguments: host, seconds, login settings as JSON (optional)."""
import base64, json, os, socket, ssl, struct, sys, time

host, seconds = sys.argv[1], float(sys.argv[2])
login = {"password": "", "fps": "60", **(json.loads(sys.argv[3]) if len(sys.argv) > 3 else {})}


def frame(payload):
    mask = os.urandom(4)
    head = bytes([0x81, 0x80 | len(payload)]) if len(payload) < 126 else bytes([0x81, 0x80 | 126]) + struct.pack(">H", len(payload))
    return head + mask + bytes(b ^ mask[i % 4] for i, b in enumerate(payload))


context = ssl.create_default_context()
context.check_hostname, context.verify_mode = False, ssl.CERT_NONE  # a test network; the certificate is not the subject
s = context.wrap_socket(socket.create_connection((host, 8765)), server_hostname=host)
s.sendall(b"GET /ws HTTP/1.1\r\nHost: x\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n"
          b"Sec-WebSocket-Key: " + base64.b64encode(os.urandom(16)) + b"\r\nSec-WebSocket-Version: 13\r\n\r\n")
buf = bytearray()
while b"\r\n\r\n" not in buf:
    buf += s.recv(4096)
del buf[:buf.index(b"\r\n\r\n") + 4]
start = time.time()
s.sendall(frame(json.dumps(login).encode()))
s.settimeout(0.02)

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
        s.sendall(frame(json.dumps(["ping", now, round(rtt * 1000), received]).encode()))
    try:
        data = s.recv(1 << 20)
    except socket.timeout:
        continue
    except ssl.SSLWantReadError:
        continue
    if not data:
        print("the host closed the connection after %.1f s" % (time.time() - start))
        break
    buf += data
    at = 0
    while len(buf) - at >= 2:
        n, off = buf[at + 1] & 127, 2
        if n == 126:
            if len(buf) - at < 4: break
            n, off = struct.unpack_from(">H", buf, at + 2)[0], 4
        elif n == 127:
            if len(buf) - at < 10: break
            n, off = struct.unpack_from(">Q", buf, at + 2)[0], 10
        if len(buf) - at < off + n: break
        body = at + off
        received += n
        if buf[at] & 15 == 1:
            message = json.loads(bytes(buf[body:body + n]))
            if message[0] == "ping":
                rtt = time.time() - message[1]
            elif message[0] == "stats":
                allowed, held = message[3], message[4]
        elif buf[body] == 0:
            padding += n
        elif buf[body] != 8:
            video += n
            frames += 1
            arrived = time.time()
            gap, last_frame = max(gap, arrived - last_frame), arrived
        at = body + n
    del buf[:at]
