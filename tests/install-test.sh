#!/bin/sh
# install and uninstall on a system without systemd. The container runs as root and has no udev daemon, so
# sudo, modprobe and udevadm are stand-ins that only say what they were asked to do.
mkdir -p $XDG_RUNTIME_DIR /fake /etc/udev/rules.d && chmod 700 $XDG_RUNTIME_DIR
printf '#!/bin/sh\nexec "$@"\n' > /fake/sudo
for tool in modprobe udevadm; do printf '#!/bin/sh\necho "  ($(basename $0) $*)"\n' > /fake/$tool; done
chmod +x /fake/*; export PATH=/fake:$PATH
systemctl --user show-environment >/dev/null 2>&1 && echo "systemd runs user services here" || echo "no systemd for user services here"
cp ./remote-desktop /tmp/remote-desktop
python3 - <<'PY' || exit 1
import os, pathlib, socket, subprocess, tempfile
with socket.socket() as other, tempfile.TemporaryDirectory() as config:
    # A different program can have the same process name as the installed host.
    pathlib.Path('/proc/self/comm').write_text('remote-desktop')
    other.bind(('127.0.0.1', 443))
    other.listen()
    password = pathlib.Path(config, 'remote-desktop', 'password')
    password.parent.mkdir()
    password.write_text('keep this password')
    result = subprocess.run(['/tmp/remote-desktop', 'install', '--no-password'],
                            env=dict(os.environ, XDG_CONFIG_HOME=config), capture_output=True, text=True)
    assert result.returncode and 'port 443 is in use by another service' in result.stderr, result
    assert password.read_text() == 'keep this password'
    assert not pathlib.Path('/usr/local/bin/remote-desktop').exists()
PY
printf 'a-test-password\n' | /tmp/remote-desktop install 2>&1 | sed -n '1,4p'
sleep 1
echo "installed: $(ls /usr/local/bin/remote-desktop /etc/udev/rules.d/70-remote-desktop.rules $HOME/.config/autostart/remote-desktop.desktop $HOME/.local/share/applications/remote-desktop.desktop 2>&1 | tr '\n' ' ')"
echo "unit written: $(ls $HOME/.config/systemd/user/remote-desktop.service 2>/dev/null || echo no)"
echo "running: $(pgrep -x -f /usr/local/bin/remote-desktop | wc -l) process, session leader: $(ps -o pid=,sid= -p $(pgrep -x -f /usr/local/bin/remote-desktop) | awk '{ print ($1 == $2) ? "yes" : "no" }')"
echo "page: $(python3 -c "
import ssl, urllib.request
print(urllib.request.urlopen('https://localhost/', context=ssl._create_unverified_context()).status)")"
/usr/local/bin/remote-desktop install >/dev/null || exit 1
sleep 1
echo "after installing again: $(pgrep -x -f /usr/local/bin/remote-desktop | wc -l) process"
# Another port, as where another service has 443: the address says it, and installing again keeps it.
/usr/local/bin/remote-desktop install --port=8765 | grep -o "Open https://[^ ]*" || exit 1
/usr/local/bin/remote-desktop install >/dev/null || exit 1
sleep 1
echo "page on port 8765: $(python3 -c "
import ssl, urllib.request
print(urllib.request.urlopen('https://localhost:8765/', context=ssl._create_unverified_context()).status)")"
# Newer versions install themselves through systemd, which is not here: the install says so and goes on.
/usr/local/bin/remote-desktop install --auto-update | grep "not installed by themselves" || exit 1
/usr/local/bin/remote-desktop password --none | tail -1
/usr/local/bin/remote-desktop uninstall 2>&1 | tail -1
sleep 0.5
echo "after uninstall: $(pgrep -x -f /usr/local/bin/remote-desktop | wc -l) processes; left: $(ls /usr/local/bin/remote-desktop /etc/udev/rules.d/70-remote-desktop.rules $HOME/.config/autostart/remote-desktop.desktop $HOME/.local/share/applications/remote-desktop.desktop 2>/dev/null | wc -l) files; kept: $(ls $HOME/.config/remote-desktop | tr '\n' ' ')"
