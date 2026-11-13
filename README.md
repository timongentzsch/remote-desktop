# Remote desktop

Your running Linux desktop in a browser tab. One file on the host, nothing on the other device, any desktop.

- **Nothing to install where you sit.** Open the address in Chrome, or install the page as an app from
  there. There is no client program.
- **One file on the host, for the session you are already using.** The same binary serves GNOME, KDE,
  Hyprland, Sway and X11 desktops, and sets itself up with one command. It is built for x86_64 and aarch64.
- **Complete without configuration.** The screen as H.264, the sound as Opus, the clipboard, keyboard and
  pointer, all encrypted. An NVIDIA card's hardware encoder is used where there is one (driver 470 or
  newer, one viewer at a time), the built-in x264 everywhere else.
- **Nobody in between.** The browser talks to your machine directly: no account, no relay, no vendor's
  server.
- **Made to feel local.** A still screen costs nothing and is sharpened while it stands still, a busy one is
  encoded in a few milliseconds, the pointer is drawn by the client, and the bitrate follows the connection.
  On a weak connection the picture is made smaller before it is made blurry, and full colour detail is kept
  while both ends have the time for it. Video, sound and the pointer's
  moves travel over UDP, so that a lost packet costs no wait: with one in twenty lost, frames still arrived
  on time and sound played on.
- **Yours to weigh.** One control says what gives way when the connection is not enough, response or
  quality, and takes effect at once. Frame rate, delay and bitrate can be pinned to a corner of the picture.

## What it is not

- **Not a way around routers.** The other device has to be able to reach the machine: on the same network,
  over a VPN such as Tailscale, or through a port you open. There is no relay service.
- **Not a game streamer.** It is made for desktop work and into a browser. There are no gamepads and no
  HDR. Keys and clicks travel over TCP, and so does everything else where UDP does not get through, which
  shows on a link that loses packets.
- **Not a session of its own.** It shows what the machine's own screen shows: the login screen, then the
  desktop of whoever logs in there. It starts no separate sessions for remote users. On Hyprland the size
  "Client" gives that desktop a screen of the client window's size instead, for as long as the client is
  connected, and switches the machine's own screens off meanwhile; elsewhere it shows the machine's screen.
  The sizes 1080p, 720p and 360p shrink the machine's screen to that height before it is encoded, for links
  that carry little; Auto, the first, does so only while the link holds the video back.
- **Not file transfer or device sharing.** Text goes through the clipboard; files, drives, printers and USB
  devices are not passed on.

## What it runs on

Any Linux distribution, with any desktop, on Wayland or X11. The exceptions:

- Distributions without glibc 2.28 or newer, such as Alpine with musl, and NixOS and Guix, which run no
  binaries built for other distributions as they are.
- Wayland compositors other than GNOME, KDE, Hyprland and those built on wlroots, such as Sway. (It needs
  the `ext-image-copy-capture-v1` or the `wlr-screencopy` protocol.)
- Without PipeWire or PulseAudio there is no sound.

The client needs WebCodecs, and Keyboard Lock to send system shortcuts such as Alt+Tab to the remote side,
which works in full screen. Video over UDP needs WebTransport; without it, or where the network lets no UDP
through, the video takes the same connection as everything else. It is developed and tested with Chrome.

## Install

In a terminal of the desktop session you want to control:

    curl -fsSL https://github.com/timongentzsch/remote-desktop/releases/latest/download/install.sh | sh

For a machine on a tailnet, reached only from the tailnet, and without a password:

    curl -fsSL https://github.com/timongentzsch/remote-desktop/releases/latest/download/install.sh | sh -s -- --tailscale --no-password

The script downloads the binary for this machine's architecture and runs `remote-desktop install` with the
options given:

- `--tailscale` takes the certificate from Tailscale (see [Certificates](#certificates)) and answers devices on
  the tailnet only,
- `--no-password` lets in whoever reaches the host without a password.

`install` asks for a login password, unless told `--no-password`, and for sudo, then

- copies the binary to `/usr/local/bin/remote-desktop`,
- lets the user at the machine create input devices (a udev rule for `/dev/uinput`),
- opens port 8765 if ufw or firewalld is active: TCP, and UDP for the video; with `--tailscale` on Tailscale's
  interface only,
- creates a certificate, kept with the password in `~/.config/remote-desktop/`,
- starts the host with the desktop session: as a systemd user service, or through the desktop's autostart
  where there is no systemd,
- with systemd, adds a system service that shows the login screen while nobody is logged in,

and prints the address to open. `remote-desktop uninstall` undoes all of it except the password and the
certificate.

While the machine shows its login screen, the system service shows it too, under the same address and
password. When you log in there it leaves the port to the desktop's own host, and the page, which takes up a
session that ended unasked for half a minute, connects to that one. The login screen was seen working with
SDDM. A login at a text console has no picture to show, and without systemd there is no such service: there,
let the display manager log the user in automatically and lock the session at once.

    remote-desktop install [--tailscale] [--no-password]
                                             what install.sh runs, for a binary downloaded by hand
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

**Tailscale.** On a tailnet with HTTPS certificates enabled, `install --tailscale` takes the certificate from
Tailscale instead. Browsers trust it as it is, the host is reachable from the tailnet at
`https://<machine>.<tailnet>.ts.net:8765`, and the certificate renews itself. A self-hosted Headscale issues
these certificates from version 0.30 on, with `dns.certificates` and a DNS challenge provider configured.

With this certificate the host answers devices on the tailnet only: connections from Tailscale's addresses
(`100.64.0.0/10`, `fd7a:115c:a1e0::/48`) and from the machine itself. Tailscale drops those addresses arriving
over any other interface, so nobody on the local network can pose as one, and a subnet router's forwarding
from the tailnet into the local network does not count. Who on the tailnet may reach the machine is up to the
tailnet's access rules.

Reaching the host from another network is left to Tailscale or another VPN altogether; this program has no
relay or router traversal of its own. Where Tailscale cannot connect two devices directly it relays them
through one of its servers, which is slow and shows as delay here: `tailscale ping <machine>` says which of
the two it is.

## Security

Whoever logs in controls the desktop session, so treat the address and password like an SSH login.

- Everything goes over TLS, the video over UDP included (as QUIC). The UDP port answers anyone who can reach
  it, but carries nothing without a token that only a logged-in session's page is given.
- After a wrong password no login is accepted for a second, whichever connection it came from. Use a long
  password where the port is reachable from networks you do not trust, or better, reach the host through
  Tailscale or another VPN.
- The client keeps the password in the browser's local storage after a successful login. Anyone with access
  to that browser profile can read it.
- With `password --none`, anyone who can reach port 8765 controls the desktop. Other web sites cannot do so
  through a visitor's browser: the host only accepts connections from its own page.
- The udev rule lets every program of the user at the machine create input devices, not only this one.
- The service for the login screen answers before anyone has logged in, behind the same password. It is root
  only until it has read the password and the certificate, and serves as the display manager's own user.
  `sudo systemctl disable --now remote-desktop-login` switches it off.
- The host listens on all network interfaces unless started with `--listen`. With a certificate from Tailscale
  it answers devices on the tailnet only (see [Certificates](#certificates)).

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

    install.sh                 the one-line install: fetches the binary, runs `remote-desktop install`
    index.html                 the client: page, styles and script in one file
    fonts/                     its typeface, JetBrains Mono, which the host serves along
    native/src/                what is the same on every operating system:
      main.rs                  the command line, and the names the rest shares
      server.rs                HTTPS and the client page, and who is answered
      session/                 one client's session: its login, threads and input, and its datagrams
      outbox.rs, ws.rs         what waits to be sent to a client; WebSocket framing
      quic.rs, fec.rs          the UDP side that video takes; its spare datagrams against loss
      rate.rs                  the bitrate a connection is given, from how well frames get through
      tls.rs, certificate.rs   the encrypted connection; the certificate behind it
      capture.rs, screen.rs    what a session asks of a capture; the screens and where the pointer goes
      stream/                  pictures to video: pacing, size, colour detail, the encoders, the FLV they go in
      audio.rs                 the desktop's sound as Opus
      platform/                what differs from one operating system to another, behind one surface (mod.rs)
        linux/                 Wayland and X11 capture, PipeWire and PulseAudio, uinput, the clipboard,
                               the login screen, install with systemd, ufw and firewalld, the virtual screen
    tests/                     the regression on every kind of desktop, and tools for measuring

To extend it: another operating system is a module in `platform/` that provides the names `platform/mod.rs`
lists; another way of capturing a screen on Linux is a module in `platform/linux/capture/` with one function,
`run`, that feeds a `Stream`, and another desktop's clipboard one in `platform/linux/clipboard/`; another
encoder implements `encoder_ops` in `stream/encoder.h`; and another kind of message gets its number in `ws.rs`
and a handler in the client's table of them. The client and the host share a protocol but no code: each value
they must agree on is named on both sides, with a note where the other side has it (`session/login.rs` for the
login, `ws.rs`, `stream/flv.rs` and `quic.rs` for the binary ones).

## Licence

GNU General Public License, version 2 or (at your option) any later version; see `LICENSE`. x264, which is
linked into the binary, is under the same terms. The client's typeface, JetBrains Mono, is under the SIL Open
Font License; see `fonts/OFL.txt`.
