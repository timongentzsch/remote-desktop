# Debian 13, whose Sway (wlroots 0.18) has only the older capture and clipboard protocols.
FROM debian:13
RUN apt-get update && apt-get install -y --no-install-recommends sway foot wl-clipboard pipewire pipewire-pulse \
    wireplumber pulseaudio-utils dbus dbus-user-session python3 sox ffmpeg ca-certificates && rm -rf /var/lib/apt/lists/*
ENV XDG_RUNTIME_DIR=/run/user/0 HOME=/root
WORKDIR /test
