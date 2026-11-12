"""Wraps length-prefixed Opus packets (as saved by wsclient.py) in an Ogg file that ordinary tools can decode."""
import struct, sys

def crc(data):
    value = 0
    for byte in data:
        value ^= byte << 24
        for _ in range(8):
            value = ((value << 1) ^ 0x04C11DB7) & 0xFFFFFFFF if value & 0x80000000 else (value << 1) & 0xFFFFFFFF
    return value

def page(packet, granule, sequence, flags=0):
    lacing = bytes([255] * (len(packet) // 255) + [len(packet) % 255])
    head = b"OggS" + struct.pack("<BBqIII", 0, flags, granule, 1, sequence, 0) + bytes([len(lacing)]) + lacing
    body = head + packet
    return body[:22] + struct.pack("<I", crc(body)) + body[26:]

raw = open(sys.argv[1], "rb").read()
out = open(sys.argv[2], "wb")
out.write(page(b"OpusHead" + struct.pack("<BBHIhB", 1, 2, 0, 48000, 0, 0), 0, 0, 2))
out.write(page(b"OpusTags" + struct.pack("<I", 4) + b"test" + struct.pack("<I", 0), 0, 1))
at, sequence = 0, 2
while at < len(raw):
    size = struct.unpack(">H", raw[at:at + 2])[0]
    out.write(page(raw[at + 2:at + 2 + size], (sequence - 1) * 960, sequence))
    at += 2 + size
    sequence += 1
print("%d packets" % (sequence - 2))
