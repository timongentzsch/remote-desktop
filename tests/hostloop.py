"""Times the host's own part of the loop, on the host: from sending a pointer move to the arrival of the next
piece of video, with the host painting the pointer. No network to speak of and no decoding are in it.
Arguments: port, how many moves."""
import json, sys, time

from websocket_client import WebSocket, test_tls

port, moves = int(sys.argv[1]), int(sys.argv[2])

ws = WebSocket("127.0.0.1", port, test_tls(), "localhost")
ws.send(json.dumps({"password": "", "cursor": "yes", "sound": "off"}))


def video_until(deadline):
    """The arrival time of the next piece of video; skip sound, padding and pointer shapes."""
    while (left := deadline - time.time()) > 0:
        opcode, data = ws.receive(left)
        if opcode is None:
            break
        if opcode == 8:
            raise SystemExit("the host closed the connection")
        if opcode == 2 and data and data[0] not in (0, 7, 8):
            return time.time()


while video_until(time.time() + 3.0):  # until the screen has been still for three seconds, refinement included
    pass
delays, places = [], [(16000, 20000), (40000, 42000)]
for turn in range(moves):
    while video_until(time.time() + 0.05):  # nothing else under way
        pass
    began = time.time()
    ws.send(json.dumps(["move", *places[turn % 2]]))
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
