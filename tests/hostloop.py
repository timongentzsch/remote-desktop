"""Times the host's own part of the loop, on the host: from sending a pointer move to the arrival of the next
piece of video, with the host painting the pointer. No network to speak of and no decoding are in it.
Arguments: port, how many moves."""
import base64, json, os, socket, ssl, struct, sys, time

port, moves = int(sys.argv[1]), int(sys.argv[2])


def frame(payload):
    mask = os.urandom(4)
    return bytes([0x81, 0x80 | len(payload)]) + mask + bytes(b ^ mask[i % 4] for i, b in enumerate(payload))


context = ssl.create_default_context()
context.check_hostname, context.verify_mode = False, ssl.CERT_NONE
s = context.wrap_socket(socket.create_connection(("127.0.0.1", port)), server_hostname="localhost")
s.sendall(b"GET /ws HTTP/1.1\r\nHost: x\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n"
          b"Sec-WebSocket-Key: " + base64.b64encode(os.urandom(16)) + b"\r\nSec-WebSocket-Version: 13\r\n\r\n")
buf = bytearray()
while b"\r\n\r\n" not in buf:
    buf += s.recv(4096)
del buf[:buf.index(b"\r\n\r\n") + 4]
s.sendall(frame(json.dumps({"password": "", "cursor": "yes", "sound": "off"}).encode()))


def video_until(deadline):
    """The arrival time of the next video message, or None once `deadline` has passed. Other messages are skipped."""
    global buf
    while True:
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
            binary, kind = buf[at] & 15 == 2, buf[at + off]
            at += off + n
            if binary and kind not in (0, 7, 8):
                del buf[:at]
                return time.time()
        del buf[:at]
        left = deadline - time.time()
        if left <= 0:
            return None
        s.settimeout(left)
        try:
            buf += s.recv(1 << 20)
        except (socket.timeout, ssl.SSLWantReadError):
            return None


while video_until(time.time() + 3.0):  # until the screen has been still for three seconds, refinement included
    pass
delays, places = [], [(16000, 20000), (40000, 42000)]
for turn in range(moves):
    while video_until(time.time() + 0.05):  # nothing else under way
        pass
    began = time.time()
    s.sendall(frame(json.dumps(["move", *places[turn % 2]]).encode()))
    arrived = video_until(began + 1.0)
    delays.append(None if arrived is None else (arrived - began) * 1000)
    time.sleep(0.4)
seen = sorted(delay for delay in delays if delay is not None)
if seen:
    at = lambda share: seen[min(len(seen) - 1, int(share * len(seen)))]
    print("on the host alone, pointer moved to video sent, %d of %d moves seen: fastest %.0f ms, median %.0f ms, nine in ten within %.0f ms, slowest %.0f ms"
          % (len(seen), len(delays), at(0), at(0.5), at(0.9), at(1)))
else:
    print("no move produced video")
