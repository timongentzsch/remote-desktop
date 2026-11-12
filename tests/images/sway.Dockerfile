# Headless Sway with sound: a current wlroots desktop, as Hyprland is one too.
FROM archlinux:latest
RUN pacman -Syu --noconfirm sway swaybg foot grim wl-clipboard mpv pipewire pipewire-pulse wireplumber libpulse dbus python sox ffmpeg \
    && pacman -Scc --noconfirm
ENV XDG_RUNTIME_DIR=/run/user/0 HOME=/root
WORKDIR /test
