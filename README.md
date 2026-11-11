# Remote desktop

A Linux remote-desktop host in one binary with a browser client. It serves the client page, streams the
screen as H.264 and injects keyboard and pointer input. Capture currently needs a Wayland compositor that
offers `ext-image-copy-capture-v1` (Hyprland, Sway and other wlroots compositors).

## Install on the machine to control

    ./remote-desktop install      # copies itself to /usr/local/bin, asks for sudo and a password
    remote-desktop password       # change the password later

The service listens on `127.0.0.1:8765` and runs with the desktop session.

## Connect

    ssh -NL 8765:localhost:8765 user@host

Then open http://localhost:8765 in Chrome. Browsers only allow video decoding and keyboard capture on
secure origins; localhost is one, which is why the tunnel is needed until the host serves HTTPS itself.

## Build

Sources are in `native/`; the client (`index.html`, `manifest.json`, `icon.svg`) is compiled into the binary.
x264 is built from the `native/x264` submodule, so clone with `--recurse-submodules`.

Needs Rust, a C compiler, `make` and, for x86, `nasm`.

    cd native
    cargo test --release
    cargo build --release

Portable binaries (glibc 2.28 or newer) need `zig` and `cargo-zigbuild`:

    cargo zigbuild --release --target x86_64-unknown-linux-gnu.2.28
    cargo zigbuild --release --target aarch64-unknown-linux-gnu.2.28

Linking x264 makes the binary GPL-licensed.

`host.py` and `install.sh` are the earlier Python host, kept as a fallback for now.
