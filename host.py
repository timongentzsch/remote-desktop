#!/usr/bin/env python3
"""Remote desktop host: screen -> H.264 -> WebSocket, browser input -> uinput.

Needs python3, gpu-screen-recorder, and access to /dev/uinput (see install.sh).
Listens on localhost only. From the client run
    ssh -NL 8765:localhost:8765 user@host
and open http://localhost:8765 in Chrome. Browsers only expose WebCodecs and
Keyboard Lock on secure origins, and localhost is one.
"""
import asyncio
import base64
import contextlib
import fcntl
import hashlib
import hmac
import json
import os
import re
import struct
from pathlib import Path

BIND = ("127.0.0.1", 8765)
ROOT = Path(__file__).resolve().parent
PASSWORD = (ROOT / "password").read_bytes().strip()
FILES = {
    "/": ("index.html", "text/html"),
    "/manifest.json": ("manifest.json", "application/manifest+json"),
    "/icon.svg": ("icon.svg", "image/svg+xml"),
}
# Capture is any program that writes FLV-wrapped H.264 to stdout. The native one (native/) is preferred: it
# only produces frames when the screen changes and can keep full colour resolution. gpu-screen-recorder is the
# fallback where the native one is not built or cannot do what is asked (it does not scale yet).
NATIVE = ROOT / "native/target/release/remote-desktop"
RECORDER = ["gpu-screen-recorder", "-w", "screen", "-c", "flv", "-k", "h264", "-encoder", "cpu", "-cr", "full", "-keyint", "300"]
RECORDER_FLAGS = {"fps": "-f", "size": "-s", "cursor": "-cursor"}
# Client-selectable settings and their allowed values, default first.
SETTINGS = {
    "fps": ("60", "30", "15"),
    "size": ("0x0", "1920x1080", "1280x720"),
    "cursor": ("no", "yes"),  # "no": the client draws its own pointer, which does not lag
    "chroma": ("420", "444"),  # 444 keeps full colour resolution; native capture only
}

EV_SYN, EV_KEY, EV_REL, EV_ABS = 0, 1, 2, 3
REL_HWHEEL, REL_WHEEL, REL_WHEEL_HI_RES, REL_HWHEEL_HI_RES = 6, 8, 11, 12
KEY_LEFTSHIFT, BTN_MOUSE = 42, 0x110
ABS_RANGE = 65535  # pointer positions arrive normalised to this range

# linux/uinput.h ioctls (identical on x86_64 and arm64)
UI_DEV_CREATE, UI_DEV_SETUP, UI_ABS_SETUP = 0x5501, 0x405C5503, 0x401C5504
UI_SET_EVBIT, UI_SET_KEYBIT, UI_SET_RELBIT, UI_SET_ABSBIT = 0x40045564, 0x40045565, 0x40045566, 0x40045567


def uinput(name, keys, rel=(), absolute=()):
    """Create a virtual input device and return its file descriptor."""
    fd = os.open("/dev/uinput", os.O_WRONLY)
    for event, set_bit, codes in (
        (EV_KEY, UI_SET_KEYBIT, keys), (EV_REL, UI_SET_RELBIT, rel), (EV_ABS, UI_SET_ABSBIT, absolute),
    ):
        if codes:
            fcntl.ioctl(fd, UI_SET_EVBIT, event)
        for code in codes:
            fcntl.ioctl(fd, set_bit, code)
    fcntl.ioctl(fd, UI_DEV_SETUP, struct.pack("4H80sI", 6, 1, 1, 1, name.encode(), 0))  # bus 6 = virtual
    for code in absolute:
        fcntl.ioctl(fd, UI_ABS_SETUP, struct.pack("H2x6i", code, 0, 0, ABS_RANGE, 0, 0, 0))
    fcntl.ioctl(fd, UI_DEV_CREATE)
    return fd


def emit(fd, *events):
    """Write (type, code, value) events followed by a sync report."""
    os.write(fd, b"".join(struct.pack("llHHi", 0, 0, *event) for event in (*events, (EV_SYN, 0, 0))))


def displays_asleep():
    return any(
        (connector / "status").read_text() == "connected\n" and (connector / "dpms").read_text() != "On\n"
        for connector in Path("/sys/class/drm").glob("card*-*")
    )


async def recv(reader):
    """Next WebSocket message from the client, or None once it closes."""
    head = await reader.readexactly(2)
    length = head[1] & 127
    if length == 126:
        length = int.from_bytes(await reader.readexactly(2), "big")
    if head[0] & 15 == 8 or length == 127:  # close frame, or larger than anything the client sends
        return None
    mask = await reader.readexactly(4)
    return bytes(byte ^ mask[i % 4] for i, byte in enumerate(await reader.readexactly(length)))


def frame_header(opcode, length):
    if length < 126:
        return bytes([0x80 | opcode, length])
    if length < 1 << 16:
        return bytes([0x80 | opcode, 126]) + length.to_bytes(2, "big")
    return bytes([0x80 | opcode, 127]) + length.to_bytes(8, "big")


async def session(reader, writer, keyboard, pointer):
    login = json.loads(await recv(reader) or b"{}")
    if not PASSWORD or not hmac.compare_digest(str(login.get("password")).encode(), PASSWORD):
        await asyncio.sleep(1)  # slow down guessing
        return
    bitrate = str(login.get("bitrate"))
    kbps = min(max(int(bitrate), 500), 50000) if bitrate.isdigit() else 20000
    chosen = {name: login[name] if login.get(name) in allowed else allowed[0] for name, allowed in SETTINGS.items()}
    if NATIVE.exists() and chosen["size"] == "0x0":
        command = [NATIVE, "--bitrate", str(kbps), "--fps", chosen["fps"], "--cursor", chosen["cursor"], "--chroma", chosen["chroma"]]
    else:
        # Constant quality (crf) capped at the bitrate, with at most a fifth of a second of it buffered: a still
        # screen costs almost nothing and motion cannot outrun the link. These are x264 options.
        command = RECORDER + ["-bm", "cbr", "-q", str(kbps), "-ffmpeg-video-opts",
                              f"tune=zerolatency;preset=ultrafast;crf=20;maxrate={kbps}000;bufsize={kbps * 200}"]
        for name, flag in RECORDER_FLAGS.items():
            command += [flag, chosen[name]]

    if displays_asleep():  # capture fails on a sleeping display; a key press wakes it
        emit(keyboard, (EV_KEY, KEY_LEFTSHIFT, 1))
        emit(keyboard, (EV_KEY, KEY_LEFTSHIFT, 0))
        for _ in range(30):
            if not displays_asleep():
                break
            await asyncio.sleep(0.1)

    capture = await asyncio.create_subprocess_exec(*command, stdout=asyncio.subprocess.PIPE)

    async def video():
        with contextlib.suppress(OSError):  # client went away
            while chunk := await capture.stdout.read(1 << 16):
                writer.write(frame_header(2, len(chunk)))
                writer.write(chunk)
                await writer.drain()
        writer.close()

    stream = asyncio.create_task(video())
    held = set()
    device = lambda code: pointer if code >= BTN_MOUSE else keyboard
    try:
        while message := await recv(reader):
            kind, a, b = json.loads(message)
            if kind == "key":
                (held.add if b else held.discard)(a)
                emit(device(a), (EV_KEY, a, b))
            elif kind == "move":
                emit(pointer, (EV_ABS, 0, a), (EV_ABS, 1, b))
            elif kind == "scroll":  # hi-res wheel units, 120 per notch
                emit(pointer, (EV_REL, REL_HWHEEL_HI_RES, a), (EV_REL, REL_WHEEL_HI_RES, b))
            elif kind == "ping":  # echoed so the client can time the round trip
                writer.write(frame_header(1, len(message)) + message)
    finally:
        for code in held:
            emit(device(code), (EV_KEY, code, 0))
        stream.cancel()
        if capture.returncode is None:
            capture.terminate()


async def handle(reader, writer, keyboard, pointer):
    try:
        request = (await reader.readuntil(b"\r\n\r\n")).decode("latin1")
        path = request.split(" ", 2)[1]
        key = re.search(r"(?im)^sec-websocket-key:\s*(\S+)", request)
        if path == "/ws" and key:
            accept = hashlib.sha1(key[1].encode() + b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11").digest()
            writer.write(
                b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n"
                b"Sec-WebSocket-Accept: " + base64.b64encode(accept) + b"\r\n\r\n"
            )
            await session(reader, writer, keyboard, pointer)
        elif path in FILES:
            name, mime = FILES[path]
            body = (ROOT / name).read_bytes()
            writer.write(
                f"HTTP/1.1 200 OK\r\nContent-Type: {mime}\r\nContent-Length: {len(body)}\r\n"
                "Cache-Control: no-cache\r\nConnection: close\r\n\r\n".encode() + body
            )
            await writer.drain()
        else:
            writer.write(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
    except (OSError, EOFError):  # client went away
        pass
    finally:
        writer.close()


async def main():
    keyboard = uinput("Remote desktop keyboard", keys=range(1, 248))
    pointer = uinput(
        "Remote desktop pointer",
        keys=range(BTN_MOUSE, BTN_MOUSE + 5),
        rel=(REL_HWHEEL, REL_WHEEL, REL_WHEEL_HI_RES, REL_HWHEEL_HI_RES),
        absolute=(0, 1),
    )
    server = await asyncio.start_server(lambda r, w: handle(r, w, keyboard, pointer), *BIND)
    async with server:
        await server.serve_forever()


if __name__ == "__main__":
    asyncio.run(main())
