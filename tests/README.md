# Tests

The unit tests are in the sources (`cargo test` in `native/`). What is here runs the host for real, on every
kind of desktop it supports, each in a container.

    ./images.sh        build the test images (once, and after changing one in images/)
    ./run.sh           the whole regression: checks, build, then every desktop

`run.sh` needs Docker and `/dev/uinput` on the machine it runs on, and takes about half an hour. It prints
each step's figures to be read; only the checks at its start pass or fail by themselves. The steps on the
GPU's encoder run where Docker has an NVIDIA card to give. KDE's compositor needs a GPU device (`/dev/dri`).

Each step is a script that runs inside one of the images, with this directory mounted as `/test`:

    *-test.sh, x11-*.sh, gnome-clipboard.sh   start a headless desktop and the host, connect a client, report
    wsclient.py                               that client: logs in, records the stream, prints what it was told
    opus2ogg.py, pitch.py, mutter-pointer.py  helpers of single tests
    xorg-two.conf                             an X server with room for two monitors

Beside the regression there are tools for looking at one thing closely. They start their own containers:

    rate-test.sh      the bitrate on a link whose speed changes (serve-rate.sh, rateclient.py, relay.py)
    video-test.sh     a video with sound, watched through the real client in a browser (serve-video.sh)
    loss-test.sh      frame delay and gaps between draws and browser refreshes on a lossy link (late-check.mjs)
    udp-test.sh       the video's way back to TCP when UDP is cut off, and to UDP when it returns
    page-check.mjs    drives the client page in headless Chrome and prints what it shows
    fingers-check.mjs the page as a phone has it: each gesture on the picture and the trackpad, and what is sent
    decoder-check.mjs the page in a browser that does not decode what the host sends: it asks for less, once
    serve-screens.sh, serve-x11.sh            desktops to point page-check.mjs at
    delay-check.mjs   the delay from moving the pointer to seeing it moved, on a real connection
    hostloop.py       the host's own share of that delay, measured on the host
    bridge.py         reaches a host that listens on its own machine only, without opening it to the network

### Scrolling and dragging

Run the fixture in the desktop session under test. It opens a scrolling document and a separate window with
a native title bar. On Windows use PowerShell. The stop file must be absent at startup.

    .\motion-window.ps1 -StopFile "$env:TEMP\stop-motion-test"

On an X11 desktop with a window manager and Tk, run `python3 motion-window.py`. The fixture also takes
`--geometry WxH+X+Y` and `--stop-file PATH` to select a display and close it from a script.

On the client, run against the host's HTTPS address. Set `PASSWORD_FILE` to a file containing the session
password if needed. The optional final argument is the duration in seconds of each gesture.

    IDLE_SECONDS=6 node interaction-check.mjs https://HOST/ 8

The test sends wheel and mouse input at 60 Hz, samples the visible document and window position on browser
refreshes, and times clicks until the document changes colour. It uses TCP and a 2 Mbit/s video ceiling by
default. `KBPS` selects another bitrate from the client's menu. For comparisons, use the same link shaping,
viewport, and Windows display settings. The JSON includes visible-update gaps in milliseconds, observed
resolutions, idle recovery, and click response times. Create the stop file on Windows to close the fixture.

### Client size and Windows scaling

Pass the browser viewport, pixel ratio, and expected display mode at each resize. Include a nearby mode
for a size the display cannot provide. The test checks the captured desktop and decoded video, exits the
Client browser, and opens a new Host session to verify that the original settings returned.

    node client-size-check.mjs https://HOST/ 1024x768@1=1024x768 640x360@2=1280x720

An X11 host has no scale to set: its screen is the window's size in CSS pixels, so `1500x850@2=1500x850`.

To measure Windows UI scaling as well, run the fixture in the interactive session on the primary display.
Use a stop file that does not yet exist.

    .\dpi-window.ps1 -StopFile "$env:TEMP\stop-dpi-test"

Append the expected effective DPI to every step. This example holds the desktop at 1920×1080 and
expects Windows UI scaling to change from 100% to 125%. The green bar is one inch wide in Windows
logical units. The fixture gets its width from the monitor's effective DPI through a per-monitor-aware
window. The test checks the outgoing viewport and DPR, the display scale reported by the host, and the
bar width in decoded pixels, allowing two pixels for video compression.
[GetDpiForWindow](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-getdpiforwindow)

    node client-size-check.mjs https://HOST/ 1920x1080@1=1920x1080:96 1536x864@1.25=1920x1080:120

To exercise a move between monitors at the same CSS viewport size, add a step with unchanged viewport
dimensions and a different ratio, using the actual supported resolution and scale as its expected values.

Choose modes and scale levels offered for the test monitor in Windows Display settings. If a small
virtual display offers only 100%, expect `:96` for its fallback case and record the resulting display
mode. Use a larger mode with at least two supported scales to verify a DPI transition. The test also
measures the original DPI before connection and again after disconnect. Create the stop file to close the fixture.

    New-Item "$env:TEMP\stop-dpi-test" -ItemType File
