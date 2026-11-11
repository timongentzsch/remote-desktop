# Remote desktop

Your running Linux desktop in a browser tab. One file on the host, nothing on the other device, any desktop.

- **Nothing to install where you sit.** Open the address in Chrome, or install the page as an app from
  there. There is no client program.
- **One file on the host, for the session you are already using.** The same binary serves GNOME, KDE,
  Hyprland, Sway and X11 desktops, on x86_64 and aarch64, and sets itself up with one command.
- **Complete without configuration.** The screen as H.264, the sound as Opus, the clipboard, keyboard and
  pointer, all encrypted. An NVIDIA card's hardware encoder is used where there is one (driver 470 or
  newer, one viewer at a time), the built-in x264 everywhere else.
- **Nobody in between.** The browser talks to your machine directly: no account, no relay, no vendor's
  server.
- **Made to feel local.** A still screen costs nothing, a busy one is encoded in a few milliseconds, the
  pointer is drawn by the client, and the bitrate follows the connection.

## How it compares

It is for reaching your own desktop from any computer with Chrome, with nothing to set up there. The usual
tools need a program on that computer, and each is ahead in something else:

| | On the other device | Ahead of this project in |
| --- | --- | --- |
| RDP (GNOME's and KDE's servers, xrdp) | An RDP program | GNOME's shows the login screen. Mature clients, sharing files and drives |
| VNC (wayvnc, x11vnc) | A VNC program, or noVNC set up beside the server | Clients for every system, small and long proven |
| RustDesk | Its program | Reaches machines behind a router without a VPN. File transfer, phone apps, Windows and macOS hosts |
| Sunshine with Moonlight | Moonlight | Delay and picture quality, over UDP with any vendor's hardware encoder. Gamepads, HDR |

## What it runs on

Any Linux distribution, with any desktop, on Wayland or X11. The exceptions:

- Distributions without glibc 2.28 or newer, such as Alpine with musl, and NixOS and Guix, which run no
  binaries built for other distributions as they are.
- Wayland compositors other than GNOME, KDE, Hyprland and those built on wlroots, such as Sway. (It needs
  the `ext-image-copy-capture-v1` or the `wlr-screencopy` protocol.)
- Without PipeWire or PulseAudio there is no sound.

The client needs WebCodecs, and Keyboard Lock to send system shortcuts such as Alt+Tab to the remote side,
which works in full screen. It is developed and tested with Chrome.

## Install

In a terminal of the desktop session you want to control:

    curl -fLo remote-desktop https://github.com/timongentzsch/remote-desktop/releases/latest/download/remote-desktop-$(uname -m) && chmod +x remote-desktop && ./remote-desktop install

`install` asks for a login password and for sudo, then

- copies the binary to `/usr/local/bin/remote-desktop`,
- lets the user at the machine create input devices (a udev rule for `/dev/uinput`),
- opens TCP port 8765 if ufw or firewalld is active,
- creates a certificate, kept with the password in `~/.config/remote-desktop/`,
- starts the host with the desktop session: as a systemd user service, or through the desktop's autostart
  where there is no systemd,

and prints the address to open. `remote-desktop uninstall` undoes all of it except the password and the
certificate.

The host runs inside the desktop session, so it is not there while the machine shows its login screen. For
a machine to be reachable after a restart, let its display manager log the user in automatically and lock
the session at once; the lock screen can be opened remotely.

    remote-desktop password                  change the login password
    remote-desktop password --none           allow access without a password
    remote-desktop certificate [--tailscale] make a new certificate, or switch where it comes from
    remote-desktop --listen 127.0.0.1:8765   run by hand on another address

## Certificates

Browsers only decode video and capture the keyboard on secure pages, so the host always serves HTTPS.

**Self-signed (default).** Works on any network. Each client has to trust the certificate once; on a Mac:

    scp user@host:.config/remote-desktop/certificate.pem remote-desktop.pem
    security add-trusted-cert -r trustRoot -p ssl -k ~/Library/Keychains/login.keychain-db remote-desktop.pem

It is valid for 820 days and names the machine's hostname and address as they were when it was made.

**Tailscale.** On a tailnet with HTTPS certificates enabled, `remote-desktop install --tailscale` takes the
certificate from Tailscale instead. Browsers trust it as it is, the host is reachable from the tailnet at
`https://<machine>.<tailnet>.ts.net:8765`, and the certificate renews itself.

## Security

Whoever logs in controls the desktop session, so treat the address and password like an SSH login.

- Everything goes over TLS. After a wrong password no login is accepted for a second, whichever connection
  it came from. Use a long password where the port is reachable from networks you do not trust, or better,
  reach the host through Tailscale or another VPN.
- The client keeps the password in the browser's local storage after a successful login. Anyone with access
  to that browser profile can read it.
- With `password --none`, anyone who can reach port 8765 controls the desktop. Other web sites cannot do so
  through a visitor's browser: the host only accepts connections from its own page.
- The udev rule lets every program of the user at the machine create input devices, not only this one.
- The host listens on all network interfaces unless started with `--listen`.

## Known limitations

- Only the first screen is captured.
- The pointer is drawn by the client, always as an arrow, or painted into the video by the host.
- "Sharp" colour (4:4:4) is decoded in software by Chrome.
- After a slow connection the automatic bitrate climbs back only while the picture is moving.
- The client is made for a keyboard and a mouse or trackpad, not for touch.
- On X11, clipboard texts of some hundred kilobytes are not passed on, and the host needs `DISPLAY` in the
  environment it is started from.
- It was developed on a real Hyprland session. Sway, GNOME, KDE and X11 were only run headless in test
  environments, so their login and lock screens, and input on them, are untried; so are aarch64 and GPUs
  other than an RTX 4090.

## Build

Needs Rust, a C compiler, `make` and, for x86, `nasm`. Clone with `--recurse-submodules`: x264 is built from
`native/x264`, and `native/nv-codec-headers` and `native/pipewire` provide headers for libraries that are
loaded at run time where present.

    cd native
    cargo fmt --check && cargo clippy --release --all-targets -- --deny warnings
    cargo test --release
    cargo build --release

Release binaries for glibc 2.28 and newer need `zig` and `cargo-zigbuild`; `.github/workflows/build.yml`
builds them and publishes them when a version tag is pushed.

    cargo zigbuild --release --target x86_64-unknown-linux-gnu.2.28 --target aarch64-unknown-linux-gnu.2.28

## Layout

    index.html                 the client: page, styles and script in one file
    native/src/
      main.rs, install.rs      the command line; install, uninstall, password
      server.rs, session.rs    HTTPS and the client page; one client's login, threads and input
      outbox.rs, ws.rs         what waits to be sent to a client; WebSocket framing
      tls.rs, certificate.rs   the encrypted connection; the certificate behind it
      capture/                 the screen's picture, one module per way of getting it
      stream/                  pictures to video: pacing, colour conversion, the encoders
      clipboard/               clipboard text in both directions, one module per kind of desktop
      audio.rs, input.rs       sound as Opus; keyboard and pointer through uinput
      compositor.rs, gnome.rs, xserver.rs
                               the display servers' connections that the above share

## Licence

GNU General Public License, version 2 or (at your option) any later version; see `LICENSE`. x264, which is
linked into the binary, is under the same terms.
