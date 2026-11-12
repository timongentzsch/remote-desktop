#!/bin/sh
# install and uninstall on a system without systemd. The container runs as root and has no udev daemon, so
# sudo, modprobe and udevadm are stand-ins that only say what they were asked to do.
mkdir -p $XDG_RUNTIME_DIR /fake /etc/udev/rules.d && chmod 700 $XDG_RUNTIME_DIR
printf '#!/bin/sh\nexec "$@"\n' > /fake/sudo
for tool in modprobe udevadm; do printf '#!/bin/sh\necho "  ($(basename $0) $*)"\n' > /fake/$tool; done
chmod +x /fake/*; export PATH=/fake:$PATH
systemctl --user show-environment >/dev/null 2>&1 && echo "systemd runs user services here" || echo "no systemd for user services here"
cp ./remote-desktop /tmp/remote-desktop
printf 'a-test-password\n' | /tmp/remote-desktop install 2>&1 | head -4
sleep 1
echo "installed: $(ls /usr/local/bin/remote-desktop /etc/udev/rules.d/70-remote-desktop.rules $HOME/.config/autostart/remote-desktop.desktop $HOME/.local/share/applications/remote-desktop.desktop 2>&1 | tr '\n' ' ')"
echo "unit written: $(ls $HOME/.config/systemd/user/remote-desktop.service 2>/dev/null || echo no)"
echo "running: $(pgrep -x -f /usr/local/bin/remote-desktop | wc -l) process, session leader: $(ps -o pid=,sid= -p $(pgrep -x -f /usr/local/bin/remote-desktop) | awk '{ print ($1 == $2) ? "yes" : "no" }')"
echo "page: $(python3 -c "
import ssl, urllib.request
print(urllib.request.urlopen('https://localhost:8765/', context=ssl._create_unverified_context()).status)")"
printf 'a-test-password\n' | /usr/local/bin/remote-desktop install >/dev/null 2>&1; sleep 1
echo "after installing again: $(pgrep -x -f /usr/local/bin/remote-desktop | wc -l) process"
/usr/local/bin/remote-desktop password --none | tail -1
/usr/local/bin/remote-desktop uninstall 2>&1 | tail -1
sleep 0.5
echo "after uninstall: $(pgrep -x -f /usr/local/bin/remote-desktop | wc -l) processes; left: $(ls /usr/local/bin/remote-desktop /etc/udev/rules.d/70-remote-desktop.rules $HOME/.config/autostart/remote-desktop.desktop $HOME/.local/share/applications/remote-desktop.desktop 2>/dev/null | wc -l) files; kept: $(ls $HOME/.config/remote-desktop | tr '\n' ' ')"
