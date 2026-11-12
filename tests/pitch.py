"""Prints the level and the dominant pitch of each channel of a 16-bit stereo WAV file."""
import struct, sys, wave
w = wave.open(sys.argv[1]); n = w.getnframes(); rate = w.getframerate()
samples = struct.unpack("<%dh" % (n * 2), w.readframes(n))
for name, channel in (("left", samples[0::2]), ("right", samples[1::2])):
    crossings = sum(1 for a, b in zip(channel, channel[1:]) if a < 0 <= b)
    print("%s: %.2f s, peak %d, about %.0f Hz" % (name, n / rate, max(map(abs, channel)), crossings / (n / rate)))
