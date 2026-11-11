# Remote desktop

A Linux remote-desktop host in one binary with a browser client. It serves the client page over HTTPS,
streams the screen as H.264 and injects keyboard and pointer input. Capture currently needs a Wayland
compositor that offers `ext-image-copy-capture-v1` (Hyprland, Sway and other wlroots compositors).

## Install on the machine to control

Run this in a terminal of the desktop session you want to control. It downloads the binary for the
machine's architecture (x86_64 or aarch64) and sets everything up:

    curl -fLo remote-desktop https://github.com/timongentzsch/remote-desktop/releases/latest/download/remote-desktop-$(uname -m) && chmod +x remote-desktop && ./remote-desktop install

The binaries are built by `.github/workflows/build.yml` and published when a version tag is pushed
(`git tag v0.1.0 && git push --tags`).

`install` asks for a login password and for sudo, then:

- copies the binary to `/usr/local/bin/remote-desktop`,
- lets the logged-in user create input devices (a udev rule for `/dev/uinput`),
- opens TCP port 8765 if ufw or firewalld is active,
- creates a certificate,
- starts the `remote-desktop` service of your user, which runs with the desktop session.

It ends by printing the address to open and, for the default certificate, how to trust it.

## Certificates

Browsers only allow video decoding and keyboard capture on secure pages, so the host always serves HTTPS.

**Self-signed (default).** Works on any network without outside services. Each client has to trust the
certificate once; on a Mac:

    scp user@host:.config/remote-desktop/certificate.pem remote-desktop.pem
    security add-trusted-cert -r trustRoot -p ssl -k ~/Library/Keychains/login.keychain-db remote-desktop.pem

It is valid for 820 days and names the machine's hostname and address at the time it was made. Run
`remote-desktop certificate` for a new one, then trust that one.

**Tailscale.** If the machine is on a tailnet with HTTPS certificates enabled (admin console, DNS page):

    remote-desktop install --tailscale        # or later: remote-desktop certificate --tailscale

The certificate then comes from Tailscale, browsers trust it as it is, and the host is reachable from any
device on the tailnet at `https://<machine>.<tailnet>.ts.net:8765`. The service renews it by itself, for
which `install` makes your user Tailscale's operator on this machine.

## Other commands

    remote-desktop password                  change the login password
    remote-desktop password --none           allow access without a password (anyone who can reach the port)
    remote-desktop --listen 127.0.0.1:8765   run by hand on another address

## Build

Sources are in `native/`; the client (`index.html`, `manifest.json`, `icon.svg`) is compiled into the binary.
x264 is built from the `native/x264` submodule, so clone with `--recurse-submodules`.

Needs Rust, a C compiler, `make` and, for x86, `nasm`.

    cd native
    cargo test --release
    cargo build --release

Release binaries that run on any distribution with glibc 2.28 or newer need `zig` and `cargo-zigbuild`:

    cargo zigbuild --release --target x86_64-unknown-linux-gnu.2.28
    cargo zigbuild --release --target aarch64-unknown-linux-gnu.2.28

Linking x264 makes the binary GPL-licensed.
