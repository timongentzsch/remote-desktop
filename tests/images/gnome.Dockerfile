# Headless GNOME: Mutter, which hands out the screen through PipeWire, and its X server for clipboard tools.
FROM archlinux:latest
RUN pacman -Syu --noconfirm mutter xorg-xwayland foot xclip pipewire wireplumber dbus glib2 python python-gobject ffmpeg \
    && pacman -Scc --noconfirm
ENV XDG_RUNTIME_DIR=/run/user/0 HOME=/root
WORKDIR /test
