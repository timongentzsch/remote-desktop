# Headless X11: Xvfb, and a real X server on a dummy display for what Xvfb cannot do (several monitors).
FROM archlinux:latest
RUN pacman -Syu --noconfirm xorg-server-xvfb xorg-server xf86-video-dummy xterm xdotool xclip xorg-xrandr xorg-xsetroot python ffmpeg \
    && pacman -Scc --noconfirm
ENV XDG_RUNTIME_DIR=/run/user/0 HOME=/root DISPLAY=:1
WORKDIR /test
