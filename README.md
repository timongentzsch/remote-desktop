# Remote desktop

A Linux remote-desktop host in one binary, with a client that runs in the browser. It serves the client page
over HTTPS, streams the screen as H.264 and the desktop's sound as Opus, shares the clipboard, and injects
keyboard and pointer input.

- **One file on the host.** No runtime dependencies beyond glibc 2.28; x86_64 and aarch64.
- **Nothing to install on the client.** Open the address in Chrome, or install the page as an app from there.
- **Made to feel local.** A still screen costs nothing, a busy one is encoded in a few milliseconds, the
  pointer is drawn by the client, and the bitrate follows the connection.

The picture is encoded on the GPU where an NVIDIA card with a hardware encoder and its driver (470 or newer)
are present, which takes about a tenth of the processor time, and with the built-in x264 everywhere else.
Nothing has to be configured; the service's log says which one a session uses. The GPU encodes for one
viewer at a time; if a second one connects meanwhile, that session is encoded with x264.

## What it runs on

Any Linux distribution on x86_64 or aarch64, with any desktop, on Wayland or X11. The exceptions:

- Distributions without glibc 2.28 or newer, such as Alpine with musl.
- Wayland compositors that are neither GNOME nor KDE and lack the `ext-image-copy-capture-v1` protocol, as
  older releases of Sway, Hyprland and other wlroots compositors do.
- On GNOME under Wayland the clipboard is not shared, because GNOME offers programs no way to do that.
- Without PipeWire or PulseAudio there is no sound.

The client is developed and tested with Chrome. It needs WebCodecs, and Keyboard Lock for sending system
shortcuts such as Alt+Tab to the remote side, which works in full screen.

## Install on the machine to control

Run this in a terminal of the desktop session you want to control. It downloads the binary for the
machine's architecture (x86_64 or aarch64) and sets everything up:

    curl -fLo remote-desktop https://github.com/timongentzsch/remote-desktop/releases/latest/download/remote-desktop-$(uname -m) && chmod +x remote-desktop && ./remote-desktop install

The binaries are built by `.github/workflows/build.yml` and published when a version tag is pushed
(`git tag v0.1.0 && git push --tags`).

`install` asks for a login password and for sudo, then:

- copies the binary to `/usr/local/bin/remote-desktop`,
- lets the user logged in at the machine create input devices (a udev rule for `/dev/uinput`),
- opens TCP port 8765 if ufw or firewalld is active,
- creates a certificate, and keeps it with the password in `~/.config/remote-desktop/`,
- writes a hidden desktop entry, which Plasma before 6.8 requires of programs that capture the screen,
- starts the `remote-desktop` service of your user, which runs with the desktop session.

It ends by printing the address to open and, for the default certificate, how to trust it.
`remote-desktop uninstall` undoes all of it except the password and the certificate.

## Security

The host gives whoever logs in full control of the desktop session, so treat its address and password like
an SSH login.

- Everything goes over TLS. The password is compared in constant time, and after a wrong one no login is
  accepted for a second, whichever connection it came from. Use a long password if the port is reachable
  from networks you do not trust, or better, reach the host through Tailscale or another VPN.
- The client remembers the password in the browser's local storage once a login succeeded, so that the next
  visit needs one click. Anyone with access to that browser profile can read it.
- `remote-desktop password --none` removes the password. Then anyone who can reach port 8765 controls the
  desktop. Other web sites cannot do so through a visitor's browser: connections are only accepted from the
  host's own page.
- The udev rule lets every program of the user at the seat create input devices, not only this one.
- It listens on all network interfaces. `ExecStart` in `~/.config/systemd/user/remote-desktop.service`
  takes `--listen ADDRESS:PORT` to restrict that.

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
    remote-desktop uninstall                 remove the service, the binary and the input rule
    remote-desktop --listen 127.0.0.1:8765   run by hand on another address

## Known limitations

- Only the first output is captured.
- The pointer is either drawn by the client, always as an arrow, or painted into the video by the host.
- "Sharp" colour (4:4:4) is decoded in software by Chrome, which costs processor time on the client.
- After a slow connection the automatic bitrate climbs back only while the picture is moving, so a still
  screen can keep saying that the link is slow for a while.
- It was developed on a real Hyprland session. Sway, GNOME (Mutter 51), KDE (KWin 6.8) and X11 were only run
  headless in test environments: their login and lock screens, and input on them, are untried.
- On X11, clipboard texts so long that their owner sends them in instalments (some hundred kilobytes) are
  not passed on, and the service needs `DISPLAY` in the environment of the user's systemd, which desktop
  sessions set up and a bare `startx` does not. The GPU encoder was run on one card (RTX 4090), and capture on aarch64 not at all.
- The client is made for a keyboard and a mouse or trackpad, not for touch.

## Build

Sources are in `native/`; the client (`index.html`, `manifest.json`, `icon.svg`) is compiled into the binary.
x264 is built from the `native/x264` submodule. The `native/nv-codec-headers` and `native/pipewire`
submodules provide headers only, for the GPU encoder and for the screen casts of GNOME and KDE; NVIDIA's and
PipeWire's libraries are loaded at run time where present. Clone with `--recurse-submodules`.

Needs Rust, a C compiler, `make` and, for x86, `nasm`.

    cd native
    cargo fmt --check
    cargo clippy --release --all-targets -- --deny warnings
    cargo test --release
    cargo build --release

The tests also exercise the GPU encoder where the machine has one.

Release binaries that run on any distribution with glibc 2.28 or newer need `zig` and `cargo-zigbuild`:

    cargo zigbuild --release --target x86_64-unknown-linux-gnu.2.28
    cargo zigbuild --release --target aarch64-unknown-linux-gnu.2.28

## Layout

    index.html                 the client: page, styles and script in one file
    native/src/
      main.rs                  the command line
      install.rs               install, uninstall, password
      server.rs                HTTPS: the client page, and the upgrade to a WebSocket
      session.rs               one client: login, its threads, the input it sends
      outbox.rs                what waits to be sent to a client, and in which order
      tls.rs, certificate.rs   the encrypted connection; the certificate behind it
      ws.rs                    WebSocket framing
      capture/                 the screen's picture: mod.rs picks wayland.rs, kwin.rs, mutter.rs or
                               x11.rs; KDE and GNOME deliver through PipeWire (pipewire.rs,
                               pipewire_stream.c)
      stream/                  pictures to video: mod.rs paces and frames, convert.rs makes YUV,
                               encode.rs drives the encoders (encoder.c, x264.c, nvenc.c)
      audio.rs                 the desktop's sound as Opus
      clipboard/               clipboard text in both directions, on Wayland and on X11
      input.rs                 keyboard and pointer through uinput
      compositor.rs            the Wayland connection that capture and clipboard share

## Licence

x264 is licensed under the GPL, version 2 or later, and is linked into the binary, so the binary can only be
distributed under the GPL.
