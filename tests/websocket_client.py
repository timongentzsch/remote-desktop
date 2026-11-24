"""WebSocket wire format for the host test clients. The host sends whole, unmasked messages."""
import base64, os, socket, ssl, time


def test_tls():
    """TLS without certificate verification for the isolated network and loopback benchmarks."""
    context = ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT)
    context.check_hostname, context.verify_mode = False, ssl.CERT_NONE
    return context


def video_frame(data):
    """An FLV video tag containing a coded frame. Byte 12 is zero for decoder configuration."""
    return len(data) >= 13 and data[0] == 9 and data[12] == 1


class WebSocket:
    def __init__(self, host, port, context=None, name=None):
        self.socket = socket.create_connection((host, port))
        if context is not None:
            self.socket = context.wrap_socket(self.socket, server_hostname=name or host)
        key = base64.b64encode(os.urandom(16))
        self.socket.sendall(b"GET /ws HTTP/1.1\r\nHost: x\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n"
                            b"Sec-WebSocket-Key: " + key + b"\r\nSec-WebSocket-Version: 13\r\n\r\n")
        header = bytearray()
        while b"\r\n\r\n" not in header:
            chunk = self.socket.recv(4096)
            if not chunk:
                raise EOFError("connection closed during WebSocket upgrade")
            header.extend(chunk)
        header, _, self.buffer = header.partition(b"\r\n\r\n")
        self.status = header.split(b"\r\n", 1)[0].decode()

    def send(self, text):
        data = text.encode()
        n = len(data)
        if n < 126:
            size = bytes([0x80 | n])
        elif n <= 65535:
            size = b"\xfe" + n.to_bytes(2, "big")
        else:
            size = b"\xff" + n.to_bytes(8, "big")
        mask = os.urandom(4)
        self.socket.sendall(b"\x81" + size + mask + bytes(b ^ mask[i % 4] for i, b in enumerate(data)))

    def _pop(self):
        buf = self.buffer
        if len(buf) < 2:
            return None
        n = buf[1] & 127
        head = 4 if n == 126 else 10 if n == 127 else 2
        if len(buf) < head:
            return None
        if head > 2:
            n = int.from_bytes(buf[2:head], "big")
        if len(buf) < head + n:
            return None
        message = buf[0] & 15, buf[head:head + n]
        del buf[:head + n]
        return message

    def receive(self, timeout):
        """Return (opcode, body). A timeout has no opcode; socket EOF is also a close (8)."""
        deadline = time.monotonic() + timeout
        while True:
            message = self._pop()
            if message is not None:
                return message
            left = deadline - time.monotonic()
            if left <= 0:
                return None, b""
            self.socket.settimeout(left)
            try:
                chunk = self.socket.recv(1 << 20)
            except (socket.timeout, ssl.SSLWantReadError):
                return None, b""
            if not chunk:
                return 8, b""
            self.buffer.extend(chunk)
