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
    loss-test.sh      how late frames are drawn on a link that delays and loses packets (late-check.mjs)
    udp-test.sh       the video's way back to TCP when UDP is cut off, and to UDP when it returns
    page-check.mjs    drives the client page in headless Chrome and prints what it shows
    serve-screens.sh, serve-x11.sh            desktops to point page-check.mjs at
    delay-check.mjs   the delay from moving the pointer to seeing it moved, on a real connection
    hostloop.py       the host's own share of that delay, measured on the host
    bridge.py         reaches a host that listens on its own machine only, without opening it to the network
