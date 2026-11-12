# Headless KDE: KWin, which hands out the screen through PipeWire. It needs a GPU device (--device /dev/dri).
FROM archlinux:latest
RUN pacman -Syu --noconfirm kwin foot wl-clipboard pipewire wireplumber dbus python ffmpeg && pacman -Scc --noconfirm
ENV XDG_RUNTIME_DIR=/run/user/0 HOME=/root
WORKDIR /test
